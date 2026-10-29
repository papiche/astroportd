//! Réception des fichiers qu'un ami envoie **depuis une autre station**.
//!
//! L'expéditeur publie pour chaque ami un message fichier (NIP-17, kind 15) emballé en NIP-59 : il porte le CID du blob
//! chiffré, la clé de fichier et le nom, rien d'autre ; le contenu ne voyage pas dans le message. Ici, pour chaque MULTIPASS
//! hébergé, la station :
//! 1. ouvre les enveloppes qui lui sont adressées (la signature du scellé prouve l'expéditeur) ;
//! 2. ne garde que les envois de **proches réciproques** (double suivi) : tout le reste est refusé sans bruit ;
//! 3. rapatrie le blob **par le maillage interne** (protocole de blobs, empreinte vérifiée — aucun kubo requis) ;
//! 4. le range sous `/Partages/De_<expéditeur>/` : même blob, même clé, aucune copie ni ré-encodage ;
//! 5. prévient le titulaire (flux personnel et message privé de BRO).
//!
//! Un blob pas encore joignable (station de l'ami hors ligne) n'est **pas** abandonné : nouvel essai avec un délai croissant
//! (1 min à 6 h), puis échec dit au titulaire au bout de 12 essais. Chaque enveloppe n'est traitée qu'une fois (`claimed`).

use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::{KeySource, nip59};
use astro_dav::adopt::{SharedFile, adopt_shared_file, sender_label};
use astro_dav::blob::is_cid;
use astro_dav::{BlobStore, ManifestPublisher};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_nostr_proto::{Event, kind};
use rusqlite::OptionalExtension;
use tokio::sync::watch;

use crate::upload_worker::Notify;

/// Espace de noms des enveloppes traitées dans `claimed`.
const CLAIM: &str = "share_inbox";
/// Espace des essais en attente dans `kv`.
const TRIES: &str = "share_inbox_tries";
/// Les enveloppes plus vieilles ne sont plus lues.
const LOOKBACK_SECS: i64 = 30 * 24 * 3600;
/// Enveloppes lues par MULTIPASS et par passage.
const BATCH: usize = 50;
/// Essais de rapatriement avant d'abandonner.
const MAX_TRIES: u32 = 12;
/// Taille maximale d'un fichier reçu (celle que le protocole de blobs accepte de servir).
const MAX_BYTES: u64 = 1 << 30;

/// Bilan d'un passage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InboxStats {
    /// Fichiers rangés.
    pub received: usize,
    /// Enveloppes écartées (pas un proche réciproque, illisible, mal formée, hors sujet).
    pub ignored: usize,
    /// Blobs pas encore joignables : nouvel essai plus tard.
    pub deferred: usize,
    /// Abandons après trop d'essais.
    pub failed: usize,
}

/// Réception des fichiers d'amis.
pub struct ShareInbox<B: BlobStore> {
    db: Arc<Db>,
    blobs: Arc<B>,
    keys: Arc<dyn KeySource>,
    publisher: Option<ManifestPublisher>,
    activity: ActivityBus,
    notify: Option<Notify>,
    prefetch: Option<Prefetch>,
}

/// Rapatrie `cid` depuis la station `peer_id` (indication de l'expéditeur) : tentative ciblée avant de demander à tous les pairs.
pub type Prefetch = Arc<dyn Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> + Send + Sync>;

fn backoff(tries: u32) -> i64 {
    (60i64 << tries.min(10)).min(6 * 3600)
}

/// Étiquettes d'un message fichier : `(cid, clé, type, sha du clair, taille, nom)`.
fn file_tags(tags: &[Vec<String>]) -> Option<SharedFile> {
    let get = |name: &str| tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).cloned();
    let (cid, key_hex) = (get("cid")?, get("decryption-key")?);
    if !is_cid(&cid) || key_hex.len() != 64 || !key_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let size: u64 = get("size")?.parse().ok()?;
    if size == 0 || size > MAX_BYTES {
        return None;
    }
    let mime = get("file-type").filter(|m| m.len() <= 100 && m.bytes().all(|b| b.is_ascii_graphic())).unwrap_or_else(|| "application/octet-stream".to_owned());
    Some(SharedFile { cid, key_hex: key_hex.to_ascii_lowercase(), mime, size, sha_plain: get("ox").filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).unwrap_or_default(), name: get("name").unwrap_or_default() })
}

impl<B: BlobStore> ShareInbox<B> {
    /// Assemble la réception. `blobs` doit rapatrier les blobs absents auprès des pairs (`P2pBlobStore`).
    pub fn new(db: Arc<Db>, blobs: Arc<B>, keys: Arc<dyn KeySource>, publisher: Option<ManifestPublisher>, activity: ActivityBus, notify: Option<Notify>) -> Self {
        Self { db, blobs, keys, publisher, activity, notify, prefetch: None }
    }

    /// Active le rapatriement ciblé vers la station de l'expéditeur (étiquette `station` du message).
    pub fn with_prefetch(mut self, f: Prefetch) -> Self {
        self.prefetch = Some(f);
        self
    }

    async fn owners(&self) -> Vec<String> {
        self.db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hex FROM identities WHERE hosted = 1 AND role IN ('multipass', 'captain')")?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    async fn claim(&self, id: &str, now: i64) -> bool {
        self.db.writer().execute("INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES (?1, ?2, ?3)", vec![CLAIM.to_owned().into(), id.to_owned().into(), now.into()]).await.is_ok_and(|r| r.changes > 0)
    }

    /// `owner` et `sender` se suivent-ils mutuellement ?
    async fn mutual(&self, owner: &str, sender: &str) -> bool {
        let (o, s) = (owner.to_owned(), sender.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let follows = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", [&o, &s], |r| r.get::<_, i64>(0)).optional()?.is_some();
                let back = c.query_row("SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.id WHERE e.kind = 3 AND e.pubkey = ?1 AND t.name = 'p' AND t.value = ?2 LIMIT 1", [&s, &o], |r| r.get::<_, i64>(0)).optional()?.is_some();
                Ok(follows && back)
            })
            .await
            .unwrap_or(false)
    }

    /// Nom sous lequel ranger les envois de `sender` : email connu, sinon nom de son profil, sinon sa clé.
    async fn label(&self, sender: &str) -> String {
        let s = sender.to_owned();
        let (email, name): (Option<String>, Option<String>) = self
            .db
            .readers()
            .read(move |c| {
                let email = c.query_row("SELECT email FROM identities WHERE hex = ?1", [&s], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
                let name = c.query_row("SELECT json_extract(content, '$.name') FROM nostr_events WHERE pubkey = ?1 AND kind = 0 ORDER BY created_at DESC LIMIT 1", [&s], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
                Ok((email, name))
            })
            .await
            .unwrap_or((None, None));
        match (email, name) {
            (Some(e), _) => sender_label(Some(&e), sender),
            (None, Some(n)) if !n.trim().is_empty() => sender_label(Some(&n.split_whitespace().collect::<Vec<_>>().join("_")), sender),
            _ => sender_label(None, sender),
        }
    }

    async fn tell(&self, owner: &str, text: String) {
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "ucloud", "share:in", text.clone(), None);
        if let Some(n) = &self.notify {
            n(owner.to_owned(), text).await;
        }
    }

    /// Un passage sur tous les MULTIPASS hébergés.
    pub async fn poll_once(&self, now: i64) -> InboxStats {
        let mut stats = InboxStats::default();
        for owner in self.owners().await {
            let Some(secret) = self.keys.secret(&owner) else { continue };
            let (o, since) = (owner.clone(), now - LOOKBACK_SECS);
            let wraps: Vec<Event> = self
                .db
                .readers()
                .read(move |c| {
                    let mut stmt = c.prepare_cached(
                        "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e \
                         JOIN nostr_tags t ON t.event_id = e.id AND t.name = 'p' AND t.value = ?1 \
                         WHERE e.kind = 1059 AND e.created_at >= ?2 \
                           AND NOT EXISTS (SELECT 1 FROM claimed c WHERE c.owner = 'share_inbox' AND c.event_id = e.id) \
                           AND NOT EXISTS (SELECT 1 FROM kv k WHERE k.scope = 'share_inbox_tries' AND k.key = e.id AND CAST(substr(k.value, instr(k.value, ':') + 1) AS INTEGER) > ?3) \
                         ORDER BY e.created_at LIMIT ?4",
                    )?;
                    let mut rows = stmt.query(rusqlite::params![o, since, now, BATCH as i64])?;
                    let mut out = Vec::new();
                    while let Some(r) = rows.next()? {
                        out.push(row_to_event(r)?);
                    }
                    Ok(out)
                })
                .await
                .unwrap_or_default();
            for wrap in wraps {
                self.handle(&owner, &secret, wrap, now, &mut stats).await;
            }
        }
        stats
    }

    async fn handle(&self, owner: &str, secret: &[u8; 32], wrap: Event, now: i64, stats: &mut InboxStats) {
        let Ok(opened) = nip59::unwrap(secret, &wrap) else {
            self.claim(&wrap.id, now).await;
            stats.ignored += 1;
            return;
        };
        let (sender, rumor) = (opened.sender, opened.rumor);
        // Hors sujet (discussion, commande…) : lue une fois, jamais rouverte.
        if rumor.kind != kind::FILE || sender == owner {
            self.claim(&wrap.id, now).await;
            stats.ignored += 1;
            return;
        }
        let Some(file) = file_tags(&rumor.tags) else {
            self.claim(&wrap.id, now).await;
            stats.ignored += 1;
            return;
        };
        if !self.mutual(owner, &sender).await {
            self.claim(&wrap.id, now).await;
            stats.ignored += 1;
            return;
        }
        let label = self.label(&sender).await;
        // Le blob : déjà ici, ou rapatrié par le maillage (empreinte vérifiée par le protocole). L'expéditeur indique sa station :
        // on la sollicite d'abord, avant de demander à tous les pairs connectés.
        let hint = rumor.tags.iter().find(|t| t.first().map(String::as_str) == Some("station")).and_then(|t| t.get(1)).cloned();
        if let (Some(peer), Some(prefetch)) = (hint, &self.prefetch) {
            if self.blobs.open(&file.cid).await.is_err() {
                let _ = prefetch(file.cid.clone(), peer).await;
            }
        }
        if let Err(e) = self.blobs.open(&file.cid).await {
            let tries = self.attempts(&wrap.id).await + 1;
            if tries >= MAX_TRIES {
                self.claim(&wrap.id, now).await;
                self.tell(owner, format!("❌ « {} » de {label} n'a pas pu être récupéré après {tries} essais ({e}). Demandez-lui de renvoyer le fichier.", safe(&file.name))).await;
                stats.failed += 1;
            } else {
                self.set_attempts(&wrap.id, tries, now + backoff(tries)).await;
                stats.deferred += 1;
            }
            return;
        }
        match adopt_shared_file(&self.db, owner, &sender, &label, &file, "friend-share").await {
            Ok(path) => {
                self.claim(&wrap.id, now).await;
                if let Some(p) = &self.publisher {
                    p.notify(owner);
                }
                self.tell(owner, format!("📎 {label} vous a envoyé « {} » : {path}", safe(&file.name))).await;
                stats.received += 1;
            }
            Err(e) => {
                // Erreur de base : on ne réclame pas, le prochain passage réessaie.
                self.activity.publish_to(Audience::Captain, "alert", "share:in", format!("fichier d'un ami non rangé : {e}"), None);
                stats.deferred += 1;
            }
        }
    }

    async fn attempts(&self, id: &str) -> u32 {
        let id = id.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'share_inbox_tries' AND key = ?1", [id], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten().and_then(|v| v.split(':').next().and_then(|n| n.parse().ok())).unwrap_or(0)
    }

    async fn set_attempts(&self, id: &str, tries: u32, next_at: i64) {
        let _ = self
            .db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![TRIES.to_owned().into(), id.to_owned().into(), format!("{tries}:{next_at}").into(), next_at.into()],
            )
            .await;
    }

    /// Boucle : un passage toutes les `every`, jusqu'à l'arrêt.
    pub async fn run(self: Arc<Self>, every: Duration, mut stop: watch::Receiver<bool>) {
        loop {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            let _ = self.poll_once(now).await;
            tokio::select! {
                () = tokio::time::sleep(every) => {}
                _ = stop.changed() => return,
            }
        }
    }
}

/// Nom court et sans retour à la ligne pour un message.
fn safe(name: &str) -> String {
    astro_dav::adopt::safe_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_crypto::StaticKeySource;
    use astro_crypto::nip59::{Rumor, gift_wrap};
    use astro_dav::blob::{BlobWriter, FsBlobStore};
    use astro_db::events::insert_event;
    use astro_nostr_proto::{UnsignedEvent, pubkey_of};
    use sha2::Digest;
    use tokio::io::AsyncWriteExt;

    use super::*;

    const ALICE: [u8; 32] = [0xA1; 32]; // destinataire, hébergée ici
    const BOB: [u8; 32] = [0xB2; 32]; // ami réciproque, hébergé ailleurs
    const EVE: [u8; 32] = [0xE5; 32]; // suit Alice sans réciprocité

    fn hex(s: &[u8; 32]) -> String {
        pubkey_of(s).unwrap()
    }

    struct Rig {
        dir: std::path::PathBuf,
        db: Arc<Db>,
        blobs: Arc<FsBlobStore>,
        inbox: ShareInbox<FsBlobStore>,
        told: Arc<Mutex<Vec<(String, String)>>>,
        bus: ActivityBus,
    }

    async fn rig(tag: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("astroportd-inbox-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@example.org', 1)", vec![hex(&ALICE).into()]).await.unwrap();
        // Alice suit Bob et Eve ; Bob la suit en retour, Eve non.
        for who in [&BOB, &EVE] {
            db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(who).into()]).await.unwrap();
        }
        let follows = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(&ALICE)]], content: String::new() }.sign(&BOB).unwrap();
        db.writer().transaction(move |tx| insert_event(tx, &follows).map(|_| ())).await.unwrap();
        let profile = UnsignedEvent { created_at: 101, kind: 0, tags: vec![], content: r#"{"name":"Bob Martin"}"#.into() }.sign(&BOB).unwrap();
        db.writer().transaction(move |tx| insert_event(tx, &profile).map(|_| ())).await.unwrap();
        let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
        let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
        let told = Arc::new(Mutex::new(Vec::new()));
        let log = told.clone();
        let notify: Notify = Arc::new(move |owner, text| {
            let log = log.clone();
            Box::pin(async move { log.lock().unwrap().push((owner, text)) })
        });
        let bus = ActivityBus::new();
        let inbox = ShareInbox::new(db.clone(), blobs.clone(), keys, None, bus.clone(), Some(notify));
        Rig { dir, db, blobs, inbox, told, bus }
    }

    async fn put_blob(blobs: &FsBlobStore, data: &[u8]) -> String {
        let mut w = blobs.begin().await.unwrap();
        w.write_all(data).await.unwrap();
        w.commit().await.unwrap().cid
    }

    async fn deliver(db: &Db, from: &[u8; 32], tags: Vec<Vec<String>>, at: u64) -> String {
        let rumor = Rumor { pubkey: hex(from), created_at: at, kind: kind::FILE, tags, content: "ipfs://x".into() };
        let wrap = gift_wrap(from, &hex(&ALICE), &rumor, at).unwrap();
        let id = wrap.id.clone();
        db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        id
    }

    fn file_tags_for(cid: &str, name: &str) -> Vec<Vec<String>> {
        [("p", hex(&ALICE)), ("file-type", "image/jpeg".into()), ("decryption-key", "ab".repeat(32)), ("cid", cid.into()), ("ox", "cd".repeat(32)), ("size", "12345".into()), ("name", name.into())].into_iter().map(|(k, v)| vec![k.to_owned(), v]).collect()
    }

    async fn entry(db: &Db, path: &str) -> Option<(String, String)> {
        let p = path.to_owned();
        db.readers().read(move |c| Ok(c.query_row("SELECT cid, origin FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", rusqlite::params![hex(&ALICE), p], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.unwrap()
    }

    #[tokio::test]
    async fn a_mutual_friends_file_is_fetched_adopted_once_and_announced_to_its_owner_only() {
        let r = rig("ok").await;
        let cid = put_blob(&r.blobs, b"contenu chiffre").await;
        deliver(&r.db, &BOB, file_tags_for(&cid, "vacances.jpg"), 1_800_000_000).await;
        let stats = r.inbox.poll_once(1_800_000_100).await;
        assert_eq!((stats.received, stats.ignored, stats.deferred), (1, 0, 0), "{stats:?}");
        // Rangé sous le nom du profil de l'ami, même blob, même clé (le trousseau a la clé de l'expéditeur).
        assert_eq!(entry(&r.db, "/Partages/De_Bob_Martin/vacances.jpg").await, Some((cid.clone(), "friend-share".to_owned())));
        let key: String = r.db.readers().read({ let cid = cid.clone(); move |c| Ok(c.query_row("SELECT key_hex FROM ucloud_keyring WHERE owner_hex = ?1 AND cid = ?2", rusqlite::params![hex(&ALICE), cid], |r| r.get(0))?) }).await.unwrap();
        assert_eq!(key, "ab".repeat(32));
        let told = r.told.lock().unwrap().clone();
        assert_eq!(told.len(), 1);
        assert!(told[0].1.contains("Bob_Martin") && told[0].1.contains("vacances.jpg"), "{told:?}");
        let mine = r.bus.recent(10).into_iter().filter(|a| a.audience == Audience::Owner(hex(&ALICE))).count();
        assert_eq!(mine, 1);
        // Une seconde passe ne refait rien.
        assert_eq!(r.inbox.poll_once(1_800_000_200).await, InboxStats::default());
        assert_eq!(r.told.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn files_from_non_reciprocal_followers_malformed_messages_and_path_tricks_are_refused_quietly() {
        let r = rig("refuse").await;
        let cid = put_blob(&r.blobs, b"x").await;
        deliver(&r.db, &EVE, file_tags_for(&cid, "intrus.jpg"), 1_800_000_000).await; // Alice la suit, elle ne suit pas Alice
        let mut bad = file_tags_for("pas-un-cid", "mauvais.jpg");
        bad.push(vec!["extra".into(), "x".into()]);
        deliver(&r.db, &BOB, bad, 1_800_000_001).await;
        let mut no_key = file_tags_for(&cid, "sans-cle.jpg");
        no_key.retain(|t| t[0] != "decryption-key");
        deliver(&r.db, &BOB, no_key, 1_800_000_002).await;
        let mut huge = file_tags_for(&cid, "enorme.bin");
        huge.iter_mut().find(|t| t[0] == "size").unwrap()[1] = (2u64 << 30).to_string();
        deliver(&r.db, &BOB, huge, 1_800_000_003).await;
        deliver(&r.db, &BOB, file_tags_for(&cid, "../../etc/passwd"), 1_800_000_004).await; // nom piégé : accepté mais assaini
        let stats = r.inbox.poll_once(1_800_000_100).await;
        assert_eq!((stats.received, stats.ignored), (1, 4), "{stats:?}");
        assert!(entry(&r.db, "/Partages/De_Bob_Martin/etcpasswd").await.is_some(), "jamais de remontée de dossier");
        assert!(entry(&r.db, "/Partages/De_Eve").await.is_none());
        let paths: Vec<String> = r.db.readers().read(|c| Ok(c.prepare("SELECT path FROM ucloud_entries WHERE type = 'file'")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(paths.len(), 1, "{paths:?}");
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn an_unreachable_blob_is_retried_with_backoff_then_adopted_when_it_arrives() {
        let r = rig("retry").await;
        let missing = astro_dav::blob::cid_raw_sha256(&sha2::Sha256::digest(b"pas encore chez nous").into());
        deliver(&r.db, &BOB, file_tags_for(&missing, "attendu.jpg"), 1_800_000_000).await;
        let stats = r.inbox.poll_once(1_800_000_100).await;
        assert_eq!((stats.received, stats.deferred), (0, 1), "{stats:?}");
        assert_eq!(r.inbox.poll_once(1_800_000_110).await, InboxStats::default(), "pas de nouvel essai avant le délai");
        assert!(r.told.lock().unwrap().is_empty(), "rien à dire tant qu'on réessaie");
        // Le blob arrive (rapatrié par le maillage) : le prochain passage après le délai le range.
        let arrived = put_blob(&r.blobs, b"pas encore chez nous").await;
        assert_eq!(arrived, missing);
        assert_eq!(r.inbox.poll_once(1_800_000_100 + 200).await.received, 1);
        assert!(entry(&r.db, "/Partages/De_Bob_Martin/attendu.jpg").await.is_some());
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn after_too_many_failures_the_owner_is_told_and_the_message_is_dropped() {
        let r = rig("giveup").await;
        let missing = astro_dav::blob::cid_raw_sha256(&sha2::Sha256::digest(b"jamais").into());
        deliver(&r.db, &BOB, file_tags_for(&missing, "perdu.jpg"), 1_800_000_000).await;
        let mut now = 1_800_000_100;
        let mut last = InboxStats::default();
        for _ in 0..MAX_TRIES {
            last = r.inbox.poll_once(now).await;
            now += 7 * 3600; // au-delà du plus long délai
        }
        assert_eq!(last.failed, 1, "{last:?}");
        let told = r.told.lock().unwrap().clone();
        assert!(told.len() == 1 && told[0].1.contains("perdu.jpg") && told[0].1.contains("renvoyer"), "{told:?}");
        assert_eq!(r.inbox.poll_once(now + 10).await, InboxStats::default(), "abandonné : plus jamais relu");
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn backoff_grows_from_a_minute_to_six_hours() {
        assert_eq!((backoff(1), backoff(2), backoff(5)), (120, 240, 1920));
        assert_eq!(backoff(11), 6 * 3600);
    }
}
