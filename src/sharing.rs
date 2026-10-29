//! Envoyer des fichiers de son uCloud à des **proches** (double suivi), sur cette station ou sur une autre.
//!
//! - ami hébergé **ici** : le fichier est rangé directement chez lui (`/Partages/De_<vous>/`), même blob, même clé ;
//! - ami hébergé **ailleurs** : un message fichier (NIP-17) emballé en NIP-59 part vers la station de l'ami
//!   ([`crate::mesh_outbox::MeshOutbox`]) ; sa station rapatrie le blob chiffré par le maillage ([`crate::share_inbox`]),
//!   sans kubo.
//!
//! Seuls les proches **réciproques** reçoivent : un envoi à quelqu'un qui ne vous suit pas est refusé avec sa raison.

use std::sync::Arc;

use astro_crypto::KeySource;
use astro_crypto::nip59::{Rumor, gift_wrap};
use astro_dav::ManifestPublisher;
use astro_dav::adopt::{SharedFile, adopt_shared_file, sender_label};
use astro_db::Db;
use astro_facecloud::Outbox;
use astro_nostr_proto::kind;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

/// Fichiers au plus par envoi.
pub const MAX_FILES: usize = 200;
/// Destinataires au plus par envoi.
pub const MAX_FRIENDS: usize = 50;

/// Un proche réciproque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Friend {
    /// Clé Nostr.
    pub hex: String,
    /// Nom affiché : profil Nostr, sinon email, sinon début de la clé.
    pub label: String,
    /// Hébergé sur cette station.
    pub hosted: bool,
}

/// Fichiers (et contenu des dossiers) de `owner` désignés par `paths`, avec ce qu'il faut pour les envoyer : chemin et
/// fichier chiffré (CID, clé). Les chemins qui n'existent pas, ou qui ne sont pas à `owner`, ne rendent rien.
pub async fn resolve_files(db: &Db, owner: &str, paths: &[String]) -> Result<Vec<(String, SharedFile)>, String> {
    let (o, paths) = (owner.to_owned(), paths.to_vec());
    db.readers()
        .read(move |c| {
            let mut out = Vec::new();
            for p in &paths {
                let like = format!("{}/%", p.trim_end_matches('/').replace('%', "\\%").replace('_', "\\_"));
                let mut stmt = c.prepare(
                    "SELECT e.path, e.cid, COALESCE(e.mime, 'application/octet-stream'), COALESCE(e.size, 0), COALESCE(e.sha_plain, ''), k.key_hex \
                     FROM ucloud_entries e JOIN ucloud_keyring k ON k.owner_hex = e.owner_hex AND k.cid = e.cid \
                     WHERE e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL AND (e.path = ?2 OR e.path LIKE ?3 ESCAPE '\\') ORDER BY e.path LIMIT ?4",
                )?;
                let rows = stmt.query_map(rusqlite::params![o, p, like, i64::try_from(MAX_FILES + 1).unwrap_or(i64::MAX)], |r| {
                    let path: String = r.get(0)?;
                    let name = astro_dav::vfs::name_of(&path).to_owned();
                    Ok((path, SharedFile { cid: r.get(1)?, mime: r.get(2)?, size: u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0), sha_plain: r.get(4)?, key_hex: r.get(5)?, name }))
                })?;
                for row in rows {
                    out.push(row?);
                }
            }
            Ok(out)
        })
        .await
        .map_err(|e| e.to_string())
}

/// Envoi de fichiers.
pub struct Sharing<O: Outbox> {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    outbox: Arc<O>,
    publisher: Option<ManifestPublisher>,
    station_peer: Option<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl<O: Outbox> Sharing<O> {
    /// `station_peer` : PeerID de cette station, indiqué au destinataire pour rapatrier le blob au bon endroit.
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, outbox: Arc<O>, publisher: Option<ManifestPublisher>, station_peer: Option<String>) -> Self {
        Self { db, keys, outbox, publisher, station_peer }
    }

    /// Proches réciproques de `owner` : il les suit (niveau 1) et ils le suivent (leur dernière liste de contacts).
    pub async fn friends(&self, owner: &str) -> Result<Vec<Friend>, String> {
        let o = owner.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT g.pubkey, COALESCE((SELECT hosted FROM identities i WHERE i.hex = g.pubkey), 0), \
                            (SELECT email FROM identities i WHERE i.hex = g.pubkey), \
                            (SELECT json_extract(e.content, '$.name') FROM nostr_events e WHERE e.pubkey = g.pubkey AND e.kind = 0 ORDER BY e.created_at DESC LIMIT 1) \
                     FROM social_graph g WHERE g.owner_hex = ?1 AND g.level = 1 \
                       AND EXISTS (SELECT 1 FROM nostr_events k JOIN nostr_tags t ON t.event_id = k.id WHERE k.kind = 3 AND k.pubkey = g.pubkey AND t.name = 'p' AND t.value = ?1) \
                     ORDER BY 1",
                )?;
                let rows = stmt
                    .query_map([&o], |r| {
                        let (hex, hosted, email, name): (String, i64, Option<String>, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
                        let label = name.filter(|n| !n.trim().is_empty()).or_else(|| email.map(|e| e.split('@').next().unwrap_or_default().to_owned())).filter(|l| !l.is_empty()).unwrap_or_else(|| hex.chars().take(8).collect());
                        Ok(Friend { hex, label, hosted: hosted == 1 })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Message fichier (NIP-17, kind 15) emballé en NIP-59 pour `friend`, avec l'indication de la station qui détient le blob.
    fn wrap_for(&self, secret: &[u8; 32], owner: &str, friend: &str, file: &SharedFile) -> Result<astro_nostr_proto::Event, String> {
        let mut tags = vec![
            vec!["p".to_owned(), friend.to_owned()],
            vec!["file-type".into(), file.mime.clone()],
            vec!["encryption-algorithm".into(), "uenc-v2".into()],
            vec!["decryption-key".into(), file.key_hex.clone()],
            vec!["cid".into(), file.cid.clone()],
            vec!["ox".into(), file.sha_plain.clone()],
            vec!["size".into(), file.size.to_string()],
            vec!["name".into(), file.name.clone()],
        ];
        if let Some(peer) = &self.station_peer {
            tags.push(vec!["station".into(), peer.clone()]);
        }
        let rumor = Rumor { pubkey: owner.to_owned(), created_at: u64::try_from(now()).unwrap_or(0), kind: kind::FILE, tags, content: format!("ipfs://{}", file.cid) };
        gift_wrap(secret, friend, &rumor, rumor.created_at).map_err(|_| "emballage NIP-59 impossible".to_owned())
    }

    /// Envoie `files` de `owner` à `friend`, hébergé **ailleurs**, sans vérifier le suivi réciproque : l'appelant (une invitation
    /// acceptée) vient de l'établir. Rend `(envoyés, première erreur)`.
    pub async fn deliver_remote(&self, owner: &str, friend: &str, files: &[SharedFile]) -> (usize, Option<String>) {
        let Some(secret) = self.keys.secret(owner) else { return (0, Some("clé du compte indisponible : envoi impossible".into())) };
        let (mut sent, mut first) = (0usize, None);
        for file in files {
            let result = match self.wrap_for(&secret, owner, friend, file) {
                Ok(wrap) => self.outbox.send(friend, wrap).await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => sent += 1,
                Err(e) => first = first.or(Some(e)),
            }
        }
        (sent, first)
    }

    /// Fichiers (et contenu des dossiers) désignés par `paths`, avec ce qu'il faut pour les envoyer.
    async fn files(&self, owner: &str, paths: &[String]) -> Result<Vec<(String, SharedFile)>, String> {
        resolve_files(&self.db, owner, paths).await
    }

    /// Envoie `paths` à chacun de `friends`. Rend, par ami, ce qui est parti ou la raison du refus.
    pub async fn send(&self, owner: &str, paths: &[String], friends: &[String]) -> Result<Value, String> {
        if paths.is_empty() || friends.is_empty() || friends.len() > MAX_FRIENDS {
            return Err(format!("1 à {MAX_FILES} fichiers et 1 à {MAX_FRIENDS} proches attendus"));
        }
        let mut files = self.files(owner, paths).await?;
        files.dedup_by(|a, b| a.0 == b.0);
        if files.is_empty() {
            return Err("aucun fichier à envoyer (chemins introuvables, ou dossiers vides)".into());
        }
        if files.len() > MAX_FILES {
            return Err(format!("trop de fichiers (maximum {MAX_FILES})"));
        }
        let allowed = self.friends(owner).await?;
        let secret = self.keys.secret(owner).ok_or("clé du compte indisponible : envoi impossible")?;
        let my_label = {
            let o = owner.to_owned();
            self.db.readers().read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1", [&o], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten()
        };
        let my_label = sender_label(my_label.as_deref(), owner);
        let mut report = Vec::new();
        for friend in friends {
            let Some(f) = allowed.iter().find(|a| &a.hex == friend) else {
                report.push(json!({ "friend": friend, "sent": 0, "error": "pas un proche réciproque (vous vous suivez mutuellement ?)" }));
                continue;
            };
            let (mut sent, mut errors) = (0usize, Vec::new());
            for (_, file) in &files {
                let result = if f.hosted {
                    adopt_shared_file(&self.db, &f.hex, owner, &my_label, file, "friend-share").await.map(|_| ()).map_err(|e| e.to_string())
                } else {
                    match self.wrap_for(&secret, owner, &f.hex, file) {
                        Ok(wrap) => self.outbox.send(&f.hex, wrap).await.map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    }
                };
                match result {
                    Ok(()) => sent += 1,
                    Err(e) => errors.push(e),
                }
            }
            if f.hosted && sent > 0 {
                if let Some(p) = &self.publisher {
                    p.notify(&f.hex);
                }
            }
            errors.dedup();
            report.push(json!({ "friend": f.hex, "label": f.label, "via": if f.hosted { "local" } else { "maillage" }, "sent": sent, "error": errors.first() }));
        }
        let total: usize = report.iter().filter_map(|r| r["sent"].as_u64()).map(|n| n as usize).sum();
        let _ = self
            .db
            .writer()
            .execute("INSERT INTO journal(stream, at, data) VALUES ('share_out', ?1, ?2)", vec![now().into(), json!({ "owner": owner, "files": files.len(), "friends": friends.len(), "sent": total }).to_string().into()])
            .await;
        Ok(json!({ "files": files.len(), "results": report }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_crypto::StaticKeySource;
    use astro_crypto::nip59;
    use astro_db::events::insert_event;
    use astro_facecloud::OutboxError;
    use astro_nostr_proto::{Event, UnsignedEvent, pubkey_of};

    use super::*;

    const ALICE: [u8; 32] = [0xA1; 32];
    const BOB: [u8; 32] = [0xB2; 32]; // ami réciproque hébergé ici
    const CAROL: [u8; 32] = [0xC3; 32]; // amie réciproque hébergée ailleurs
    const DAVE: [u8; 32] = [0xD4; 32]; // suivi par Alice, ne la suit pas

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

    async fn rig(tag: &str) -> (std::path::PathBuf, Arc<Db>, Arc<Capture>, Sharing<Capture>) {
        let dir = std::env::temp_dir().join(format!("astroportd-sharing-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        for (s, email, hosted) in [(&ALICE, "alice@example.org", 1), (&BOB, "bob@example.org", 1), (&CAROL, "carol@example.org", 0), (&DAVE, "dave@example.org", 0)] {
            db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, ?3)", vec![hex(s).into(), email.to_owned().into(), i64::from(hosted).into()]).await.unwrap();
        }
        for who in [&BOB, &CAROL, &DAVE] {
            db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(who).into()]).await.unwrap();
        }
        for (secret, follows) in [(&BOB, [&ALICE]), (&CAROL, [&ALICE]), (&DAVE, [&CAROL])] {
            let ev = UnsignedEvent { created_at: 100, kind: 3, tags: follows.iter().map(|f| vec!["p".to_owned(), hex(f)]).collect(), content: String::new() }.sign(secret).unwrap();
            db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
        }
        let cid = "bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e";
        for (path, parent) in [("/Photos/a.jpg", "/Photos"), ("/Photos/b.jpg", "/Photos")] {
            db.writer()
                .execute(
                    "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, sha_plain, data) VALUES (?1, ?2, ?3, 'file', ?4, 'image/jpeg', 10, 1, ?5, '{}')",
                    vec![hex(&ALICE).into(), path.to_owned().into(), parent.to_owned().into(), cid.to_owned().into(), "ee".repeat(32).into()],
                )
                .await
                .unwrap();
        }
        db.writer().execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", vec![hex(&ALICE).into(), cid.to_owned().into(), "ab".repeat(32).into()]).await.unwrap();
        let outbox = Arc::new(Capture::default());
        let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
        let sharing = Sharing::new(db.clone(), keys, outbox.clone(), None, Some("12D3KooWSender".into()));
        (dir, db, outbox, sharing)
    }

    #[tokio::test]
    async fn only_reciprocal_friends_are_offered_and_labelled() {
        let (dir, _, _, sharing) = rig("friends").await;
        let friends = sharing.friends(&hex(&ALICE)).await.unwrap();
        let mut got: Vec<(String, bool)> = friends.iter().map(|f| (f.label.clone(), f.hosted)).collect();
        got.sort();
        assert_eq!(got, [("bob".to_owned(), true), ("carol".to_owned(), false)], "Dave ne la suit pas : absent");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn files_go_straight_to_a_local_friend_and_as_a_sealed_message_to_a_remote_one() {
        let (dir, db, outbox, sharing) = rig("send").await;
        let r = sharing.send(&hex(&ALICE), &["/Photos".to_owned()], &[hex(&BOB), hex(&CAROL), hex(&DAVE)]).await.unwrap();
        assert_eq!(r["files"], 2);
        let by = |h: &str| r["results"].as_array().unwrap().iter().find(|x| x["friend"] == h).unwrap().clone();
        assert_eq!((by(&hex(&BOB))["via"].as_str(), by(&hex(&BOB))["sent"].as_u64()), (Some("local"), Some(2)));
        assert_eq!((by(&hex(&CAROL))["via"].as_str(), by(&hex(&CAROL))["sent"].as_u64()), (Some("maillage"), Some(2)));
        assert!(by(&hex(&DAVE))["error"].as_str().unwrap().contains("réciproque"), "refus dit avec sa raison");
        // Bob : rangé chez lui, même blob ; Carol : deux enveloppes qu'elle seule peut ouvrir, avec l'indication de station.
        let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Partages/De_alice/%'", [hex(&BOB)], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 2);
        let sent = outbox.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|(to, _)| to == &hex(&CAROL)));
        let opened = nip59::unwrap(&CAROL, &sent[0].1).unwrap();
        assert_eq!(opened.sender, hex(&ALICE));
        let tag = |n: &str| opened.rumor.tags.iter().find(|t| t[0] == n).map(|t| t[1].clone());
        assert_eq!((tag("station").as_deref(), tag("decryption-key").as_deref(), opened.rumor.kind), (Some("12D3KooWSender"), Some("ab".repeat(32).as_str()), kind::FILE));
        assert!(nip59::unwrap(&BOB, &sent[0].1).is_err(), "personne d'autre ne l'ouvre");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn bad_requests_are_refused_before_anything_leaves() {
        let (dir, _, outbox, sharing) = rig("bad").await;
        let owner = hex(&ALICE);
        assert!(sharing.send(&owner, &[], &[hex(&BOB)]).await.is_err());
        assert!(sharing.send(&owner, &["/Photos/a.jpg".into()], &[]).await.is_err());
        assert!(sharing.send(&owner, &["/inexistant".into()], &[hex(&CAROL)]).await.unwrap_err().contains("aucun fichier"));
        assert!(outbox.0.lock().unwrap().is_empty());
        // Les fichiers d'un autre compte ne sont jamais joignables.
        assert!(sharing.send(&hex(&BOB), &["/Photos/a.jpg".into()], &[hex(&ALICE)]).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
