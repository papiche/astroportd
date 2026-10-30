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
//! La restauration d'un compte depuis ces blobs n'est pas encore automatisée.

use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_crypto::nip59::{self, Rumor, gift_wrap};
use astro_dav::adopt::{adopt_backup_blob, sender_label};
use astro_dav::{BlobStore, ManifestPublisher};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_facecloud::Outbox;
use astro_nostr_proto::{Event, kind};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::share_inbox::Prefetch;
use crate::sharing::Sharing;
use crate::upload_worker::Notify;

/// Étiquette (`["t", …]`) des messages de sauvegarde.
pub const TAG: &str = "astro-backup";
const CLAIM: &str = "backup_inbox";
const TRIES: &str = "backup_inbox_tries";
/// Fichiers envoyés au plus par passage et par compte.
const PER_PASS: usize = 100;
const LOOKBACK_SECS: i64 = 30 * 24 * 3600;
const BATCH: usize = 50;
const MAX_TRIES: u32 = 12;
const MAX_BYTES: u64 = 1 << 30;
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
    activity: ActivityBus,
    notify: Option<Notify>,
    prefetch: Option<Prefetch>,
}

impl<O: Outbox, B: BlobStore> PrivateBackup<O, B> {
    /// `sharing` fournit la liste des proches réciproques ; `station_peer` est indiqué à l'hébergeur pour rapatrier les blobs.
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, outbox: Arc<O>, blobs: Arc<B>, sharing: Arc<Sharing<O>>, activity: ActivityBus) -> Self {
        Self { db, keys, outbox, blobs, sharing, publisher: None, station_peer: None, activity, notify: None, prefetch: None }
    }

    /// Republie le manifeste de l'hébergeur après chaque rangement, et indique la station aux amis.
    pub fn with_station(mut self, publisher: Option<ManifestPublisher>, station_peer: Option<String>) -> Self {
        self.publisher = publisher;
        self.station_peer = station_peer;
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
            hosting_json.push(json!({ "owner": who, "label": self.label_of(&who).await, "status": status, "quota_bytes": quota, "used_bytes": used, "asked_at": at }));
        }
        let friends = self.sharing.friends(owner).await?.into_iter().map(|f| json!({ "hex": f.hex, "label": f.label })).collect::<Vec<_>>();
        Ok(json!({ "mine": mine, "hosting": hosting_json, "friends": friends }))
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
        const GONE: &str = "NOT EXISTS (SELECT 1 FROM ucloud_entries e WHERE e.owner_hex = backup_items.owner_hex AND e.cid = backup_items.cid AND e.path NOT LIKE '/Sauvegardes/%')";
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
            for (cid, size) in self.pending_files(&owner, &host, PER_PASS).await {
                if used.saturating_add(size) > quota {
                    continue; // trop gros pour le volume restant : les plus petits peuvent encore passer
                }
                let result = if local {
                    adopt_backup_blob(&self.db, &host, &owner, &label, &cid, size).await.map(|_| ()).map_err(|e| e.to_string())
                } else {
                    let mut tags = vec![vec!["cid".to_owned(), cid.clone()], vec!["size".into(), size.to_string()], vec!["op".into(), "item".into()]];
                    if let Some(peer) = &self.station_peer {
                        tags.push(vec!["station".into(), peer.clone()]);
                    }
                    self.send_to(&owner, &host, kind::FILE, tags, &format!("ipfs://{cid}")).await
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
            // Un blob chiffré à ranger.
            Some("item") if rumor.kind == kind::FILE => self.store_item(me, &sender, &wrap.id, &rumor, at, stats).await,
            _ => {
                self.claim(&wrap.id, at).await;
                stats.ignored += 1;
            }
        }
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
        Ok(format!("{total} fichier(s) privé(s) sauvegardé(s) chez des amis, {dropped} ancien(s) blob(s) retiré(s)"))
    }

    /// Travailleur de fond : réception des demandes, réponses et blobs jusqu'au signal d'arrêt. L'**envoi** n'a lieu
    /// qu'à 20h12 ([`Self::sync_night`]) : un lot par nuit plutôt qu'un envoi à chaque modification.
    pub async fn run(self: Arc<Self>, every: Duration, mut stop: watch::Receiver<bool>) {
        loop {
            self.poll_once(now()).await;
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
}
