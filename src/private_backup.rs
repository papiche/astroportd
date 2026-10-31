//! Sauvegarde **privée** des fichiers non partagés d'un MULTIPASS chez un proche réciproque.
//!
//! - le propriétaire choisit un ami et active l'option ; **l'ami doit accepter**, et fixe le volume qu'il met à disposition ;
//! - ne partent que les fichiers « privés » : ni sous `/Partages/`, ni jamais envoyés (`ucloud_sharing`), ni dans une
//!   invitation, ni partagés par FaceCloud ; les contacts (`/Contacts`) en font partie ;
//! - l'ami reçoit le blob **chiffré**, sans la clé : il le range sous `/Sauvegardes/De_<nom>/<cid>`, où il compte dans son
//!   quota, et ne peut rien lire. Les clés restent dans le trousseau du propriétaire (déjà sauvegardé par son manifeste) ;
//! - sur la même station tout se fait en base ; sur une autre, par messages NIP-59 portant l'étiquette `astro-backup`
//!   (`op` : `request`, `accept`, `decline`, `revoke`, `item`), blob rapatrié par le maillage comme pour un partage.
//!
//! L'envoi se fait **une fois par nuit**, au 20h12 solaire de la station ; la réception, elle, est continue. Les fichiers
//! supprimés ou remplacés chez le propriétaire sont retirés de chez l'ami au bout de 7 jours (nettoyage de la même nuit),
//! pour qu'une suppression accidentelle reste rattrapable.
//!
//! **Restauration** (station détruite, ou déménagement) : le membre rétablit son compte sur une autre station depuis sa carte
//! secrète ([`astro_lifecycle::multipass::SecretCard`]), et tout se passe ensuite **sans aucune action de ses amis** :
//!
//! - l'envoi nocturne range chez l'ami, avec les fichiers privés, le **blob du manifeste** (VFS + trousseau, chiffré) : sans lui
//!   l'événement 30078 retrouvé sur les relais ne désignerait rien ;
//! - la nouvelle station annonce sa **station d'accueil** en republiant le DID du membre (`home_station`, répliqué par le crawl)
//!   et envoie à chaque proche un message `restore` ; la station de l'ami met à jour la station d'accueil, puis renvoie d'elle-même
//!   ce qu'elle détient : le manifeste d'abord, puis les fichiers sauvegardés et ceux que le membre lui avait partagés (`restored`) ;
//! - le manifeste rétabli, le VFS et le trousseau reviennent ; les fichiers qui manquent encore (reçus d'autres amis) sont redemandés
//!   par leur CID, qu'un ami ne rend qu'à qui le connaît et seulement s'il l'a partagé ;
//! - tant que la restauration est en cours, aucun manifeste vide n'est publié ([`astro_dav::manifest::restoring`]) ;
//! - **déménagement** : la nouvelle station écrit à l'ancienne Home un message `moved`, signé de la clé du compte (le seul message
//!   qu'un compte s'adresse à lui-même). L'ancienne station, si elle tourne encore, cesse d'héberger ce compte : il passe à
//!   « non hébergé », pointe vers la nouvelle Home, et son dossier est mis de côté dans `.moved/` (rien n'est effacé).

use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_crypto::nip59::{self, Rumor, gift_wrap};
use astro_dav::adopt::{adopt_backup_blob, sender_label};
use astro_dav::manifest::{self, Pointer};
use astro_dav::{BlobStore, ManifestPublisher};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_facecloud::Outbox;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::register::PublishFn;
use crate::share_inbox::Prefetch;
use crate::sharing::Sharing;
use crate::upload_worker::Notify;

/// Étiquette (`["t", …]`) des messages de sauvegarde.
pub const TAG: &str = "astro-backup";
const CLAIM: &str = "backup_inbox";
const TRIES: &str = "backup_inbox_tries";
/// Fichiers envoyés au plus par passage et par compte.
const PER_PASS: usize = 100;
/// Pause entre deux envois NIP-59 vers une autre station.
const SEND_PACE: std::time::Duration = std::time::Duration::from_millis(250);
const LOOKBACK_SECS: i64 = 30 * 24 * 3600;
const BATCH: usize = 50;
const MAX_TRIES: u32 = 12;
const MAX_BYTES: u64 = 1 << 30;
/// Une demande de restauration est renouvelée toutes les 6 h tant que le manifeste n'est pas rétabli.
const RESTORE_RETRY_SECS: i64 = 6 * 3600;
/// Sans manifeste retrouvé au bout de 3 jours, le compte repart à neuf : la publication de manifestes est débloquée.
const RESTORE_GIVE_UP_SECS: i64 = 3 * 24 * 3600;
/// Fichiers renvoyés à un propriétaire par passage, et CID redemandés par message.
const RESTORE_PER_PASS: usize = 40;
const RESTORE_WANT: usize = 200;
/// Fichiers qui manquent encore après le manifeste : au plus 4 relances espacées de 6 h.
const RESTORE_MISSING_ROUNDS: u32 = 4;
/// Tâches de renvoi côté ami : `kv(scope, "<ami>:<propriétaire>")`.
/// Durée de garde d'un compte déménagé, mis de côté dans `.moved/` (il contient encore ses clés) : 30 jours, puis effacé.
const MOVED_KEEP_SECS: i64 = 30 * 24 * 3600;
/// Part P3 de la clé de secours : côté propriétaire, ce qui a déjà été confié (`<owner>:<host>` → empreinte de la part).
const SHARE_SENT: &str = "backup_share_sent";
/// Côté hébergeur : la part P3 qu'un ami lui a confiée (`<host>:<owner>` → texte de la part).
const SHARE_HELD: &str = "backup_share_held";
const JOB: &str = "backup_restore_job";
/// Délai avant de retirer chez l'ami le blob d'un fichier disparu chez le propriétaire (suppression ou remplacement).
pub const GRACE_SECS: i64 = 7 * 24 * 3600;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn backoff(tries: u32) -> i64 {
    (60i64 << tries.min(9)).min(6 * 3600)
}

fn tag<'a>(tags: &'a [Vec<String>], name: &str) -> Option<&'a str> {
    tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(String::as_str)
}

/// Plus aucun CID prioritaire à renvoyer dans cette tâche ?
fn rest_is_empty(job: &Value) -> bool {
    job["priority"].as_array().is_none_or(Vec::is_empty)
}

/// `<PeerID>:<NODE_HEX>` (même règle que le projecteur du DID).
fn valid_home(h: &str) -> bool {
    h.split_once(':').is_some_and(|(peer, node)| (10..=64).contains(&peer.len()) && peer.bytes().all(|b| b.is_ascii_alphanumeric()) && is_hex64(node))
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Bilan d'un passage de réception.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackupStats {
    /// Messages de contrôle traités (demande, réponse).
    pub control: usize,
    /// Blobs rangés.
    pub stored: usize,
    /// Messages écartés (hors accord, volume dépassé, mal formés).
    pub ignored: usize,
    /// Blobs pas encore joignables : nouvel essai plus tard.
    pub deferred: usize,
}

/// Sauvegarde privée : réglages, accords et envoi.
pub struct PrivateBackup<O: Outbox, B: BlobStore> {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    outbox: Arc<O>,
    blobs: Arc<B>,
    sharing: Arc<Sharing<O>>,
    publisher: Option<ManifestPublisher>,
    station_peer: Option<String>,
    /// `<PeerID>:<NODE_HEX>` de cette station, annoncé dans le DID d'un compte rétabli ici.
    home: Option<String>,
    publish: Option<PublishFn>,
    /// Dossier des comptes (`~/.zen/game/nostr`) : un compte déménagé y est mis de côté dans `.moved/`.
    accounts_dir: Option<std::path::PathBuf>,
    activity: ActivityBus,
    notify: Option<Notify>,
    prefetch: Option<Prefetch>,
}

impl<O: Outbox, B: BlobStore> PrivateBackup<O, B> {
    /// `sharing` fournit la liste des proches réciproques ; `station_peer` est indiqué à l'hébergeur pour rapatrier les blobs.
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, outbox: Arc<O>, blobs: Arc<B>, sharing: Arc<Sharing<O>>, activity: ActivityBus) -> Self {
        Self { db, keys, outbox, blobs, sharing, publisher: None, station_peer: None, home: None, publish: None, accounts_dir: None, activity, notify: None, prefetch: None }
    }

    /// Republie le manifeste de l'hébergeur après chaque rangement, et indique la station aux amis.
    pub fn with_station(mut self, publisher: Option<ManifestPublisher>, station_peer: Option<String>) -> Self {
        self.publisher = publisher;
        self.station_peer = station_peer;
        self
    }

    /// Station d'accueil annoncée (`<PeerID>:<NODE_HEX>`) et publication des événements signés, pour la restauration d'un compte.
    pub fn with_home(mut self, home: Option<String>, publish: Option<PublishFn>) -> Self {
        self.home = home;
        self.publish = publish;
        self
    }

    /// Dossier des comptes, pour mettre de côté celui d'un membre qui a déménagé.
    pub fn with_accounts_dir(mut self, dir: Option<std::path::PathBuf>) -> Self {
        self.accounts_dir = dir;
        self
    }

    /// Prévient le titulaire (message privé de BRO).
    pub fn with_notify(mut self, notify: Option<Notify>) -> Self {
        self.notify = notify;
        self
    }

    /// Rapatriement d'un blob depuis la station du propriétaire.
    pub fn with_prefetch(mut self, f: Prefetch) -> Self {
        self.prefetch = Some(f);
        self
    }

    async fn tell(&self, who: &str, text: String) {
        self.activity.publish_to(Audience::Owner(who.to_owned()), "ucloud", "backup", text.clone(), None);
        if let Some(n) = &self.notify {
            n(who.to_owned(), text).await;
        }
    }

    async fn label_of(&self, hex: &str) -> String {
        let h = hex.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let email = c.query_row("SELECT email FROM identities WHERE hex = ?1", [&h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
                let name = c.query_row("SELECT json_extract(content, '$.name') FROM nostr_events WHERE pubkey = ?1 AND kind = 0 ORDER BY created_at DESC LIMIT 1", [&h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
                Ok(sender_label(email.as_deref().or(name.as_deref().map(str::trim).filter(|n| !n.is_empty())), &h))
            })
            .await
            .unwrap_or_else(|_| hex.chars().take(8).collect())
    }

    /// Confie la part P3 de la clé de secours de chaque compte hébergé ici aux amis qui ont accepté d'héberger sa sauvegarde
    /// (un seul message par ami, renvoyé si la part change). Une part seule ne révèle rien à l'ami qui la garde.
    pub async fn send_shares(&self, at: i64) -> usize {
        let Some(dir) = self.accounts_dir.clone() else { return 0 };
        let pacts: Vec<(String, String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT p.owner_hex, p.host_hex, i.email FROM backup_pacts p JOIN identities i ON i.hex = p.owner_hex AND i.hosted = 1 WHERE p.status = 'accepted' AND i.email IS NOT NULL")?;
                Ok(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut sent = 0;
        for (owner, host, email) in pacts {
            let Some(share) = astro_lifecycle::share::read_share(&dir.join(&email), 3) else { continue };
            let (text, key) = (share.to_text(), format!("{owner}:{host}"));
            let fingerprint = hex::encode(&<sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())[..8]);
            if self.kv_get(SHARE_SENT, &key).await.is_some_and(|(v, _)| v == fingerprint) {
                continue;
            }
            let delivered = if self.hosted_here(&host).await {
                self.kv_put(SHARE_HELD, &format!("{host}:{owner}"), &text, at).await;
                Ok(())
            } else {
                self.send_to(&owner, &host, kind::CHAT, vec![vec!["op".into(), "share".into()]], &text).await
            };
            match delivered {
                Ok(()) => {
                    self.kv_put(SHARE_SENT, &key, &fingerprint, at).await;
                    sent += 1;
                }
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("clé de secours : part non confiée à …{} ({e})", &host[..8.min(host.len())]), None);
                }
            }
        }
        sent
    }

    async fn hosted_here(&self, hex: &str) -> bool {
        let h = hex.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [h], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false)
    }

    async fn mutual(&self, owner: &str, other: &str) -> bool {
        let (o, s) = (owner.to_owned(), other.to_owned());
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

    /// Message de contrôle (kind 14) ou blob (kind 15) emballé pour `to`.
    async fn send_to(&self, from: &str, to: &str, k: u32, mut tags: Vec<Vec<String>>, content: &str) -> Result<(), String> {
        let secret = self.keys.secret(from).ok_or("clé du compte indisponible")?;
        tags.insert(0, vec!["t".into(), TAG.into()]);
        tags.insert(0, vec!["p".into(), to.to_owned()]);
        let at = u64::try_from(now()).unwrap_or(0);
        let rumor = Rumor { pubkey: from.to_owned(), created_at: at, kind: k, tags, content: content.to_owned() };
        let wrap = gift_wrap(&secret, to, &rumor, at).map_err(|_| "emballage NIP-59 impossible".to_owned())?;
        self.outbox.send(to, wrap).await.map_err(|e| e.to_string())
    }

    async fn control(&self, from: &str, to: &str, op: &str, extra: Vec<Vec<String>>) -> Result<(), String> {
        let mut tags = vec![vec!["op".to_owned(), op.to_owned()]];
        tags.extend(extra);
        self.send_to(from, to, kind::CHAT, tags, "sauvegarde privée").await
    }

    /// État pour l'écran : mon réglage, et les demandes reçues comme hébergeur.
    pub async fn status(&self, owner: &str) -> Result<Value, String> {
        let o = owner.to_owned();
        let (mine, hosting) = self
            .db
            .readers()
            .read(move |c| {
                let mine = c
                    .query_row(
                        "SELECT host_hex, status, quota_bytes, enabled, \
                                COALESCE((SELECT SUM(size) FROM backup_items b WHERE b.owner_hex = p.owner_hex AND b.host_hex = p.host_hex), 0), \
                                (SELECT COUNT(*) FROM backup_items b WHERE b.owner_hex = p.owner_hex AND b.host_hex = p.host_hex) \
                         FROM backup_pacts p WHERE owner_hex = ?1 ORDER BY enabled DESC, asked_at DESC LIMIT 1",
                        [&o],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?)),
                    )
                    .optional()?;
                let mut stmt = c.prepare(
                    "SELECT owner_hex, status, quota_bytes, asked_at, \
                            COALESCE((SELECT SUM(size) FROM backup_items b WHERE b.owner_hex = p.owner_hex AND b.host_hex = p.host_hex), 0) \
                     FROM backup_pacts p WHERE host_hex = ?1 AND status IN ('pending','accepted') ORDER BY asked_at DESC",
                )?;
                let hosting = stmt.query_map([&o], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((mine, hosting))
            })
            .await
            .map_err(|e| e.to_string())?;
        let mine = match mine.filter(|m| m.3 == 1 || m.1 == "accepted") {
            Some((host, status, quota, enabled, used, files)) => json!({ "host": host, "label": self.label_of(&host).await, "status": status, "quota_bytes": quota, "enabled": enabled == 1, "backed_up_bytes": used, "files": files }),
            None => Value::Null,
        };
        let mut hosting_json = Vec::new();
        for (who, status, quota, at, used) in hosting {
            let mut entry = json!({ "owner": who, "label": self.label_of(&who).await, "status": status, "quota_bytes": quota, "used_bytes": used, "asked_at": at });
            // « J'appelle un ami » : l'ami hébergeur montre la part P3 (texte et QR) à celui qui rétablit son compte.
            if status == "accepted" {
                if let Some((share, _)) = self.kv_get(SHARE_HELD, &format!("{owner}:{who}")).await {
                    entry["share_svg"] = astro_ui::qr::svg(&share).into();
                    entry["share"] = share.into();
                }
            }
            hosting_json.push(entry);
        }
        let o = owner.to_owned();
        let rescue = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM kv WHERE scope = ?1 AND key LIKE ?2", rusqlite::params![SHARE_SENT, format!("{o}:%")], |r| r.get::<_, i64>(0))?)).await.unwrap_or(0);
        let friends = self.sharing.friends(owner).await?.into_iter().map(|f| json!({ "hex": f.hex, "label": f.label })).collect::<Vec<_>>();
        Ok(json!({ "mine": mine, "hosting": hosting_json, "friends": friends, "rescue_friends": rescue }))
    }

    /// Active la sauvegarde chez `host` (un proche réciproque : la demande lui est envoyée) ou la suspend (`host` absent).
    pub async fn set(&self, owner: &str, enabled: bool, host: Option<&str>) -> Result<Value, String> {
        if !enabled {
            self.db.writer().execute("UPDATE backup_pacts SET enabled = 0 WHERE owner_hex = ?1", vec![owner.to_owned().into()]).await.map_err(|e| e.to_string())?;
            return self.status(owner).await;
        }
        let host = host.filter(|h| is_hex64(h)).ok_or("choisissez un proche pour héberger la sauvegarde")?.to_ascii_lowercase();
        if self.sharing.friends(owner).await?.iter().all(|f| f.hex != host) {
            return Err("cet ami n'est pas un proche réciproque (vous vous suivez mutuellement ?)".into());
        }
        let (o, h, at) = (owner.to_owned(), host.clone(), now());
        let must_ask = self
            .db
            .writer()
            .transaction(move |tx| {
                tx.execute("UPDATE backup_pacts SET enabled = 0 WHERE owner_hex = ?1", [&o])?;
                let status: Option<String> = tx.query_row("SELECT status FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2", [&o, &h], |r| r.get(0)).optional()?;
                let ask = !matches!(status.as_deref(), Some("accepted" | "pending"));
                if status.is_none() {
                    tx.execute("INSERT INTO backup_pacts(owner_hex, host_hex, status, enabled, asked_at) VALUES (?1, ?2, 'pending', 1, ?3)", rusqlite::params![o, h, at])?;
                } else if ask {
                    tx.execute("UPDATE backup_pacts SET status = 'pending', quota_bytes = 0, enabled = 1, asked_at = ?3, decided_at = NULL WHERE owner_hex = ?1 AND host_hex = ?2", rusqlite::params![o, h, at])?;
                } else {
                    tx.execute("UPDATE backup_pacts SET enabled = 1 WHERE owner_hex = ?1 AND host_hex = ?2", [&o, &h])?;
                }
                Ok(ask)
            })
            .await
            .map_err(|e| e.to_string())?;
        if must_ask {
            let label = self.label_of(owner).await;
            if self.hosted_here(&host).await {
                self.tell(&host, format!("🗄️ {label} vous demande d'héberger sa sauvegarde privée (chiffrée, illisible pour vous). Acceptez ou refusez dans Compte › Sauvegarde privée.")).await;
            } else {
                self.control(owner, &host, "request", vec![]).await?;
            }
        }
        self.status(owner).await
    }

    /// L'hébergeur `host` répond à `owner` : accepte avec un volume (`quota_bytes`, plafonné à `max_bytes`), refuse, ou
    /// révoque un accord (ses blobs sont alors supprimés et son quota libéré).
    pub async fn respond(&self, host: &str, owner: &str, accept: bool, quota_bytes: u64, max_bytes: u64) -> Result<Value, String> {
        let (h, o) = (host.to_owned(), owner.to_owned());
        let current: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT status FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2", [&o, &h], |r| r.get(0)).optional()?)).await.map_err(|e| e.to_string())?;
        let current = current.ok_or("aucune demande de cet ami")?;
        let (status, quota) = if accept {
            if quota_bytes == 0 {
                return Err("indiquez le volume que vous mettez à disposition".into());
            }
            ("accepted", quota_bytes.min(max_bytes))
        } else if current == "accepted" {
            ("revoked", 0)
        } else {
            ("declined", 0)
        };
        let (h, o, at) = (host.to_owned(), owner.to_owned(), now());
        self.db
            .writer()
            .transaction(move |tx| {
                tx.execute("UPDATE backup_pacts SET status = ?3, quota_bytes = ?4, decided_at = ?5 WHERE owner_hex = ?1 AND host_hex = ?2", rusqlite::params![o, h, status, i64::try_from(quota).unwrap_or(i64::MAX), at])?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        if status == "revoked" || status == "declined" {
            self.forget_hosted(host, owner).await;
        }
        let label = self.label_of(host).await;
        if self.hosted_here(owner).await {
            let text = if accept { format!("✅ {label} accepte d'héberger votre sauvegarde privée.") } else { format!("⛔ {label} n'héberge plus (ou refuse d'héberger) votre sauvegarde privée.") };
            self.tell(owner, text).await;
        } else {
            let op = match status {
                "accepted" => "accept",
                "revoked" => "revoke",
                _ => "decline",
            };
            self.control(host, owner, op, vec![vec!["quota".into(), quota.to_string()]]).await?;
        }
        self.status(host).await
    }

    /// Supprime chez `host` les blobs sauvegardés par `owner` (quota libéré) et oublie l'inventaire de ce couple.
    async fn forget_hosted(&self, host: &str, owner: &str) {
        self.drop_hosted(host, owner, None).await;
        // La part P3 de la clé de secours suit le sort de la sauvegarde : plus d'accord, plus de part.
        let key = format!("{host}:{owner}");
        let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SHARE_HELD.to_owned().into(), key.into()]).await;
    }

    /// Supprime chez `host` les blobs de `owner` (tous, ou seulement `only`), puis les blobs que plus aucune entrée ne référence.
    async fn drop_hosted(&self, host: &str, owner: &str, only: Option<&[String]>) -> usize {
        let (h, o, only) = (host.to_owned(), owner.to_owned(), only.map(<[String]>::to_vec));
        let cids: Vec<String> = self
            .db
            .writer()
            .transaction(move |tx| {
                let mut stmt = tx.prepare("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/%' AND json_extract(data, '$.backup_of') = ?2")?;
                let mut cids = stmt.query_map([&h, &o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                drop(stmt);
                if let Some(only) = &only {
                    cids.retain(|c| only.contains(c));
                }
                for cid in &cids {
                    tx.execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/%' AND json_extract(data, '$.backup_of') = ?2 AND cid = ?3", rusqlite::params![h, o, cid])?;
                }
                match &only {
                    None => tx.execute("DELETE FROM backup_items WHERE owner_hex = ?2 AND host_hex = ?1", [&h, &o])?,
                    Some(only) => {
                        let mut n = 0;
                        for cid in only {
                            n += tx.execute("DELETE FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", rusqlite::params![o, h, cid])?;
                        }
                        n
                    }
                };
                Ok(cids)
            })
            .await
            .unwrap_or_default();
        for cid in &cids {
            let c = cid.clone();
            let used = self.db.readers().read(move |conn| astro_dav::vfs::cid_in_use(conn, &c).map_err(|_| astro_db::DbError::Aborted)).await.unwrap_or(true);
            if !used {
                let _ = self.blobs.remove(cid).await;
            }
        }
        if !cids.is_empty() {
            if let Some(p) = &self.publisher {
                p.notify(host);
            }
        }
        cids.len()
    }

    /// Nettoyage de nuit côté propriétaire : un blob dont le fichier n'existe plus chez lui (supprimé ou remplacé) est noté
    /// « disparu » ; s'il le reste [`GRACE_SECS`], il est retiré de chez l'ami. Un fichier revenu efface la marque.
    /// Rend le nombre de blobs retirés.
    pub async fn gc_night(&self, at: i64) -> usize {
        // Le manifeste courant n'est pas une entrée du VFS : il ne doit jamais être pris pour un fichier disparu.
        const GONE: &str = "(NOT EXISTS (SELECT 1 FROM ucloud_entries e WHERE e.owner_hex = backup_items.owner_hex AND e.cid = backup_items.cid AND e.path NOT LIKE '/Sauvegardes/%') \
                            AND backup_items.cid <> COALESCE((SELECT k.value FROM kv k WHERE k.scope = 'ucloud_manifest_cid' AND k.key = backup_items.owner_hex), ''))";
        let pacts: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT p.owner_hex, p.host_hex FROM backup_pacts p JOIN identities i ON i.hex = p.owner_hex AND i.hosted = 1 WHERE p.status = 'accepted'")?;
                Ok(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut dropped = 0;
        for (owner, host) in pacts {
            let (o, h) = (owner.clone(), host.clone());
            let due: Vec<String> = self
                .db
                .writer()
                .transaction(move |tx| {
                    tx.execute(&format!("UPDATE backup_items SET missing_since = NULL WHERE owner_hex = ?1 AND host_hex = ?2 AND missing_since IS NOT NULL AND NOT {GONE}"), [&o, &h])?;
                    tx.execute(&format!("UPDATE backup_items SET missing_since = ?3 WHERE owner_hex = ?1 AND host_hex = ?2 AND missing_since IS NULL AND {GONE}"), rusqlite::params![o, h, at])?;
                    let mut stmt = tx.prepare("SELECT cid FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 AND missing_since IS NOT NULL AND missing_since <= ?3")?;
                    Ok(stmt.query_map(rusqlite::params![o, h, at - GRACE_SECS], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
                })
                .await
                .unwrap_or_default();
            if due.is_empty() {
                continue;
            }
            if self.hosted_here(&host).await {
                self.drop_hosted(&host, &owner, Some(&due)).await;
                dropped += due.len();
            } else {
                for chunk in due.chunks(100) {
                    let tags = chunk.iter().map(|c| vec!["cid".to_owned(), c.clone()]).collect();
                    if self.control(&owner, &host, "drop", tags).await.is_err() {
                        break;
                    }
                    let (o, h, ids) = (owner.clone(), host.clone(), chunk.to_vec());
                    let _ = self
                        .db
                        .writer()
                        .transaction(move |tx| {
                            for cid in &ids {
                                tx.execute("DELETE FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", rusqlite::params![o, h, cid])?;
                            }
                            Ok(())
                        })
                        .await;
                    dropped += chunk.len();
                }
            }
        }
        dropped
    }

    /// Fichiers privés de `owner` pas encore sauvegardés chez `host`.
    async fn pending_files(&self, owner: &str, host: &str, limit: usize) -> Vec<(String, u64)> {
        let (o, h) = (owner.to_owned(), host.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT DISTINCT e.cid, COALESCE(e.size, 0) FROM ucloud_entries e \
                     WHERE e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL \
                       AND e.path NOT LIKE '/Partages/%' AND e.path NOT LIKE '/Sauvegardes/%' \
                       AND NOT EXISTS (SELECT 1 FROM ucloud_sharing s WHERE s.owner_hex = e.owner_hex AND s.cid = e.cid) \
                       AND NOT EXISTS (SELECT 1 FROM face_shares f WHERE f.owner_hex = e.owner_hex AND f.cid = e.cid) \
                       AND NOT EXISTS (SELECT 1 FROM invites i WHERE i.owner_hex = e.owner_hex AND i.revoked_at IS NULL AND instr(i.items, e.cid) > 0) \
                       AND NOT EXISTS (SELECT 1 FROM backup_items b WHERE b.owner_hex = e.owner_hex AND b.host_hex = ?2 AND b.cid = e.cid) \
                     ORDER BY COALESCE(e.mtime, 0) LIMIT ?3",
                )?;
                let rows = stmt.query_map(rusqlite::params![o, h, i64::try_from(limit).unwrap_or(i64::MAX)], |r| Ok((r.get::<_, String>(0)?, u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0))))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Le blob du manifeste uCloud de `owner` (son VFS et son trousseau, chiffrés), s'il n'est pas encore chez `host`.
    /// Sans lui, un compte dont la station a brûlé retrouverait l'événement 30078 mais pas son contenu.
    async fn manifest_item(&self, owner: &str, host: &str) -> Option<(String, u64)> {
        let (o, h) = (owner.to_owned(), host.to_owned());
        let cid: String = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT k.value FROM kv k WHERE k.scope = 'ucloud_manifest_cid' AND k.key = ?1 \
                       AND NOT EXISTS (SELECT 1 FROM backup_items b WHERE b.owner_hex = ?1 AND b.host_hex = ?2 AND b.cid = k.value)",
                    [o, h],
                    |r| r.get::<_, String>(0),
                )
                .optional()?)
            })
            .await
            .ok()
            .flatten()?;
        let (_, size) = self.blobs.open(&cid).await.ok()?;
        Some((cid, size))
    }

    async fn record_item(&self, owner: &str, host: &str, cid: &str, size: u64) -> Result<(), String> {
        self.db
            .writer()
            .execute("INSERT OR IGNORE INTO backup_items(owner_hex, host_hex, cid, size, at) VALUES (?1, ?2, ?3, ?4, ?5)", vec![owner.to_owned().into(), host.to_owned().into(), cid.to_owned().into(), i64::try_from(size).unwrap_or(0).into(), now().into()])
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn used(&self, owner: &str, host: &str) -> u64 {
        let (o, h) = (owner.to_owned(), host.to_owned());
        self.db.readers().read(move |c| Ok(c.query_row("SELECT COALESCE(SUM(size), 0) FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2", [o, h], |r| r.get::<_, i64>(0))?)).await.map_or(0, |n| u64::try_from(n).unwrap_or(0))
    }

    /// Envoie ce qui reste à sauvegarder pour chaque propriétaire hébergé ici dont l'ami a accepté. Rend le nombre de blobs envoyés.
    pub async fn run_once(&self) -> usize {
        let pacts: Vec<(String, String, u64)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT p.owner_hex, p.host_hex, p.quota_bytes FROM backup_pacts p JOIN identities i ON i.hex = p.owner_hex AND i.hosted = 1 WHERE p.enabled = 1 AND p.status = 'accepted'")?;
                Ok(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0))))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut sent = 0;
        for (owner, host, quota) in pacts {
            let local = self.hosted_here(&host).await;
            let label = self.label_of(&owner).await;
            let mut used = self.used(&owner, &host).await;
            let mut stored_here = false;
            let mut todo: Vec<(String, u64)> = self.manifest_item(&owner, &host).await.into_iter().collect();
            todo.extend(self.pending_files(&owner, &host, PER_PASS).await);
            for (cid, size) in todo {
                if used.saturating_add(size) > quota {
                    continue; // trop gros pour le volume restant : les plus petits peuvent encore passer
                }
                let result = if local {
                    adopt_backup_blob(&self.db, &host, &owner, &label, &cid, size).await.map(|_| ()).map_err(|e| e.to_string())
                } else {
                    // Compromis assumé : la déduplication zéro-copie (CID = SHA-256 du blob chiffré) prévaut sur l'anonymisation
                    // de la taille, qui reste visible de l'hébergeur.
                    let mut tags = vec![vec!["cid".to_owned(), cid.clone()], vec!["size".into(), size.to_string()], vec!["op".into(), "item".into()]];
                    if let Some(peer) = &self.station_peer {
                        tags.push(vec!["station".into(), peer.clone()]);
                    }
                    let sent = self.send_to(&owner, &host, kind::FILE, tags, &format!("ipfs://{cid}")).await;
                    // Pas de rafale de gift wraps : un premier envoi de milliers de fichiers ne doit pas saturer la box ni les relais.
                    tokio::time::sleep(SEND_PACE).await;
                    sent
                };
                match result {
                    Ok(()) => {
                        if self.record_item(&owner, &host, &cid, size).await.is_ok() {
                            used += size;
                            sent += 1;
                            stored_here |= local;
                        }
                    }
                    Err(e) => {
                        self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde privée non envoyée : {e}"), None);
                        break;
                    }
                }
            }
            if stored_here {
                if let Some(p) = &self.publisher {
                    p.notify(&host);
                }
            }
        }
        sent
    }

    /// Un passage de réception sur les comptes hébergés ici.
    pub async fn poll_once(&self, at: i64) -> BackupStats {
        let mut stats = BackupStats::default();
        let owners: Vec<String> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hex FROM identities WHERE hosted = 1 AND role IN ('multipass', 'captain')")?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        for me in owners {
            let Some(secret) = self.keys.secret(&me) else { continue };
            let (m, since) = (me.clone(), at - LOOKBACK_SECS);
            let wraps: Vec<Event> = self
                .db
                .readers()
                .read(move |c| {
                    let mut stmt = c.prepare_cached(
                        "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e \
                         JOIN nostr_tags t ON t.event_id = e.id AND t.name = 'p' AND t.value = ?1 \
                         WHERE e.kind = 1059 AND e.created_at >= ?2 \
                           AND NOT EXISTS (SELECT 1 FROM claimed c WHERE c.owner = 'backup_inbox' AND c.event_id = e.id) \
                           AND NOT EXISTS (SELECT 1 FROM kv k WHERE k.scope = 'backup_inbox_tries' AND k.key = e.id AND CAST(substr(k.value, instr(k.value, ':') + 1) AS INTEGER) > ?3) \
                         ORDER BY e.created_at LIMIT ?4",
                    )?;
                    let mut rows = stmt.query(rusqlite::params![m, since, at, BATCH as i64])?;
                    let mut out = Vec::new();
                    while let Some(r) = rows.next()? {
                        out.push(row_to_event(r)?);
                    }
                    Ok(out)
                })
                .await
                .unwrap_or_default();
            for wrap in wraps {
                self.handle(&me, &secret, wrap, at, &mut stats).await;
            }
        }
        stats
    }

    async fn claim(&self, id: &str, at: i64) {
        let _ = self.db.writer().execute("INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES (?1, ?2, ?3)", vec![CLAIM.to_owned().into(), id.to_owned().into(), at.into()]).await;
    }

    async fn handle(&self, me: &str, secret: &[u8; 32], wrap: Event, at: i64, stats: &mut BackupStats) {
        let Ok(opened) = nip59::unwrap(secret, &wrap) else {
            self.claim(&wrap.id, at).await;
            return;
        };
        let (sender, rumor) = (opened.sender, opened.rumor);
        // Le seul message qu'un compte s'adresse à lui-même : « j'habite ailleurs ». La signature est la sienne, donc c'est lui.
        if sender == me && tag(&rumor.tags, "t") == Some(TAG) && tag(&rumor.tags, "op") == Some("moved") {
            self.moved_away(me, &rumor, at).await;
            self.claim(&wrap.id, at).await;
            stats.control += 1;
            return;
        }
        if sender == me || tag(&rumor.tags, "t") != Some(TAG) {
            self.claim(&wrap.id, at).await; // pas une sauvegarde : le partage entre amis s'en occupe
            return;
        }
        if !self.mutual(me, &sender).await {
            self.claim(&wrap.id, at).await;
            stats.ignored += 1;
            return;
        }
        let label = self.label_of(&sender).await;
        match tag(&rumor.tags, "op") {
            // Un ami me demande de l'héberger.
            Some("request") => {
                let (m, s, t) = (me.to_owned(), sender.clone(), at);
                let _ = self
                    .db
                    .writer()
                    .execute(
                        "INSERT INTO backup_pacts(owner_hex, host_hex, status, enabled, asked_at) VALUES (?1, ?2, 'pending', 0, ?3) \
                         ON CONFLICT(owner_hex, host_hex) DO UPDATE SET status = CASE WHEN status = 'accepted' THEN status ELSE 'pending' END, asked_at = ?3",
                        vec![s.into(), m.into(), t.into()],
                    )
                    .await;
                self.claim(&wrap.id, at).await;
                self.tell(me, format!("🗄️ {label} vous demande d'héberger sa sauvegarde privée (chiffrée, illisible pour vous). Acceptez ou refusez dans Compte › Sauvegarde privée.")).await;
                stats.control += 1;
            }
            // Mon hébergeur répond.
            Some(op @ ("accept" | "decline" | "revoke")) => {
                let quota: i64 = tag(&rumor.tags, "quota").and_then(|q| q.parse().ok()).unwrap_or(0);
                let status = match op {
                    "accept" => "accepted",
                    "revoke" => "revoked",
                    _ => "declined",
                };
                let (m, s) = (me.to_owned(), sender.clone());
                let _ = self
                    .db
                    .writer()
                    .transaction(move |tx| {
                        tx.execute("UPDATE backup_pacts SET status = ?3, quota_bytes = ?4, decided_at = ?5 WHERE owner_hex = ?1 AND host_hex = ?2", rusqlite::params![m, s, status, quota.max(0), at])?;
                        if status != "accepted" {
                            tx.execute("DELETE FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2", [&m, &s])?;
                        }
                        Ok(())
                    })
                    .await;
                self.claim(&wrap.id, at).await;
                let text = if op == "accept" { format!("✅ {label} accepte d'héberger votre sauvegarde privée.") } else { format!("⛔ {label} n'héberge plus (ou refuse d'héberger) votre sauvegarde privée.") };
                self.tell(me, text).await;
                stats.control += 1;
            }
            // Mon ami retire des blobs dont les fichiers ont disparu chez lui depuis plus d'une semaine.
            Some("drop") => {
                let cids: Vec<String> = rumor.tags.iter().filter(|t| t.first().map(String::as_str) == Some("cid")).filter_map(|t| t.get(1).cloned()).take(100).collect();
                let (m, s) = (me.to_owned(), sender.clone());
                let accepted = self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2 AND status = 'accepted'", [s, m], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false);
                if accepted {
                    self.drop_hosted(me, &sender, Some(&cids)).await;
                }
                self.claim(&wrap.id, at).await;
                stats.control += 1;
            }
            // La part P3 de la clé de secours de mon ami : je la garde, sans pouvoir rien en faire seul.
            Some("share") => {
                let accepted = {
                    let (m, s) = (me.to_owned(), sender.clone());
                    self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2 AND status = 'accepted'", [s, m], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false)
                };
                // La part doit être celle de CET ami (clé publique du message) et porter le numéro 3.
                match astro_lifecycle::share::SecretShare::parse(&rumor.content) {
                    Ok(share) if accepted && share.index == 3 && share.hex == sender => {
                        self.kv_put(SHARE_HELD, &format!("{me}:{sender}"), &share.to_text(), at).await;
                        self.tell(me, format!("🔑 {label} vous a confié une part de sa clé de secours. Seule, elle ne sert à rien : s'il perd son compte, il vous appellera et vous la lui montrerez (Compte › Sauvegarde privée).")).await;
                    }
                    _ => stats.ignored += 1,
                }
                self.claim(&wrap.id, at).await;
                stats.control += 1;
            }
            // Un blob chiffré à ranger.
            Some("item") if rumor.kind == kind::FILE => self.store_item(me, &sender, &wrap.id, &rumor, at, stats).await,
            // Mon ami, rétabli sur une autre station, demande ce que je détiens pour lui.
            Some("restore") => {
                self.accept_restore(me, &sender, &rumor, at).await;
                self.claim(&wrap.id, at).await;
                stats.control += 1;
            }
            // Mon ami me renvoie ce que je lui avais confié (ou partagé).
            Some("restored") if rumor.kind == kind::FILE => self.store_restored(me, &sender, &wrap.id, &rumor, at, stats).await,
            _ => {
                self.claim(&wrap.id, at).await;
                stats.ignored += 1;
            }
        }
    }

    /// Le blob `cid` est-il disponible ici ? Sinon le rapatrie depuis la station indiquée par l'expéditeur ; en cas d'échec,
    /// remet le message à plus tard (reculade exponentielle), ou l'écarte après [`MAX_TRIES`] essais.
    async fn ensure_blob(&self, cid: &str, rumor: &Rumor, wrap_id: &str, at: i64, stats: &mut BackupStats) -> bool {
        if self.blobs.open(cid).await.is_err() {
            if let (Some(peer), Some(prefetch)) = (tag(&rumor.tags, "station"), &self.prefetch) {
                let _ = prefetch(cid.to_owned(), peer.to_owned()).await;
            }
        }
        if self.blobs.open(cid).await.is_err() {
            let id = wrap_id.to_owned();
            let tries = self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [TRIES, id.as_str()], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten().and_then(|v| v.split(':').next().and_then(|n| n.parse::<u32>().ok())).unwrap_or(0) + 1;
            if tries >= MAX_TRIES {
                self.claim(wrap_id, at).await;
                stats.ignored += 1;
            } else {
                let _ = self
                    .db
                    .writer()
                    .execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![TRIES.to_owned().into(), wrap_id.to_owned().into(), format!("{tries}:{}", at + backoff(tries)).into(), at.into()])
                    .await;
                stats.deferred += 1;
            }
            return false;
        }
        true
    }

    async fn store_item(&self, me: &str, sender: &str, wrap_id: &str, rumor: &Rumor, at: i64, stats: &mut BackupStats) {
        let (Some(cid), Some(size)) = (tag(&rumor.tags, "cid"), tag(&rumor.tags, "size").and_then(|s| s.parse::<u64>().ok())) else {
            self.claim(wrap_id, at).await;
            stats.ignored += 1;
            return;
        };
        let (m, s) = (me.to_owned(), sender.to_owned());
        let pact: Option<(String, i64)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT status, quota_bytes FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2", [&s, &m], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.ok().flatten();
        let quota = match pact {
            Some((status, q)) if status == "accepted" => u64::try_from(q).unwrap_or(0),
            _ => 0,
        };
        if !astro_dav::blob::is_cid(cid) || size > MAX_BYTES || self.used(sender, me).await.saturating_add(size) > quota {
            self.claim(wrap_id, at).await; // hors accord ou volume dépassé : refusé, sans bruit
            stats.ignored += 1;
            return;
        }
        if !self.ensure_blob(cid, rumor, wrap_id, at, stats).await {
            return;
        }
        let label = self.label_of(sender).await;
        match adopt_backup_blob(&self.db, me, sender, &label, cid, size).await {
            Ok(_) => {
                let _ = self.record_item(sender, me, cid, size).await;
                if let Some(p) = &self.publisher {
                    p.notify(me);
                }
                self.claim(wrap_id, at).await;
                stats.stored += 1;
            }
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde d'un ami non rangée : {e}"), None);
                stats.deferred += 1;
            }
        }
    }

    // ───────────────────────────── Restauration ─────────────────────────────

    async fn kv_get(&self, scope: &'static str, key: &str) -> Option<(String, i64)> {
        let key = key.to_owned();
        self.db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT value, updated_at FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![scope, key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).optional()?))
            .await
            .ok()
            .flatten()
    }

    async fn kv_put(&self, scope: &'static str, key: &str, value: &str, at: i64) {
        let stored = self
            .db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![scope.to_owned().into(), key.to_owned().into(), value.to_owned().into(), at.into()],
            )
            .await;
        if let Err(e) = stored {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : état « {scope} » non enregistré ({e})"), None);
        }
    }

    /// Dernier événement de `owner` de ce kind (et de ce tag `d`), tel que le crawl l'a rapatrié.
    async fn latest_event(&self, owner: &str, k: u32, d: Option<&str>) -> Option<Event> {
        let (o, d) = (owner.to_owned(), d.map(str::to_owned));
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare_cached("SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e WHERE e.pubkey = ?1 AND e.kind = ?2 AND (?3 IS NULL OR e.d_tag = ?3) ORDER BY e.created_at DESC LIMIT 1")?;
                let mut rows = stmt.query(rusqlite::params![o, k, d])?;
                Ok(match rows.next()? {
                    Some(r) => Some(row_to_event(r)?),
                    None => None,
                })
            })
            .await
            .ok()
            .flatten()
    }

    /// Comptes hébergés ici dont la restauration est dans l'état `state` (`pending`, `done`).
    async fn restore_owners(&self, state: &'static str) -> Vec<String> {
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT k.key FROM kv k JOIN identities i ON i.hex = k.key AND i.hosted = 1 WHERE k.scope = 'restore' AND k.value = ?1")?;
                Ok(stmt.query_map([state], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Proches réciproques de `owner` hébergés ailleurs : ceux à qui écrire.
    async fn remote_friends(&self, owner: &str) -> Vec<String> {
        self.sharing.friends(owner).await.map(|f| f.into_iter().filter(|f| !f.hosted).map(|f| f.hex).collect()).unwrap_or_default()
    }

    /// Un passage côté propriétaire : pour chaque compte rétabli ici dont les sauvegardes ne sont pas encore revenues, annonce la
    /// station d'accueil, demande à chaque proche ce qu'il détient (renouvelé toutes les 6 h) et applique le manifeste dès que son
    /// blob est là ; puis redemande les fichiers qui manquent encore. Rend le nombre de messages envoyés.
    pub async fn restore_step(&self, at: i64) -> usize {
        let mut sent = 0;
        for owner in self.restore_owners("pending").await {
            sent += self.restore_owner(&owner, at).await;
        }
        for owner in self.restore_owners("done").await {
            // Le manifeste a pu revenir avant que le DID soit rapatrié : la station d'accueil s'annonce quand même.
            self.announce_home(&owner, at).await;
            self.announce_moved(&owner, at).await;
            sent += self.restore_missing(&owner, at).await;
        }
        sent
    }

    async fn restore_owner(&self, owner: &str, at: i64) -> usize {
        self.announce_home(owner, at).await;
        self.announce_moved(owner, at).await;
        if self.try_restore_manifest(owner, at).await {
            return 0;
        }
        let started = self.kv_get("restore", owner).await.map_or(at, |(_, t)| t);
        if at - started > RESTORE_GIVE_UP_SECS {
            self.kv_put("restore", owner, "abandoned", at).await;
            let text = "⚠️ Restauration : aucune sauvegarde de votre uCloud n'a été retrouvée chez vos proches en 3 jours. Le compte repart à neuf ; vos amis et vos contacts restent les vôtres.".to_owned();
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration abandonnée pour {} : aucun manifeste retrouvé", &owner[..8.min(owner.len())]), None);
            self.tell(owner, text).await;
            return 0;
        }
        let last = self.kv_get("restore_asked", owner).await.map_or(0, |(_, t)| t);
        if last > 0 && at - last < RESTORE_RETRY_SECS {
            return 0;
        }
        let friends = self.remote_friends(owner).await;
        if friends.is_empty() {
            return 0; // la liste d'amis n'est pas encore rapatriée par le crawl : on réessaiera au prochain passage
        }
        let mut tags = Vec::new();
        if let Some(home) = &self.home {
            tags.push(vec!["home".to_owned(), home.clone()]);
        }
        if let Some(pointer) = self.latest_event(owner, kind::APP_DATA, Some(manifest::D_TAG)).await.and_then(|e| manifest::open_pointer(&e, &*self.keys).ok()) {
            tags.push(vec!["cid".to_owned(), pointer.manifest_cid]);
        }
        tags.push(vec!["all".to_owned(), "1".to_owned()]);
        let mut sent = 0;
        for friend in &friends {
            match self.control(owner, friend, "restore", tags.clone()).await {
                Ok(()) => sent += 1,
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : demande non envoyée à …{} ({e})", &friend[..8.min(friend.len())]), None);
                }
            }
        }
        if sent > 0 {
            self.kv_put("restore_asked", owner, &sent.to_string(), at).await;
        }
        sent
    }

    /// Applique le manifeste d'origine dès que son blob est revenu. Rend `true` quand la restauration n'a plus rien à attendre.
    async fn try_restore_manifest(&self, owner: &str, at: i64) -> bool {
        if self.kv_get("restore", owner).await.is_none_or(|(state, _)| state != "pending") {
            return false;
        }
        let Some(event) = self.latest_event(owner, kind::APP_DATA, Some(manifest::D_TAG)).await else { return false };
        let Ok(pointer) = manifest::open_pointer(&event, &*self.keys) else { return false };
        if self.blobs.open(&pointer.manifest_cid).await.is_err() {
            return false;
        }
        let o = owner.to_owned();
        let files: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", [o], |r| r.get(0))?)).await.unwrap_or(0);
        if files > 0 {
            // Appliquer le manifeste remplacerait tout le VFS : on ne détruit pas ce que le membre a ajouté depuis.
            self.kv_put("restore", owner, "abandoned", at).await;
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration de …{} : des fichiers ont été ajoutés avant le manifeste, non appliqué", &owner[..8.min(owner.len())]), None);
            self.tell(owner, "⚠️ Des fichiers ont été ajoutés avant la fin de la restauration : pour ne rien perdre, le manifeste d'origine n'a pas été appliqué. Prévenez le Capitaine.".to_owned()).await;
            return true;
        }
        match manifest::restore_from_event(&self.db, &*self.blobs, &*self.keys, &event).await {
            Ok(Pointer { manifest_cid, rev, files, .. }) => {
                self.kv_put("ucloud_manifest_rev", owner, &rev.to_string(), at).await;
                self.kv_put("ucloud_manifest_cid", owner, &manifest_cid, at).await;
                self.kv_put("restore_missing", owner, "0", at).await;
                self.kv_put("restore", owner, "done", at).await;
                if let Some(p) = &self.publisher {
                    p.notify(owner);
                }
                self.tell(owner, format!("✅ Votre compte est rétabli : {files} fichier(s) retrouvé(s) dans votre uCloud. Les fichiers eux-mêmes reviennent de chez vos proches au fil des heures.")).await;
                true
            }
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration de …{} : manifeste illisible ({e})", &owner[..8.min(owner.len())]), None);
                false
            }
        }
    }

    /// Annonce la station d'accueil en republiant le DID du compte (le crawl la propage à toutes les stations, même celles dont
    /// l'ami est hors ligne) : `home_station`, tag que le projecteur lit sur le DID.
    async fn announce_home(&self, owner: &str, at: i64) {
        let (Some(home), Some(secret)) = (self.home.as_deref().filter(|h| valid_home(h)), self.keys.secret(owner)) else { return };
        if self.kv_get("restore_home", owner).await.is_some() {
            return;
        }
        let Some(did) = self.latest_event(owner, kind::DID, Some("did")).await else { return }; // pas encore rapatrié : plus tard
        // L'ancienne station d'accueil, avant que la nouvelle ne l'écrase : c'est à elle que s'adresse le message `moved`.
        let o = owner.to_owned();
        let old_home: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT home_station FROM identities WHERE hex = ?1", [o], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten();
        if let Some(old) = old_home.filter(|h| valid_home(h) && h != home) {
            self.kv_put("restore_old_home", owner, &old, at).await;
        }
        let mut doc: Value = serde_json::from_str(&did.content).unwrap_or(Value::Null);
        if let Some(meta) = doc.get_mut("metadata").and_then(Value::as_object_mut) {
            meta.insert("astroportStation".to_owned(), json!(home));
        }
        let mut tags: Vec<Vec<String>> = did.tags.iter().filter(|t| t.first().map(String::as_str) != Some("home_station")).cloned().collect();
        tags.push(vec!["home_station".to_owned(), home.to_owned()]);
        let created_at = u64::try_from(at).unwrap_or(0).max(did.created_at + 1);
        let event = match (UnsignedEvent { created_at, kind: kind::DID, tags, content: doc.to_string() }).sign(&secret) {
            Ok(e) => e,
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : DID non signé ({e})"), None);
                return;
            }
        };
        let stored = event.clone();
        let local = self
            .db
            .writer()
            .transaction(move |tx| {
                astro_db::events::insert_event(tx, &stored)?;
                astro_db::apply_statements(tx, &astro_sync::Projector::plan(&stored).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?)?;
                Ok(())
            })
            .await;
        if let Err(e) = local {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : nouvelle station d'accueil non enregistrée ({e})"), None);
            return;
        }
        if let Some(publish) = &self.publish {
            if let Err(e) = publish(event).await {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : nouvelle station d'accueil non publiée ({e})"), None);
                return;
            }
        }
        self.kv_put("restore_home", owner, home, at).await;
    }

    /// Dit à l'**ancienne** station d'accueil que le compte l'a quittée : un message `moved` que le compte s'adresse à lui-même,
    /// donc signé de sa clé, acheminé vers l'ancienne Home (encore inscrite dans la base à cet instant). Si cette station est
    /// détruite, il n'y a personne à prévenir et l'échec reste visible dans l'état ; sinon elle cesse d'héberger.
    async fn announce_moved(&self, owner: &str, at: i64) {
        let (Some((old, _)), Some(home)) = (self.kv_get("restore_old_home", owner).await, self.home.clone()) else { return };
        if self.kv_get("restore_moved", owner).await.is_some_and(|(state, t)| state == "sent" || at - t < RESTORE_RETRY_SECS) {
            return;
        }
        let state = match self.control(owner, owner, "moved", vec![vec!["home".to_owned(), home]]).await {
            Ok(()) => "sent".to_owned(),
            Err(e) => format!("injoignable ({old}) : {e}"), // station en panne, ou hors ligne : nouvel essai dans 6 h
        };
        self.kv_put("restore_moved", owner, &state, at).await;
    }

    /// Côté ancienne station : le compte `me` a écrit, de sa propre clé, qu'il habite désormais ailleurs. Il cesse d'être hébergé
    /// ici (plus de connexion, de sauvegarde ni de crawl pour lui) et son dossier est mis de côté, jamais effacé.
    async fn moved_away(&self, me: &str, rumor: &Rumor, at: i64) {
        let Some(home) = tag(&rumor.tags, "home").filter(|h| valid_home(h)) else { return };
        if self.home.as_deref() == Some(home) {
            return; // c'est cette station : rien à quitter
        }
        // Rejeu d'un ancien message après un nouveau rétablissement ici : seul un message postérieur compte.
        if self.kv_get("restore", me).await.is_some_and(|(_, since)| i64::try_from(rumor.created_at).unwrap_or(0) <= since) {
            return;
        }
        let m = me.to_owned();
        let row: Option<(String, Option<String>)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT role, email FROM identities WHERE hex = ?1 AND hosted = 1", [m], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.ok().flatten();
        let Some((role, email)) = row else { return };
        if role != "multipass" {
            // Le Capitaine tient la station : un message ne le débarque pas, le Capitaine tranche.
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("{role} …{} annonce un déménagement vers {home} : non appliqué, à traiter à la main", &me[..8.min(me.len())]), None);
            return;
        }
        let (m, h) = (me.to_owned(), home.to_owned());
        if let Err(e) = self.db.writer().execute("UPDATE identities SET hosted = 0, home_station = ?2 WHERE hex = ?1 AND hosted = 1", vec![m.into(), h.into()]).await {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("déménagement de …{} non enregistré ({e})", &me[..8.min(me.len())]), None);
            return;
        }
        self.kv_put("moved", me, home, at).await;
        let label = email.clone().unwrap_or_else(|| me.chars().take(8).collect());
        let mut aside = String::new();
        if let (Some(dir), Some(email)) = (self.accounts_dir.clone(), email) {
            let target = dir.join(".moved").join(format!("{email}-{at}"));
            let from = dir.join(&email);
            let moved = tokio::task::spawn_blocking(move || std::fs::create_dir_all(target.parent().unwrap_or(&target)).and_then(|()| std::fs::rename(&from, &target)).map(|()| target)).await;
            match moved {
                Ok(Ok(target)) => aside = format!(" ; dossier mis de côté dans {}", target.display()),
                Ok(Err(e)) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("déménagement de {label} : dossier du compte non déplacé ({e}), à ranger à la main"), None);
                }
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("déménagement de {label} : dossier du compte non déplacé ({e})"), None);
                }
            }
        }
        self.activity.publish_to(Audience::Captain, "system", "backup", format!("{label} a déménagé vers {home} : cette station n'héberge plus son compte{aside}"), None);
    }

    /// Après le manifeste : redemande par leur CID les fichiers dont le blob manque encore (ceux que des amis ont partagés avec le
    /// membre, jamais sauvegardés chez un hôte). Un ami ne rend que ce qu'il a lui-même partagé.
    async fn restore_missing(&self, owner: &str, at: i64) -> usize {
        let Some((rounds, last)) = self.kv_get("restore_missing", owner).await else { return 0 };
        let rounds: u32 = rounds.parse().unwrap_or(RESTORE_MISSING_ROUNDS);
        if rounds >= RESTORE_MISSING_ROUNDS || at - last < RESTORE_RETRY_SECS {
            return 0;
        }
        let o = owner.to_owned();
        let candidates: Vec<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT DISTINCT cid FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL ORDER BY cid LIMIT 2000")?;
                Ok(stmt.query_map([o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut missing = Vec::new();
        for cid in candidates {
            if self.blobs.open(&cid).await.is_err() {
                missing.push(cid);
                if missing.len() >= RESTORE_WANT {
                    break;
                }
            }
        }
        if missing.is_empty() {
            self.kv_put("restore_missing", owner, &RESTORE_MISSING_ROUNDS.to_string(), at).await;
            return 0;
        }
        let tags: Vec<Vec<String>> = missing.iter().map(|c| vec!["cid".to_owned(), c.clone()]).collect();
        let mut sent = 0;
        for friend in self.remote_friends(owner).await {
            if self.control(owner, &friend, "restore", tags.clone()).await.is_ok() {
                sent += 1;
            }
        }
        self.kv_put("restore_missing", owner, &(rounds + 1).to_string(), at).await;
        sent
    }

    /// Côté ami : `owner` rétabli sur une autre station demande ce que je détiens pour lui. Aucune action de ma part : j'enregistre
    /// sa nouvelle station d'accueil (jamais pour un compte hébergé ici) et je programme le renvoi.
    async fn accept_restore(&self, me: &str, owner: &str, rumor: &Rumor, at: i64) {
        if let Some(home) = tag(&rumor.tags, "home").filter(|h| valid_home(h)) {
            let (o, h) = (owner.to_owned(), home.to_owned());
            let updated = self
                .db
                .writer()
                .execute(
                    "INSERT INTO identities(hex, role, home_station, hosted) VALUES (?1, 'multipass', ?2, 0) ON CONFLICT(hex) DO UPDATE SET home_station = excluded.home_station WHERE hosted = 0",
                    vec![o.into(), h.into()],
                )
                .await;
            if let Err(e) = updated {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : station d'accueil de …{} non enregistrée ({e})", &owner[..8.min(owner.len())]), None);
            }
        }
        if self.hosted_here(owner).await {
            return; // même station : rien à renvoyer, tout est déjà ici
        }
        let all = tag(&rumor.tags, "all") == Some("1");
        let mut priority = Vec::new();
        for cid in rumor.tags.iter().filter(|t| t.first().map(String::as_str) == Some("cid")).filter_map(|t| t.get(1)).take(RESTORE_WANT) {
            if astro_dav::blob::is_cid(cid) && self.held(me, owner, cid).await.is_some() {
                priority.push(cid.clone());
            }
        }
        if !all && priority.is_empty() {
            return;
        }
        let key = format!("{me}:{owner}");
        let mut job = match self.kv_get(JOB, &key).await.and_then(|(v, _)| serde_json::from_str::<Value>(&v).ok()) {
            // Demande renouvelée alors qu'un renvoi est en cours : on le complète sans le recommencer.
            Some(mut old) => {
                let mut merged: Vec<String> = old["priority"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                let fresh: Vec<String> = priority.iter().filter(|c| !merged.contains(c)).cloned().collect();
                merged.extend(fresh);
                old["priority"] = json!(merged);
                old["all"] = json!(all || old["all"].as_bool().unwrap_or(false));
                old
            }
            None => json!({ "priority": priority, "all": all, "cursor": "" }),
        };
        job["asked_at"] = json!(at);
        self.kv_put(JOB, &key, &job.to_string(), at).await;
    }

    /// Ce que je détiens pour `owner` sous ce CID, et pourquoi je peux le lui rendre : une sauvegarde qu'il m'a confiée, un fichier
    /// qu'il m'a partagé, ou un fichier que j'ai moi-même partagé (connaître son CID en est la preuve). Le blob doit être là.
    async fn held(&self, host: &str, owner: &str, cid: &str) -> Option<(u64, &'static str)> {
        let (h, o, c) = (host.to_owned(), owner.to_owned(), cid.to_owned());
        let found = self
            .db
            .readers()
            .read(move |conn| {
                let backup = conn.query_row("SELECT size FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", [&o, &h, &c], |r| r.get::<_, i64>(0)).optional()?;
                if let Some(size) = backup {
                    return Ok(Some((size, "backup")));
                }
                let shared = conn
                    .query_row(
                        "SELECT COALESCE(e.size, 0) FROM ucloud_entries e WHERE e.owner_hex = ?2 AND e.cid = ?3 AND e.type = 'file' AND e.path NOT LIKE '/Sauvegardes/%' \
                           AND (json_extract(e.data, '$.shared_by') = ?1 OR EXISTS (SELECT 1 FROM ucloud_sharing s WHERE s.owner_hex = e.owner_hex AND s.cid = e.cid)) LIMIT 1",
                        [&o, &h, &c],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?;
                Ok(shared.map(|size| (size, "share")))
            })
            .await
            .ok()
            .flatten()?;
        self.blobs.open(cid).await.ok()?;
        Some((u64::try_from(found.0).unwrap_or(0), found.1))
    }

    async fn send_restored(&self, host: &str, owner: &str, cid: &str, size: u64, quota: i64, src: &str) -> Result<(), String> {
        let mut tags = vec![vec!["cid".to_owned(), cid.to_owned()], vec!["size".into(), size.to_string()], vec!["op".into(), "restored".into()], vec!["src".into(), src.to_owned()], vec!["quota".into(), quota.to_string()]];
        if let Some(peer) = &self.station_peer {
            tags.push(vec!["station".into(), peer.clone()]);
        }
        let sent = self.send_to(host, owner, kind::FILE, tags, &format!("ipfs://{cid}")).await;
        tokio::time::sleep(SEND_PACE).await; // pas de rafale de gift wraps
        sent
    }

    /// Côté ami : avance les renvois programmés par [`Self::accept_restore`], par petits lots. Rend le nombre de blobs envoyés.
    pub async fn serve_restores(&self) -> usize {
        let jobs: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT key, value FROM kv WHERE scope = ?1")?;
                Ok(stmt.query_map([JOB], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut sent = 0;
        for (key, value) in jobs {
            let Some((host, owner)) = key.split_once(':') else { continue };
            let Ok(mut job) = serde_json::from_str::<Value>(&value) else {
                let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![JOB.to_owned().into(), key.clone().into()]).await;
                continue;
            };
            let (h, o) = (host.to_owned(), owner.to_owned());
            let quota: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT quota_bytes FROM backup_pacts WHERE owner_hex = ?2 AND host_hex = ?1 AND status = 'accepted'", [h, o], |r| r.get(0)).optional()?)).await.ok().flatten().unwrap_or(0);
            let all = job["all"].as_bool().unwrap_or(false);
            let mut priority: Vec<String> = job["priority"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            let later = priority.split_off(RESTORE_PER_PASS.min(priority.len()));
            // CID prioritaires déjà renvoyés : le lot « tout le reste » ne les renvoie pas une seconde fois.
            let mut skip: Vec<String> = job["skip"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            let mut failed = false;
            for (n, cid) in priority.iter().enumerate() {
                let Some((size, src)) = self.held(host, owner, cid).await else { continue };
                if let Err(e) = self.send_restored(host, owner, cid, size, quota, src).await {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration d'un ami : envoi impossible ({e})"), None);
                    priority.drain(..n); // reprend au CID qui a échoué
                    failed = true;
                    break;
                }
                skip.push(cid.clone());
                sent += 1;
            }
            let mut rest = if failed { priority } else { Vec::new() };
            rest.extend(later);
            job["priority"] = json!(rest);
            job["skip"] = json!(skip);
            if !failed && rest_is_empty(&job) && all {
                let cursor = job["cursor"].as_str().unwrap_or_default().to_owned();
                let (h, o) = (host.to_owned(), owner.to_owned());
                let batch: Vec<(String, i64, String)> = self
                    .db
                    .readers()
                    .read(move |c| {
                        let mut stmt = c.prepare(
                            "SELECT cid, size, src FROM (\
                               SELECT cid, size, 'backup' AS src FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 \
                               UNION \
                               SELECT cid, COALESCE(size, 0), 'share' FROM ucloud_entries WHERE owner_hex = ?2 AND type = 'file' AND cid IS NOT NULL AND path NOT LIKE '/Sauvegardes/%' AND json_extract(data, '$.shared_by') = ?1\
                             ) WHERE cid > ?3 ORDER BY cid LIMIT ?4",
                        )?;
                        Ok(stmt.query_map(rusqlite::params![o, h, cursor, RESTORE_PER_PASS as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>, _>>()?)
                    })
                    .await
                    .unwrap_or_default();
                if batch.is_empty() {
                    let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![JOB.to_owned().into(), key.clone().into()]).await;
                    continue;
                }
                for (cid, size, src) in batch {
                    if !skip.contains(&cid) && self.blobs.open(&cid).await.is_ok() {
                        let src: &str = if src == "backup" { "backup" } else { "share" };
                        if let Err(e) = self.send_restored(host, owner, &cid, u64::try_from(size).unwrap_or(0), quota, src).await {
                            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration d'un ami : envoi impossible ({e})"), None);
                            break;
                        }
                        sent += 1;
                    }
                    job["cursor"] = json!(cid);
                }
            } else if !failed && rest_is_empty(&job) {
                let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![JOB.to_owned().into(), key.clone().into()]).await;
                continue;
            }
            self.kv_put(JOB, &key, &job.to_string(), now()).await;
        }
        sent
    }

    /// Côté propriétaire : un blob que mon ami me renvoie. Seul un compte en cours de restauration (ou récemment rétabli) l'accepte.
    async fn store_restored(&self, me: &str, sender: &str, wrap_id: &str, rumor: &Rumor, at: i64, stats: &mut BackupStats) {
        let (Some(cid), Some(size)) = (tag(&rumor.tags, "cid"), tag(&rumor.tags, "size").and_then(|s| s.parse::<u64>().ok())) else {
            self.claim(wrap_id, at).await;
            stats.ignored += 1;
            return;
        };
        if self.kv_get("restore", me).await.is_none() || !astro_dav::blob::is_cid(cid) || size > MAX_BYTES {
            self.claim(wrap_id, at).await; // aucune restauration demandée, ou message mal formé : écarté sans bruit
            stats.ignored += 1;
            return;
        }
        if !self.ensure_blob(cid, rumor, wrap_id, at, stats).await {
            return;
        }
        let quota: i64 = tag(&rumor.tags, "quota").and_then(|q| q.parse().ok()).unwrap_or(0);
        if tag(&rumor.tags, "src") == Some("backup") && quota > 0 {
            // L'ami héberge ma sauvegarde : l'accord est rétabli sans qu'il ait rien à faire, et ce blob n'a pas à repartir.
            let _ = self.record_item(me, sender, cid, size).await;
            let (m, s) = (me.to_owned(), sender.to_owned());
            let pact = self
                .db
                .writer()
                .execute(
                    "INSERT INTO backup_pacts(owner_hex, host_hex, status, quota_bytes, enabled, asked_at, decided_at) \
                     VALUES (?1, ?2, 'accepted', ?3, NOT EXISTS (SELECT 1 FROM backup_pacts WHERE owner_hex = ?1 AND enabled = 1), ?4, ?4) \
                     ON CONFLICT(owner_hex, host_hex) DO UPDATE SET status = 'accepted', quota_bytes = excluded.quota_bytes, decided_at = excluded.decided_at",
                    vec![m.into(), s.into(), quota.into(), at.into()],
                )
                .await;
            if let Err(e) = pact {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : accord de sauvegarde non rétabli ({e})"), None);
            }
        }
        self.claim(wrap_id, at).await;
        stats.stored += 1;
        self.try_restore_manifest(me, at).await;
    }

    /// Étape de la nuit (20h12 solaire) : envoie tout ce qui reste à sauvegarder, par lots, jusqu'à épuisement.
    pub async fn sync_night(&self) -> Result<String, String> {
        let mut total = 0;
        for _ in 0..200 {
            let sent = self.run_once().await;
            if sent == 0 {
                break;
            }
            total += sent;
        }
        let dropped = self.gc_night(now()).await;
        let moved = self.purge_moved(now()).await?;
        Ok(format!("{total} fichier(s) privé(s) sauvegardé(s) chez des amis, {dropped} ancien(s) blob(s) retiré(s) ; {moved} compte(s) déménagé(s) effacé(s) de `.moved`"))
    }

    /// Efface de `.moved/` les comptes déménagés depuis plus de [`MOVED_KEEP_SECS`] (l'âge est lu dans le nom du dossier,
    /// `<email>-<horodatage>` : le déplacement conserve la date de modification d'origine). Une erreur est dite, jamais avalée.
    async fn purge_moved(&self, now: i64) -> Result<usize, String> {
        let Some(dir) = self.accounts_dir.clone().map(|d| d.join(".moved")) else { return Ok(0) };
        tokio::task::spawn_blocking(move || {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
                Err(e) => return Err(format!("lecture de {} : {e}", dir.display())),
            };
            let mut gone = 0;
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(at) = name.rsplit_once('-').and_then(|(_, ts)| ts.parse::<i64>().ok()) else { continue };
                if now.saturating_sub(at) >= MOVED_KEEP_SECS {
                    std::fs::remove_dir_all(entry.path()).map_err(|e| format!("effacement de {} : {e}", entry.path().display()))?;
                    gone += 1;
                }
            }
            Ok(gone)
        })
        .await
        .map_err(|e| format!("effacement de .moved : {e}"))?
    }

    /// Travailleur de fond : réception des demandes, réponses et blobs jusqu'au signal d'arrêt. L'**envoi** n'a lieu
    /// qu'à 20h12 ([`Self::sync_night`]) : un lot par nuit plutôt qu'un envoi à chaque modification.
    pub async fn run(self: Arc<Self>, every: Duration, mut stop: watch::Receiver<bool>) {
        loop {
            let at = now();
            self.poll_once(at).await;
            self.restore_step(at).await;
            self.serve_restores().await;
            self.send_shares(at).await;
            tokio::select! {
                () = tokio::time::sleep(every) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_crypto::StaticKeySource;
    use astro_dav::FsBlobStore;
    use astro_db::events::insert_event;
    use astro_facecloud::OutboxError;
    use astro_nostr_proto::{UnsignedEvent, pubkey_of};

    use super::*;

    const ALICE: [u8; 32] = [0xA1; 32]; // propriétaire, hébergée ici
    const BOB: [u8; 32] = [0xB2; 32]; // ami hébergé ici
    const CAROL: [u8; 32] = [0xC3; 32]; // amie hébergée ailleurs

    fn hex(s: &[u8; 32]) -> String {
        pubkey_of(s).unwrap()
    }

    #[derive(Default)]
    struct Capture(Mutex<Vec<(String, Event)>>);

    impl Outbox for Capture {
        async fn send(&self, to: &str, wrap: Event) -> Result<(), OutboxError> {
            self.0.lock().unwrap().push((to.to_owned(), wrap));
            Ok(())
        }
    }

    fn cid(n: u8) -> String {
        format!("bafkrei{}", format!("{n:02}abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopq").chars().take(52).collect::<String>())
    }

    async fn rig(tag: &str) -> (std::path::PathBuf, Arc<Db>, Arc<Capture>, PrivateBackup<Capture, FsBlobStore>) {
        let dir = std::env::temp_dir().join(format!("astroportd-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        for (s, email, hosted) in [(&ALICE, "alice@example.org", 1), (&BOB, "bob@example.org", 1), (&CAROL, "carol@example.org", 0)] {
            db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, ?3)", vec![hex(s).into(), email.to_owned().into(), i64::from(hosted).into()]).await.unwrap();
        }
        for who in [&BOB, &CAROL] {
            db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(who).into()]).await.unwrap();
            let ev = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(&ALICE)]], content: String::new() }.sign(who).unwrap();
            db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
        }
        // Alice : trois fichiers privés de 10 octets, un déjà envoyé à un ami, un dans /Partages.
        for (n, path) in [(1u8, "/Docs/a.txt"), (2, "/Docs/b.txt"), (3, "/Docs/c.txt"), (4, "/Docs/shared.txt"), (5, "/Partages/De_bob/x.txt")] {
            db.writer()
                .execute(
                    "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, ?3, 'file', ?4, 'text/plain', 10, ?5, '{}')",
                    vec![hex(&ALICE).into(), path.to_owned().into(), path.rsplit_once('/').unwrap().0.to_owned().into(), cid(n).into(), i64::from(n).into()],
                )
                .await
                .unwrap();
        }
        astro_dav::adopt::mark_shared(&db, &hex(&ALICE), vec![cid(4)], "link").await;
        let outbox = Arc::new(Capture::default());
        let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BOB));
        let sharing = Arc::new(Sharing::new(db.clone(), keys.clone(), outbox.clone(), None, None));
        let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
        let backup = PrivateBackup::new(db.clone(), keys, outbox.clone(), blobs, sharing, ActivityBus::new()).with_station(None, Some("12D3KooWAlice".into()));
        (dir, db, outbox, backup)
    }

    async fn count(db: &Db, sql: &'static str, arg: String) -> i64 {
        db.readers().read(move |c| Ok(c.query_row(sql, [arg], |r| r.get(0))?)).await.unwrap()
    }

    #[tokio::test]
    async fn nothing_leaves_before_the_friend_accepts_then_only_unshared_files_within_his_quota() {
        let (dir, db, _, backup) = rig("local").await;
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let st = backup.set(&alice, true, Some(&bob)).await.unwrap();
        assert_eq!(st["mine"]["status"], "pending");
        assert_eq!(backup.run_once().await, 0, "pas d'accord, pas d'envoi");
        assert_eq!(backup.status(&bob).await.unwrap()["hosting"][0]["status"], "pending");

        // Bob accepte 15 octets : un seul fichier de 10 octets entre, ni le partagé ni /Partages.
        backup.respond(&bob, &alice, true, 15, 100).await.unwrap();
        assert_eq!(backup.run_once().await, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/De_alice/%'", bob.clone()).await, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM ucloud_keyring WHERE owner_hex = ?1", bob.clone()).await, 0, "l'ami n'a jamais la clé");
        assert_eq!(backup.run_once().await, 0, "volume plein : rien de plus");

        // Il augmente le volume : le reste des fichiers privés part, jamais le fichier partagé.
        backup.respond(&bob, &alice, true, 1000, 1000).await.unwrap();
        assert_eq!(backup.run_once().await, 2);
        let stored: Vec<String> = db.readers().read(move |c| Ok(c.prepare("SELECT cid FROM backup_items ORDER BY cid")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert!(!stored.contains(&cid(4)) && !stored.contains(&cid(5)), "{stored:?}");

        // Il révoque : ses blobs disparaissent et son quota est libéré.
        backup.respond(&bob, &alice, false, 0, 0).await.unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/%' AND type = 'file'", bob).await, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_deleted_file_is_removed_from_the_friend_only_after_seven_days() {
        let (dir, db, _, backup) = rig("gc").await;
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        backup.set(&alice, true, Some(&bob)).await.unwrap();
        backup.respond(&bob, &alice, true, 1000, 1000).await.unwrap();
        assert_eq!(backup.run_once().await, 3);
        let hosted = |db: Arc<Db>, bob: String| async move { count(&db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/%' AND type = 'file'", bob).await };

        // Alice supprime /Docs/a.txt (cid 1) : marqué, pas encore retiré.
        db.writer().execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/Docs/a.txt'", vec![alice.clone().into()]).await.unwrap();
        let t0 = 1_000_000;
        assert_eq!(backup.gc_night(t0).await, 0);
        assert_eq!(backup.gc_night(t0 + GRACE_SECS - 1).await, 0, "avant 7 jours : rien");
        assert_eq!(hosted(db.clone(), bob.clone()).await, 3);

        // Elle le restaure avant la date : la marque s'efface, et le délai repart de zéro.
        db.writer()
            .execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, '/Docs/a.txt', '/Docs', 'file', ?2, 'text/plain', 10, 1, '{}')", vec![alice.clone().into(), cid(1).into()])
            .await
            .unwrap();
        assert_eq!(backup.gc_night(t0 + GRACE_SECS).await, 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1 AND missing_since IS NOT NULL", alice.clone()).await, 0);

        // Supprimé pour de bon : retiré après 7 jours, le quota de l'ami est libéré.
        db.writer().execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/Docs/a.txt'", vec![alice.clone().into()]).await.unwrap();
        let t1 = t0 + 2 * GRACE_SECS;
        assert_eq!(backup.gc_night(t1).await, 0);
        assert_eq!(backup.gc_night(t1 + GRACE_SECS).await, 1);
        assert_eq!(hosted(db.clone(), bob.clone()).await, 2);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1", alice).await, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    async fn count2(db: &Db, sql: &'static str, a: String, b: String) -> i64 {
        db.readers().read(move |c| Ok(c.query_row(sql, [a, b], |r| r.get(0))?)).await.unwrap()
    }

    async fn put_blob(backup: &PrivateBackup<Capture, FsBlobStore>, bytes: &[u8]) -> String {
        use astro_dav::BlobWriter as _;
        use tokio::io::AsyncWriteExt as _;
        let mut w = backup.blobs.begin().await.unwrap();
        w.write_all(bytes).await.unwrap();
        w.commit().await.unwrap().cid
    }

    async fn set_manifest(db: &Db, owner: &str, cid: &str) {
        db.writer()
            .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('ucloud_manifest_cid', ?1, ?2, 0) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value", vec![owner.to_owned().into(), cid.to_owned().into()])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_manifest_blob_goes_to_the_friend_and_outlives_the_night_cleanup_until_replaced() {
        const HAS: &str = "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1 AND cid = ?2";
        let (dir, db, _, backup) = rig("manifest").await;
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let m1 = put_blob(&backup, b"manifest-v1").await;
        set_manifest(&db, &alice, &m1).await;
        backup.set(&alice, true, Some(&bob)).await.unwrap();
        backup.respond(&bob, &alice, true, 1000, 1000).await.unwrap();

        assert_eq!(backup.run_once().await, 4, "le manifeste et les trois fichiers privés");
        assert_eq!(count2(&db, HAS, alice.clone(), m1.clone()).await, 1);
        assert_eq!(backup.run_once().await, 0, "rien à renvoyer tant que le manifeste ne change pas");

        // Le manifeste n'est pas une entrée du VFS : le nettoyage de nuit ne doit pas le prendre pour un fichier disparu.
        let t0 = 1_000_000;
        assert_eq!(backup.gc_night(t0).await, 0);
        assert_eq!(backup.gc_night(t0 + 2 * GRACE_SECS).await, 0);
        assert_eq!(count2(&db, HAS, alice.clone(), m1.clone()).await, 1, "le manifeste courant reste chez l'ami");

        // Un nouveau manifeste part tout de suite ; l'ancien est retiré après 7 jours, le courant reste.
        let m2 = put_blob(&backup, b"manifest-v2-plus-long").await;
        set_manifest(&db, &alice, &m2).await;
        assert_eq!(backup.run_once().await, 1);
        assert_eq!(count2(&db, HAS, alice.clone(), m2.clone()).await, 1);
        let t1 = t0 + 4 * GRACE_SECS;
        assert_eq!(backup.gc_night(t1).await, 0);
        assert_eq!(backup.gc_night(t1 + GRACE_SECS).await, 1);
        assert_eq!(count2(&db, HAS, alice.clone(), m1).await, 0);
        assert_eq!(count2(&db, HAS, alice, m2).await, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_remote_friend_gets_a_request_then_sealed_blobs_without_the_file_key() {
        let (dir, db, outbox, backup) = rig("remote").await;
        let (alice, carol) = (hex(&ALICE), hex(&CAROL));
        backup.set(&alice, true, Some(&carol)).await.unwrap();
        let open = |wrap: &Event| nip59::unwrap(&CAROL, wrap).unwrap().rumor;
        let first = outbox.0.lock().unwrap()[0].1.clone();
        let rumor = open(&first);
        assert_eq!((tag(&rumor.tags, "t"), tag(&rumor.tags, "op")), (Some(TAG), Some("request")));

        // Carol accepte : message `accept` scellé pour Alice, reçu par la station.
        let reply = Rumor { pubkey: carol.clone(), created_at: u64::try_from(now()).unwrap(), kind: kind::CHAT, tags: vec![vec!["t".into(), TAG.into()], vec!["op".into(), "accept".into()], vec!["quota".into(), "1000".into()]], content: String::new() };
        let wrap = gift_wrap(&CAROL, &alice, &reply, reply.created_at).unwrap();
        db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        let stats = backup.poll_once(now()).await;
        assert_eq!(stats.control, 1);
        assert_eq!(backup.status(&alice).await.unwrap()["mine"]["status"], "accepted");

        assert_eq!(backup.run_once().await, 3);
        let sent = outbox.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 4, "une demande + trois blobs");
        for (to, wrap) in &sent[1..] {
            let r = open(wrap);
            assert_eq!((to.as_str(), r.kind, tag(&r.tags, "op")), (carol.as_str(), kind::FILE, Some("item")));
            assert!(tag(&r.tags, "decryption-key").is_none() && tag(&r.tags, "cid").is_some(), "le blob part chiffré, sans sa clé");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    // ───────────────── Clé de secours : la part P3 chez les amis hébergeurs ─────────────────

    fn write_rescue_shares(dir: &std::path::Path, secret: &[u8; 32], email: &str) -> Vec<astro_lifecycle::share::SecretShare> {
        let card = astro_lifecycle::multipass::SecretCard::parse(&format!("astro-secret:v1|{email}|Sel123|Poivre456|legacy||{}", hex(secret))).unwrap();
        let shares = astro_lifecycle::share::split_card(&card).unwrap();
        let account = dir.join("nostr").join(email);
        std::fs::create_dir_all(&account).unwrap();
        astro_lifecycle::share::write_shares(&account, &shares).unwrap();
        shares
    }

    async fn accept_pact(db: &Db, owner: &[u8; 32], host: &[u8; 32]) {
        db.writer().execute("INSERT OR REPLACE INTO backup_pacts(owner_hex, host_hex, status, quota_bytes, enabled, asked_at) VALUES (?1, ?2, 'accepted', 1000, 1, 1)", vec![hex(owner).into(), hex(host).into()]).await.unwrap();
    }

    #[tokio::test]
    async fn only_the_third_share_goes_to_the_friend_who_hosts_the_backup_and_he_can_show_it_until_he_stops() {
        let (alice, carol) = (hex(&ALICE), hex(&CAROL));
        let owner = node("rescue-o", &[(&ALICE, "alice@example.org")], &[(&CAROL, "carol@example.org")], "12D3KooWRescueOwner", None, None).await;
        let host = node("rescue-h", &[(&CAROL, "carol@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWRescueHost", None, None).await;
        let shares = write_rescue_shares(&owner.dir, &ALICE, "alice@example.org");
        for (n, a, b) in [(&owner, &ALICE, &CAROL), (&host, &CAROL, &ALICE)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(a).into(), hex(b).into()]).await.unwrap();
        }
        give(&host.db, follows(&ALICE, &[&CAROL])).await;

        // Sans accord d'hébergement, rien ne part : l'ami n'a rien demandé.
        assert_eq!(owner.backup.send_shares(now()).await, 0);
        assert!(owner.outbox.0.lock().unwrap().is_empty());

        accept_pact(&owner.db, &ALICE, &CAROL).await;
        accept_pact(&host.db, &ALICE, &CAROL).await;
        assert_eq!(owner.backup.send_shares(now()).await, 1);
        assert_eq!(owner.backup.send_shares(now()).await, 0, "un seul envoi tant que la part ne change pas");
        let sent = owner.outbox.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        let rumor = nip59::unwrap(&CAROL, &sent[0].1).unwrap().rumor;
        assert_eq!((tag(&rumor.tags, "op"), rumor.content.as_str()), (Some("share"), shares[2].to_text().as_str()), "P3, ni P1 ni P2");
        assert!(!rumor.content.contains("Sel123") && !rumor.content.contains("Poivre456"));
        assert_eq!(owner.backup.status(&alice).await.unwrap()["rescue_friends"], 1);

        // L'ami la reçoit et peut la montrer (texte et QR) à celui qui le rappelle.
        pump(&owner, &host).await;
        assert_eq!(host.backup.poll_once(now()).await.control, 1);
        let shown = host.backup.status(&carol).await.unwrap();
        assert_eq!(shown["hosting"][0]["share"], shares[2].to_text());
        assert!(shown["hosting"][0]["share_svg"].as_str().is_some_and(|s| s.contains("<svg")));

        // Une part qui n'est pas la P3 de l'expéditeur est écartée (autre numéro, ou clé publique d'un autre compte).
        let forged = |text: String| {
            let r = Rumor { pubkey: alice.clone(), created_at: u64::try_from(now()).unwrap() + 1, kind: kind::CHAT, tags: vec![vec!["t".into(), TAG.into()], vec!["op".into(), "share".into()]], content: text };
            gift_wrap(&ALICE, &carol, &r, r.created_at).unwrap()
        };
        host.db.writer().execute("DELETE FROM kv WHERE scope = ?1", vec![SHARE_HELD.to_owned().into()]).await.unwrap();
        for bad in [shares[0].to_text(), "n'importe quoi".to_owned()] {
            let wrap = forged(bad);
            host.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        }
        let stats = host.backup.poll_once(now()).await;
        assert_eq!(stats.ignored, 2, "P1 et le texte quelconque sont refusés");
        assert!(host.backup.status(&carol).await.unwrap()["hosting"][0].get("share").is_none());

        // L'hébergeur redonne la part puis arrête d'héberger : la part disparaît avec la sauvegarde.
        let wrap = forged(shares[2].to_text());
        host.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        host.backup.poll_once(now()).await;
        assert!(host.backup.status(&carol).await.unwrap()["hosting"][0].get("share").is_some());
        host.backup.respond(&carol, &alice, false, 0, 1000).await.unwrap();
        assert!(host.backup.kv_get(SHARE_HELD, &format!("{carol}:{alice}")).await.is_none());
        for n in [owner, host] {
            let _ = std::fs::remove_dir_all(n.dir);
        }
    }

    #[tokio::test]
    async fn a_friend_hosted_on_the_same_station_holds_the_share_without_any_message() {
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let n = node("rescue-local", &[(&ALICE, "alice@example.org"), (&BOB, "bob@example.org")], &[], "12D3KooWRescueLocal", None, None).await;
        let shares = write_rescue_shares(&n.dir, &ALICE, "alice@example.org");
        accept_pact(&n.db, &ALICE, &BOB).await;
        assert_eq!(n.backup.send_shares(now()).await, 1);
        assert!(n.outbox.0.lock().unwrap().is_empty());
        assert_eq!(n.backup.status(&bob).await.unwrap()["hosting"][0]["share"], shares[2].to_text());
        // Le même compte n'a pas besoin de la part d'un autre : Alice, hébergeuse de personne, ne détient rien.
        assert!(n.backup.status(&alice).await.unwrap()["hosting"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(n.dir);
    }

    // ───────────────── Restauration de bout en bout : trois stations, une panne ─────────────────

    struct Node {
        dir: std::path::PathBuf,
        db: Arc<Db>,
        outbox: Arc<Capture>,
        blobs: Arc<FsBlobStore>,
        backup: PrivateBackup<Capture, FsBlobStore>,
    }

    /// Station de test : `hosted` (clé, email) y sont hébergés, `remote` sont des amis d'ailleurs. `from` : station dont le
    /// maillage rapatrie les blobs (copie entre dossiers, le CID étant le SHA-256 du blob chiffré).
    async fn node(tag: &str, hosted: &[(&[u8; 32], &str)], remote: &[(&[u8; 32], &str)], peer: &str, home: Option<String>, from: Option<Arc<FsBlobStore>>) -> Node {
        let dir = std::env::temp_dir().join(format!("astroportd-restore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let mut keys = StaticKeySource::new();
        for (secret, email) in hosted {
            db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(secret).into(), (*email).to_owned().into()]).await.unwrap();
            keys = keys.with(**secret);
        }
        for (secret, email) in remote {
            db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 0)", vec![hex(secret).into(), (*email).to_owned().into()]).await.unwrap();
        }
        let keys: Arc<dyn KeySource> = Arc::new(keys);
        let outbox = Arc::new(Capture::default());
        let sharing = Arc::new(Sharing::new(db.clone(), keys.clone(), outbox.clone(), None, None));
        let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
        let mut backup = PrivateBackup::new(db.clone(), keys, outbox.clone(), blobs.clone(), sharing, ActivityBus::new()).with_station(None, Some(peer.to_owned())).with_home(home, None).with_accounts_dir(Some(dir.join("nostr")));
        if let Some(origin) = from {
            let target = blobs.clone();
            backup = backup.with_prefetch(Arc::new(move |cid: String, _peer: String| {
                let (origin, target) = (origin.clone(), target.clone());
                Box::pin(async move {
                    use astro_dav::BlobWriter as _;
                    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                    let (mut reader, _) = origin.open(&cid).await.map_err(|e| e.to_string())?;
                    let mut bytes = Vec::new();
                    reader.read_to_end(&mut bytes).await.map_err(|e| e.to_string())?;
                    let mut w = target.begin().await.map_err(|e| e.to_string())?;
                    w.write_all(&bytes).await.map_err(|e| e.to_string())?;
                    let got = w.commit().await.map_err(|e| e.to_string())?.cid;
                    if got == cid { Ok(()) } else { Err(format!("blob altéré : {got} au lieu de {cid}")) }
                })
            }));
        }
        Node { dir, db, outbox, blobs, backup }
    }

    /// Ce que le crawl rapatrie : l'événement est rangé et projeté (graphe social, DID).
    async fn give(db: &Db, event: Event) {
        db.writer()
            .transaction(move |tx| {
                astro_db::events::insert_event(tx, &event)?;
                astro_db::apply_statements(tx, &astro_sync::Projector::plan(&event).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?)?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Les messages que `from` a envoyés arrivent chez `to` (un relais les lui sert).
    async fn pump(from: &Node, to: &Node) -> usize {
        let msgs: Vec<(String, Event)> = from.outbox.0.lock().unwrap().drain(..).collect();
        let n = msgs.len();
        for (_, wrap) in msgs {
            to.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        }
        n
    }

    fn follows(secret: &[u8; 32], who: &[&[u8; 32]]) -> Event {
        UnsignedEvent { created_at: 100, kind: 3, tags: who.iter().map(|w| vec!["p".to_owned(), hex(w)]).collect(), content: String::new() }.sign(secret).unwrap()
    }

    fn did_of(secret: &[u8; 32], email: &str) -> Event {
        let id = hex(secret);
        let doc = json!({ "id": format!("did:nostr:{id}"), "type": "DIDNostr", "metadata": { "email": email, "contractStatus": "active_rental", "language": "fr" } });
        UnsignedEvent { created_at: 100, kind: kind::DID, tags: vec![vec!["d".into(), "did".into()], vec!["t".into(), "uplanet".into()], vec!["t".into(), "did-document".into()]], content: doc.to_string() }.sign(secret).unwrap()
    }

    async fn scalar(db: &Db, sql: &'static str, args: Vec<String>) -> i64 {
        db.readers().read(move |c| Ok(c.query_row(sql, rusqlite::params_from_iter(args), |r| r.get(0))?)).await.unwrap()
    }

    #[tokio::test]
    async fn a_member_whose_station_burned_gets_back_the_account_the_files_and_the_home_without_any_friend_acting() {
        const NODE: &str = "ab";
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let new_home = format!("12D3KooWNewHome:{}", NODE.repeat(32));
        let old_home = format!("12D3KooWOldHome:{}", "cd".repeat(32));

        // Trois stations : l'ancienne (Alice), celle de Bob, et la nouvelle (Alice rétablie). Chacune rapatrie depuis l'autre.
        let old = node("old", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWOldStation", Some(old_home.clone()), None).await;
        let host = node("host", &[(&BOB, "bob@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWBobStation", None, Some(old.blobs.clone())).await;
        // La nouvelle station héberge Alice dès son rétablissement : sa clé (la même, dérivée de SALT/PEPPER) y est connue.
        let new = node("new", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWNewStation", Some(new_home.clone()), Some(host.blobs.clone())).await;

        // Chacune connaît le graphe social de la sienne.
        for (n, owner, friend) in [(&old, &ALICE, &BOB), (&host, &BOB, &ALICE)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(owner).into(), hex(friend).into()]).await.unwrap();
        }
        give(&old.db, follows(&BOB, &[&ALICE])).await;
        give(&host.db, follows(&ALICE, &[&BOB])).await;

        // 1. Alice a un uCloud : trois fichiers chiffrés, leur trousseau, un manifeste publié.
        let mut files = Vec::new();
        for (i, name) in ["a.txt", "b.txt", "c.txt"].iter().enumerate() {
            let cid = put_blob(&old.backup, format!("contenu chiffré de {name}").as_bytes()).await;
            let (path, key) = (format!("/Docs/{name}"), format!("{:064x}", i + 1));
            old.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, '/Docs', 'file', ?3, 'text/plain', 20, 1, '{}')", vec![alice.clone().into(), path.into(), cid.clone().into()]).await.unwrap();
            old.db.writer().execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", vec![alice.clone().into(), cid.clone().into(), key.into()]).await.unwrap();
            files.push(cid);
        }
        let published = manifest::publish(&old.db, &*old.blobs, &*old.backup.keys, &alice).await.unwrap();
        let manifest_cid = published.pointer.manifest_cid.clone();

        // 2. Bob accepte d'héberger ; la nuit, tout part chez lui : les trois fichiers ET le manifeste.
        old.backup.set(&alice, true, Some(&bob)).await.unwrap();
        pump(&old, &host).await;
        host.backup.poll_once(now()).await;
        host.backup.respond(&bob, &alice, true, 1_000_000, 1_000_000).await.unwrap();
        pump(&host, &old).await;
        old.backup.poll_once(now()).await;
        assert_eq!(old.backup.run_once().await, 4, "le manifeste et les trois fichiers privés");
        pump(&old, &host).await;
        assert_eq!(host.backup.poll_once(now()).await.stored, 4);
        assert_eq!(scalar(&host.db, "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1 AND cid = ?2", vec![alice.clone(), manifest_cid.clone()]).await, 1, "le blob du manifeste est chez l'ami");
        let manifest_event = old.backup.latest_event(&alice, kind::APP_DATA, Some(manifest::D_TAG)).await.unwrap();
        let did = did_of(&ALICE, "alice@example.org");

        // 3. La station d'Alice brûle. Une station vierge la rétablit depuis sa carte : même clé, état « restauration en cours ».
        let old_dir = old.dir.clone();
        drop(old);
        let _ = std::fs::remove_dir_all(old_dir);
        new.backup.kv_put("restore", &alice, "pending", now()).await;

        // Aucun manifeste vide ne doit remplacer l'original tant que la restauration dure.
        assert!(manifest::restoring(&new.db, &alice).await);
        assert!(manifest::publish(&new.db, &*new.blobs, &*new.backup.keys, &alice).await.is_err());

        // 4. Le crawl rapatrie les événements d'Alice (contacts, DID, manifeste) et ceux de Bob.
        for e in [follows(&ALICE, &[&BOB]), did.clone(), manifest_event.clone(), follows(&BOB, &[&ALICE])] {
            give(&new.db, e).await;
        }
        // Un premier passage : annonce de la nouvelle station d'accueil + demande à chaque proche.
        assert_eq!(new.backup.restore_step(now()).await, 1, "une demande, pour Bob");
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM kv WHERE scope = 'restore_home' AND key = ?1", vec![alice.clone()]).await, 1);
        let announced: String = new.db.readers().read({ let a = alice.clone(); move |c| Ok(c.query_row("SELECT home_station FROM identities WHERE hex = ?1", [a], |r| r.get(0))?) }).await.unwrap();
        assert_eq!(announced, new_home, "le DID republié porte la nouvelle station d'accueil");
        assert_eq!(new.backup.restore_step(now()).await, 0, "la demande n'est pas renouvelée avant 6 h");

        // 5. Bob n'a RIEN à faire : sa station apprend la nouvelle Home et renvoie d'elle-même ce qu'elle détient.
        pump(&new, &host).await;
        assert_eq!(host.backup.poll_once(now()).await.control, 1);
        let bobs_view: String = host.db.readers().read({ let a = alice.clone(); move |c| Ok(c.query_row("SELECT COALESCE(home_station, '') FROM identities WHERE hex = ?1", [a], |r| r.get(0))?) }).await.unwrap();
        assert_eq!(bobs_view, new_home, "les amis écrivent désormais à la nouvelle station");
        assert_eq!(host.backup.serve_restores().await, 4, "le manifeste d'abord, puis les trois fichiers, sans doublon");
        assert_eq!(host.backup.serve_restores().await, 0);
        assert_eq!(scalar(&host.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_restore_job'", vec![]).await, 0, "renvoi terminé");

        // 6. Alice retrouve tout : manifeste appliqué, VFS et trousseau, blobs, accord de sauvegarde rétabli.
        pump(&host, &new).await;
        let stats = new.backup.poll_once(now()).await;
        assert_eq!((stats.stored, stats.ignored, stats.deferred), (4, 0, 0));
        assert_eq!(new.backup.kv_get("restore", &alice).await.map(|v| v.0), Some("done".to_owned()));
        assert!(!manifest::restoring(&new.db, &alice).await);
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", vec![alice.clone()]).await, 3);
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM ucloud_keyring WHERE owner_hex = ?1", vec![alice.clone()]).await, 3);
        for cid in &files {
            assert!(new.blobs.open(cid).await.is_ok(), "le blob {cid} est revenu");
        }
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2 AND status = 'accepted' AND enabled = 1", vec![alice.clone(), bob.clone()]).await, 1);
        assert_eq!(new.backup.run_once().await, 0, "tout est déjà chez Bob : rien à renvoyer cette nuit");

        // 7. Garde-fous : un proche quelconque ne récupère pas les sauvegardes d'autrui.
        let eve = [0xE5u8; 32];
        host.db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'eve@example.org', 0)", vec![hex(&eve).into()]).await.unwrap();
        host.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![bob.clone().into(), hex(&eve).into()]).await.unwrap();
        give(&host.db, follows(&eve, &[&BOB])).await;
        let ask = Rumor { pubkey: hex(&eve), created_at: u64::try_from(now()).unwrap(), kind: kind::CHAT, tags: vec![vec!["p".into(), bob.clone()], vec!["t".into(), TAG.into()], vec!["op".into(), "restore".into()], vec!["cid".into(), files[0].clone()]], content: String::new() };
        let wrap = gift_wrap(&eve, &bob, &ask, ask.created_at).unwrap();
        host.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        host.backup.poll_once(now()).await;
        assert_eq!(host.backup.serve_restores().await, 0, "le CID d'un fichier d'Alice ne se redemande pas au nom d'Eve");
        assert_eq!(scalar(&host.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_restore_job'", vec![]).await, 0);

        for n in [&new, &host] {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    #[tokio::test]
    async fn files_that_a_friend_received_from_the_member_come_back_by_cid_without_creating_a_backup_agreement() {
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let host = node("share-host", &[(&BOB, "bob@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWBobStation", None, None).await;
        let new = node("share-new", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWNewStation", None, Some(host.blobs.clone())).await;
        for (n, owner, friend) in [(&new, &ALICE, &BOB), (&host, &BOB, &ALICE)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(owner).into(), hex(friend).into()]).await.unwrap();
        }
        for n in [&new, &host] {
            give(&n.db, follows(&ALICE, &[&BOB])).await;
            give(&n.db, follows(&BOB, &[&ALICE])).await;
        }

        // Bob a reçu un fichier d'Alice (rangé sous /Partages) ; Alice, rétablie, le connaît par son manifeste mais n'a plus le blob.
        let shared = put_blob(&host.backup, b"photo partagee avec Bob").await;
        host.db
            .writer()
            .execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, '/Partages/De_alice/x.jpg', '/Partages/De_alice', 'file', ?2, 'image/jpeg', 23, 1, ?3)", vec![bob.clone().into(), shared.clone().into(), json!({ "shared_by": alice }).to_string().into()])
            .await
            .unwrap();
        // Un fichier privé de Bob, que personne d'autre ne doit pouvoir se faire rendre.
        let private = put_blob(&host.backup, b"journal prive de Bob").await;
        host.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, '/Docs/journal.txt', '/Docs', 'file', ?2, 'text/plain', 20, 1, '{}')", vec![bob.clone().into(), private.clone().into()]).await.unwrap();

        for cid in [&shared, &private] {
            new.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, '/Docs', 'file', ?3, 'image/jpeg', 23, 1, '{}')", vec![alice.clone().into(), format!("/Docs/{}", &cid[cid.len() - 6..]).into(), cid.clone().into()]).await.unwrap();
        }
        let at = now();
        new.backup.kv_put("restore", &alice, "done", at - 100_000).await;
        new.backup.kv_put("restore_missing", &alice, "0", at - 100_000).await;

        assert_eq!(new.backup.restore_step(at).await, 1, "une demande par CID manquant, pour Bob");
        pump(&new, &host).await;
        host.backup.poll_once(now()).await;
        assert_eq!(host.backup.serve_restores().await, 1, "seul le fichier qu'Alice lui avait partagé est rendu, pas le journal de Bob");
        pump(&host, &new).await;
        let stats = new.backup.poll_once(now()).await;
        assert_eq!((stats.stored, stats.ignored), (1, 0));
        assert!(new.blobs.open(&shared).await.is_ok() && new.blobs.open(&private).await.is_err());
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM backup_pacts", vec![]).await, 0, "un fichier partagé ne crée aucun accord de sauvegarde");
        assert_eq!(new.backup.kv_get("restore_missing", &alice).await.map(|v| v.0), Some("1".to_owned()));
        assert_eq!(new.backup.restore_step(now()).await, 0, "pas de relance avant 6 h");

        // Un message `restored` qu'aucune restauration n'attendait est écarté sans bruit.
        let stranger = node("share-stranger", &[(&CAROL, "carol@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWCarolStation", None, None).await;
        stranger.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&CAROL).into(), bob.clone().into()]).await.unwrap();
        give(&stranger.db, follows(&BOB, &[&CAROL])).await;
        give(&host.db, follows(&CAROL, &[&BOB])).await;
        host.db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'carol@example.org', 0)", vec![hex(&CAROL).into()]).await.unwrap();
        host.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![bob.clone().into(), hex(&CAROL).into()]).await.unwrap();
        host.backup.send_restored(&bob, &hex(&CAROL), &shared, 23, 0, "share").await.unwrap();
        pump(&host, &stranger).await;
        let stats = stranger.backup.poll_once(now()).await;
        assert_eq!((stats.stored, stats.ignored), (0, 1));

        for n in [&new, &host, &stranger] {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    fn did_at(secret: &[u8; 32], email: &str, home: &str) -> Event {
        let id = hex(secret);
        let doc = json!({ "id": format!("did:nostr:{id}"), "type": "DIDNostr", "metadata": { "email": email, "contractStatus": "active_rental", "language": "fr", "astroportStation": home } });
        UnsignedEvent { created_at: 100, kind: kind::DID, tags: vec![vec!["d".into(), "did".into()], vec!["t".into(), "uplanet".into()], vec!["home_station".into(), home.to_owned()]], content: doc.to_string() }.sign(secret).unwrap()
    }

    /// Un message `moved` emballé pour `to`, signé de `from` (le compte lui-même, ou un tiers qui le contrefait).
    async fn moved_wrap(db: &Db, from: &[u8; 32], to: &[u8; 32], home: &str, at: u64) {
        let rumor = Rumor { pubkey: hex(from), created_at: at, kind: kind::CHAT, tags: vec![vec!["p".into(), hex(to)], vec!["t".into(), TAG.into()], vec!["op".into(), "moved".into()], vec!["home".into(), home.to_owned()]], content: String::new() };
        let wrap = gift_wrap(from, &hex(to), &rumor, at).unwrap();
        db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
    }

    #[tokio::test]
    async fn a_member_who_moves_away_on_purpose_makes_the_old_station_stop_hosting_the_account() {
        let alice = hex(&ALICE);
        let (old_home, new_home) = (format!("12D3KooWOldHome:{}", "cd".repeat(32)), format!("12D3KooWNewHome:{}", "ab".repeat(32)));
        let old = node("mv-old", &[(&ALICE, "alice@example.org")], &[], "12D3KooWOldStation", Some(old_home.clone()), None).await;
        let new = node("mv-new", &[(&ALICE, "alice@example.org")], &[], "12D3KooWNewStation", Some(new_home.clone()), None).await;
        let folder = old.dir.join("nostr/alice@example.org");
        std::fs::create_dir_all(&folder).unwrap();
        let nsec = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), &ALICE).unwrap();
        std::fs::write(folder.join(".secret.nostr"), format!("NSEC={nsec}; NPUB=x; HEX={alice}\n")).unwrap();
        let files = astro_crypto::FileKeySource::new(old.dir.join("nostr"));
        assert!(files.secret(&alice).is_some(), "avant : la station lit la clé du compte dans son dossier");

        // La nouvelle station connaît l'ancienne Home par le DID rapatrié ; elle s'annonce et prévient l'ancienne.
        give(&new.db, did_at(&ALICE, "alice@example.org", &old_home)).await;
        new.backup.kv_put("restore", &alice, "pending", now()).await;
        new.backup.restore_step(now()).await;
        assert_eq!(new.backup.kv_get("restore_moved", &alice).await.map(|v| v.0), Some("sent".to_owned()));
        assert_eq!(new.outbox.0.lock().unwrap().len(), 1, "un seul message `moved`, adressé au compte lui-même");
        new.backup.restore_step(now() + 3 * 3600).await;
        assert_eq!(new.outbox.0.lock().unwrap().len(), 1, "pas renvoyé une fois parti");

        // L'ancienne station le reçoit : le compte n'est plus hébergé, pointe vers la nouvelle Home, son dossier est mis de côté.
        pump(&new, &old).await;
        assert_eq!(old.backup.poll_once(now()).await.control, 1);
        let (hosted, home): (i64, String) = old.db.readers().read({ let a = alice.clone(); move |c| Ok(c.query_row("SELECT hosted, COALESCE(home_station, '') FROM identities WHERE hex = ?1", [a], |r| Ok((r.get(0)?, r.get(1)?)))?) }).await.unwrap();
        assert_eq!((hosted, home.as_str()), (0, new_home.as_str()));
        assert!(!folder.exists(), "plus de clé du compte dans le dossier lu par la station");
        let aside: Vec<_> = std::fs::read_dir(old.dir.join("nostr/.moved")).unwrap().flatten().collect();
        assert_eq!(aside.len(), 1);
        assert!(aside[0].path().join(".secret.nostr").is_file(), "mis de côté, jamais effacé");
        assert_eq!(old.backup.kv_get("moved", &alice).await.map(|v| v.0), Some(new_home.clone()));
        // `.moved` garde le compte 30 jours (il contient encore ses clés), puis la nuit l'efface ; un nom inattendu est laissé.
        let stray = old.dir.join("nostr/.moved/pas-de-date");
        std::fs::create_dir_all(&stray).unwrap();
        assert_eq!(old.backup.purge_moved(now() + MOVED_KEEP_SECS - 3600).await.unwrap(), 0, "trop tôt");
        assert!(aside[0].path().exists());
        assert_eq!(old.backup.purge_moved(now() + MOVED_KEEP_SECS + 3600).await.unwrap(), 1);
        assert!(!aside[0].path().exists() && stray.exists(), "le compte est effacé, le dossier sans date est laissé");
        assert!(files.secret(&alice).is_none(), "après : la station ne peut plus signer pour ce compte");

        // Garde-fous, sur une autre ancienne station (le compte y est hébergé) : rien de tout cela ne doit le débarquer à tort.
        let station = node("mv-guard", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWGuardStation", Some(old_home.clone()), None).await;
        let kept = || async { scalar(&station.db, "SELECT hosted FROM identities WHERE hex = ?1", vec![hex(&ALICE)]).await };
        let t = u64::try_from(now()).unwrap();
        moved_wrap(&station.db, &BOB, &ALICE, &new_home, t).await; // contrefait par un tiers
        moved_wrap(&station.db, &ALICE, &ALICE, &old_home, t).await; // la station annoncée est celle-ci
        moved_wrap(&station.db, &ALICE, &ALICE, "pas-une-station", t).await; // adresse illisible
        station.backup.poll_once(now()).await;
        assert_eq!(kept().await, 1, "ni un tiers, ni la même station, ni une adresse invalide ne comptent");
        // Le Capitaine n'est pas débarqué par un message : le Capitaine tranche.
        station.db.writer().execute("UPDATE identities SET role = 'captain' WHERE hex = ?1", vec![hex(&ALICE).into()]).await.unwrap();
        moved_wrap(&station.db, &ALICE, &ALICE, &new_home, t).await;
        station.backup.poll_once(now()).await;
        assert_eq!(kept().await, 1);
        station.db.writer().execute("UPDATE identities SET role = 'multipass' WHERE hex = ?1", vec![hex(&ALICE).into()]).await.unwrap();
        // Un ancien message rejoué après un rétablissement ici (postérieur au message) est ignoré ; un message plus récent compte.
        station.backup.kv_put("restore", &hex(&ALICE), "done", now() + 1000).await;
        moved_wrap(&station.db, &ALICE, &ALICE, &new_home, t).await;
        station.backup.poll_once(now()).await;
        assert_eq!(kept().await, 1, "un message antérieur au dernier rétablissement ici est un rejeu");
        station.backup.kv_put("restore", &hex(&ALICE), "done", now() - 1000).await;
        moved_wrap(&station.db, &ALICE, &ALICE, &new_home, t).await;
        station.backup.poll_once(now()).await;
        assert_eq!(kept().await, 0, "un vrai déménagement, postérieur à tout, est appliqué");
        for n in [&old, &new, &station] {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }
}
