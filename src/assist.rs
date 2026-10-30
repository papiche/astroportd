//! Demande d'assistance entre Capitaines amis : « aidez-moi sur ma machine ».
//!
//! Le Capitaine choisit un service (SSH, ComfyUI, Ollama), une durée, et sollicite **tous** ses Capitaines amis ou **un
//! seul** ; il peut ne retenir que le premier qui accepte (`exclusive`). La demande part en message NIP-59 (étiquette
//! `astro-assist`, `op` : `request`, `accept`, `decline`, `closed`, `cancel`). **Rien n'est ouvert à l'envoi** : l'accès n'est
//! accordé (par la [`TunnelPolicy`], avec son expiration) que lorsqu'un ami **accepte**. L'ami voit la demande dans son écran,
//! avec la station à joindre, et répond. Seuls les Capitaines amis réciproques sont écoutés ; une demande reste sans réponse
//! 48 h au plus.

use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_crypto::nip59::{self, Rumor, gift_wrap};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_facecloud::Outbox;
use astro_nostr_proto::{Event, kind};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::tunnel_policy::TunnelPolicy;
use crate::upload_worker::Notify;

/// Étiquette (`["t", …]`) des messages d'assistance.
pub const TAG: &str = "astro-assist";
const CLAIM: &str = "assist_inbox";
const LOOKBACK_SECS: i64 = 7 * 24 * 3600;
/// Une demande sans réponse expire au bout de 48 h.
const EXPIRY_SECS: i64 = 48 * 3600;
/// Durée maximale d'un accès demandé, en heures (7 jours).
const MAX_HOURS: i64 = 168;
/// Demandes en attente au plus par ami (contre le harcèlement).
const MAX_PENDING_PER_FRIEND: i64 = 5;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn tag<'a>(tags: &'a [Vec<String>], name: &str) -> Option<&'a str> {
    tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(String::as_str)
}

fn valid_service(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Les demandes d'assistance d'une station.
pub struct Assist<O: Outbox> {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    outbox: Arc<O>,
    policy: Arc<TunnelPolicy>,
    exposed: Vec<String>,
    station_peer: Option<String>,
    activity: ActivityBus,
    notify: Option<Notify>,
}

impl<O: Outbox> Assist<O> {
    /// `exposed` : les services que la station offre (SSH, ComfyUI, Ollama…) ; `station_peer` : son PeerID, indiqué à l'ami.
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, outbox: Arc<O>, policy: Arc<TunnelPolicy>, exposed: Vec<String>, station_peer: Option<String>, activity: ActivityBus) -> Self {
        Self { db, keys, outbox, policy, exposed, station_peer, activity, notify: None }
    }

    /// Prévient le Capitaine par un message privé de BRO.
    pub fn with_notify(mut self, notify: Option<Notify>) -> Self {
        self.notify = notify;
        self
    }

    async fn tell(&self, captain: &str, text: String) {
        self.activity.publish_to(Audience::Captain, "alert", "assist", text.clone(), None);
        if let Some(n) = &self.notify {
            n(captain.to_owned(), text).await;
        }
    }

    /// Clé Nostr du Capitaine hébergé ici.
    async fn captain(&self) -> Result<String, String> {
        self.db
            .readers()
            .read(|c| Ok(c.query_row("SELECT hex FROM identities WHERE role = 'captain' AND hosted = 1 LIMIT 1", [], |r| r.get::<_, String>(0)).optional()?))
            .await
            .ok()
            .flatten()
            .ok_or_else(|| "aucun Capitaine hébergé sur cette station".to_owned())
    }

    async fn send(&self, from: &str, to: &str, op: &str, mut tags: Vec<Vec<String>>) -> Result<(), String> {
        let secret = self.keys.secret(from).ok_or("clé du Capitaine indisponible")?;
        tags.splice(0..0, [vec!["p".to_owned(), to.to_owned()], vec!["t".into(), TAG.into()], vec!["op".into(), op.to_owned()]]);
        let at = u64::try_from(now()).unwrap_or(0);
        let rumor = Rumor { pubkey: from.to_owned(), created_at: at, kind: kind::CHAT, tags, content: "assistance".into() };
        let wrap = gift_wrap(&secret, to, &rumor, at).map_err(|_| "emballage NIP-59 impossible".to_owned())?;
        self.outbox.send(to, wrap).await.map_err(|e| e.to_string())
    }

    /// Demande de l'aide pour `service` pendant `hours` heures à tous les Capitaines amis, ou au seul `to`.
    /// `exclusive` : seul le premier qui accepte reçoit l'accès.
    pub async fn request(&self, service: &str, hours: i64, note: &str, to: Option<&str>, exclusive: bool) -> Result<Value, String> {
        if !valid_service(service) || !self.exposed.iter().any(|s| s == service) {
            return Err(format!("« {service} » n'est pas un service que cette station offre"));
        }
        if !(1..=MAX_HOURS).contains(&hours) {
            return Err(format!("durée : 1 à {MAX_HOURS} heures"));
        }
        let captain = self.captain().await?;
        let friends = self.policy.friends_full().await;
        let targets: Vec<&(String, String, String)> = match to {
            Some(hex) => vec![friends.iter().find(|f| f.0 == hex).ok_or("cet ami n'est pas un Capitaine ami réciproque")?],
            None => friends.iter().collect(),
        };
        if targets.is_empty() {
            return Err("aucun Capitaine ami réciproque à solliciter (suivez-vous mutuellement)".into());
        }
        let mut raw = [0u8; 8];
        getrandom::fill(&mut raw).map_err(|e| e.to_string())?;
        let id = hex::encode(raw);
        let note: String = note.chars().filter(|c| !c.is_control()).take(280).collect();
        let at = now();
        let mut sent = 0;
        let mut first_error = None;
        for (hex, _, _) in &targets {
            let (i, c, h, s, n) = (id.clone(), captain.clone(), hex.clone(), service.to_owned(), note.clone());
            self.db
                .writer()
                .execute(
                    "INSERT OR IGNORE INTO assist_requests(id, direction, captain, other, service, hours, note, exclusive, status, created_at) VALUES (?1, 'out', ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
                    vec![i.into(), c.into(), h.into(), s.into(), hours.into(), n.into(), i64::from(exclusive).into(), at.into()],
                )
                .await
                .map_err(|e| e.to_string())?;
            let mut tags = vec![vec!["id".to_owned(), id.clone()], vec!["service".into(), service.to_owned()], vec!["hours".into(), hours.to_string()], vec!["note".into(), note.clone()], vec!["exclusive".into(), u8::from(exclusive).to_string()]];
            if let Some(peer) = &self.station_peer {
                tags.push(vec!["station".into(), peer.clone()]);
            }
            match self.send(&captain, hex, "request", tags).await {
                Ok(()) => sent += 1,
                Err(e) => first_error = first_error.or(Some(e)),
            }
        }
        self.activity.publish_to(Audience::Captain, "system", "assist", format!("assistance demandée ({service}, {hours} h) à {sent} Capitaine(s) ami(s)"), None);
        Ok(json!({ "id": id, "sent": sent, "error": first_error }))
    }

    /// Réponse à une demande reçue : accepter (l'ami est prévenu et reçoit l'accès) ou refuser.
    pub async fn respond(&self, id: &str, other: &str, accept: bool) -> Result<Value, String> {
        let captain = self.captain().await?;
        let (i, o) = (id.to_owned(), other.to_owned());
        let row: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT status FROM assist_requests WHERE id = ?1 AND direction = 'in' AND other = ?2", [i, o], |r| r.get(0)).optional()?)).await.map_err(|e| e.to_string())?;
        match row.as_deref() {
            None => return Err("demande inconnue".into()),
            Some("pending") => {}
            Some(other_status) => return Err(format!("demande déjà traitée ({other_status})")),
        }
        let status = if accept { "accepted" } else { "declined" };
        let (i, o, t) = (id.to_owned(), other.to_owned(), now());
        self.db.writer().execute("UPDATE assist_requests SET status = ?4, decided_at = ?5 WHERE id = ?1 AND direction = 'in' AND other = ?2 AND captain = ?3", vec![i.into(), o.into(), captain.clone().into(), status.to_owned().into(), t.into()]).await.map_err(|e| e.to_string())?;
        self.send(&captain, other, if accept { "accept" } else { "decline" }, vec![vec!["id".to_owned(), id.to_owned()]]).await?;
        self.view().await
    }

    /// Demande reçue et acceptée : `(station à joindre, service, durée en heures)`. Seule une demande acceptée ouvre un accès local.
    pub async fn accepted_incoming(&self, id: &str) -> Option<(String, String, i64)> {
        let i = id.to_owned();
        self.db
            .readers()
            .read(move |c| {
                Ok(c.query_row("SELECT station, service, hours FROM assist_requests WHERE id = ?1 AND direction = 'in' AND status = 'accepted' AND station IS NOT NULL", [i], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).ok())
            })
            .await
            .ok()
            .flatten()
    }

    /// Retire les demandes encore sans réponse : les amis qui n'ont pas répondu n'ont plus rien à faire.
    pub async fn cancel(&self, id: &str) -> Result<Value, String> {
        let captain = self.captain().await?;
        let i = id.to_owned();
        let pending: Vec<String> = self
            .db
            .writer()
            .transaction(move |tx| {
                let mut stmt = tx.prepare("SELECT other FROM assist_requests WHERE id = ?1 AND direction = 'out' AND status = 'pending'")?;
                let rows = stmt.query_map([&i], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                drop(stmt);
                tx.execute("UPDATE assist_requests SET status = 'closed', decided_at = ?2 WHERE id = ?1 AND direction = 'out' AND status = 'pending'", rusqlite::params![i, now()])?;
                Ok(rows)
            })
            .await
            .map_err(|e| e.to_string())?;
        for other in pending {
            let _ = self.send(&captain, &other, "cancel", vec![vec!["id".to_owned(), id.to_owned()]]).await;
        }
        self.view().await
    }

    /// Écran : amis sollicitables, demandes reçues, demandes envoyées.
    pub async fn view(&self) -> Result<Value, String> {
        let friends = self.policy.friends_full().await;
        let label = |hex: &str| friends.iter().find(|f| f.0 == hex).map_or_else(|| hex.chars().take(8).collect::<String>(), |f| f.2.clone());
        type Row = (String, String, String, String, i64, String, i64, Option<String>, String, i64);
        let rows: Vec<Row> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT id, direction, other, service, hours, note, exclusive, station, status, created_at FROM assist_requests ORDER BY created_at DESC LIMIT 100")?;
                let mut out = Vec::new();
                let mut q = stmt.query([])?;
                while let Some(r) = q.next()? {
                    out.push((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?));
                }
                Ok(out)
            })
            .await
            .map_err(|e| e.to_string())?;
        let (mut incoming, mut outgoing): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
        for (id, direction, other, service, hours, note, exclusive, station, status, created_at) in rows {
            if direction == "in" {
                incoming.push(json!({ "id": id, "other": other, "from": label(&other), "service": service, "hours": hours, "note": note, "station": station, "status": status, "created_at": created_at }));
            } else if let Some(existing) = outgoing.iter_mut().find(|o| o["id"] == id) {
                if let Some(r) = existing["recipients"].as_array_mut() {
                    r.push(json!({ "label": label(&other), "status": status }));
                }
            } else {
                outgoing.push(json!({ "id": id, "service": service, "hours": hours, "note": note, "exclusive": exclusive == 1, "created_at": created_at, "recipients": [{ "label": label(&other), "status": status }] }));
            }
        }
        Ok(json!({ "services": self.exposed, "friends": friends.iter().map(|f| json!({ "hex": f.0, "label": f.2 })).collect::<Vec<_>>(), "incoming": incoming, "outgoing": outgoing }))
    }

    /// Un passage de réception sur les demandes et réponses adressées au Capitaine.
    pub async fn poll_once(&self, at: i64) -> usize {
        let Ok(captain) = self.captain().await else { return 0 };
        let Some(secret) = self.keys.secret(&captain) else { return 0 };
        let _ = self.db.writer().execute("UPDATE assist_requests SET status = 'expired' WHERE status = 'pending' AND created_at < ?1", vec![(at - EXPIRY_SECS).into()]).await;
        let (c, since) = (captain.clone(), at - LOOKBACK_SECS);
        let wraps: Vec<Event> = self
            .db
            .readers()
            .read(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e \
                     JOIN nostr_tags t ON t.event_id = e.id AND t.name = 'p' AND t.value = ?1 \
                     WHERE e.kind = 1059 AND e.created_at >= ?2 \
                       AND NOT EXISTS (SELECT 1 FROM claimed c WHERE c.owner = 'assist_inbox' AND c.event_id = e.id) \
                     ORDER BY e.created_at LIMIT 50",
                )?;
                let mut rows = stmt.query(rusqlite::params![c, since])?;
                let mut out = Vec::new();
                while let Some(r) = rows.next()? {
                    out.push(row_to_event(r)?);
                }
                Ok(out)
            })
            .await
            .unwrap_or_default();
        let mut handled = 0;
        for wrap in wraps {
            let _ = self.db.writer().execute("INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES (?1, ?2, ?3)", vec![CLAIM.to_owned().into(), wrap.id.clone().into(), at.into()]).await;
            let Ok(opened) = nip59::unwrap(&secret, &wrap) else { continue };
            if opened.sender == captain || tag(&opened.rumor.tags, "t") != Some(TAG) {
                continue;
            }
            if self.handle(&captain, &opened.sender, &opened.rumor, at).await {
                handled += 1;
            }
        }
        handled
    }

    async fn handle(&self, me: &str, sender: &str, rumor: &Rumor, at: i64) -> bool {
        let friends = self.policy.friends_full().await;
        let Some((_, friend_peer, friend_label)) = friends.iter().find(|f| f.0 == sender).cloned() else { return false };
        let Some(id) = tag(&rumor.tags, "id").filter(|i| i.len() == 16 && i.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_owned) else { return false };
        match tag(&rumor.tags, "op") {
            // Un ami me demande de l'aide.
            Some("request") => {
                let service = tag(&rumor.tags, "service").unwrap_or("");
                let hours = tag(&rumor.tags, "hours").and_then(|h| h.parse::<i64>().ok()).unwrap_or(0);
                if !valid_service(service) || !(1..=MAX_HOURS).contains(&hours) {
                    return false;
                }
                let (m, s) = (me.to_owned(), sender.to_owned());
                let pending: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM assist_requests WHERE direction = 'in' AND captain = ?1 AND other = ?2 AND status = 'pending'", [m, s], |r| r.get(0))?)).await.unwrap_or(0);
                if pending >= MAX_PENDING_PER_FRIEND {
                    return false;
                }
                let note: String = tag(&rumor.tags, "note").unwrap_or("").chars().filter(|c| !c.is_control()).take(280).collect();
                let exclusive = tag(&rumor.tags, "exclusive") == Some("1");
                let station = tag(&rumor.tags, "station").map(|s| s.chars().take(80).collect::<String>());
                let res = self
                    .db
                    .writer()
                    .execute(
                        "INSERT OR IGNORE INTO assist_requests(id, direction, captain, other, service, hours, note, exclusive, station, status, created_at) VALUES (?1, 'in', ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9)",
                        vec![id.into(), me.to_owned().into(), sender.to_owned().into(), service.to_owned().into(), hours.into(), note.clone().into(), i64::from(exclusive).into(), station.map_or(rusqlite::types::Value::Null, Into::into), at.into()],
                    )
                    .await;
                if res.is_ok_and(|r| r.changes > 0) {
                    self.tell(me, format!("🆘 {friend_label} demande de l'aide ({service}, {hours} h){}. Répondez dans Options › Tunnels › Assistance.", if note.is_empty() { String::new() } else { format!(" : « {note} »") })).await;
                }
                true
            }
            // Un ami répond à ma demande.
            Some(op @ ("accept" | "decline")) => {
                let (i, s) = (id.clone(), sender.to_owned());
                let row: Option<(String, i64, i64, String)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT service, hours, exclusive, status FROM assist_requests WHERE id = ?1 AND direction = 'out' AND other = ?2", [i, s], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?)).await.ok().flatten();
                let Some((service, hours, exclusive, status)) = row else { return false };
                if status != "pending" {
                    return false;
                }
                if op == "decline" {
                    let _ = self.db.writer().execute("UPDATE assist_requests SET status = 'declined', decided_at = ?3 WHERE id = ?1 AND direction = 'out' AND other = ?2", vec![id.into(), sender.to_owned().into(), at.into()]).await;
                    self.tell(me, format!("{friend_label} ne peut pas vous aider pour l'instant.")).await;
                    return true;
                }
                // Mode « le premier seulement » : un autre ami a déjà accepté, celui-ci est remercié sans accès.
                let i = id.clone();
                let taken: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM assist_requests WHERE id = ?1 AND direction = 'out' AND status = 'accepted'", [i], |r| r.get(0))?)).await.unwrap_or(0);
                if exclusive == 1 && taken > 0 {
                    let _ = self.db.writer().execute("UPDATE assist_requests SET status = 'closed', decided_at = ?3 WHERE id = ?1 AND direction = 'out' AND other = ?2", vec![id.clone().into(), sender.to_owned().into(), at.into()]).await;
                    let _ = self.send(me, sender, "closed", vec![vec!["id".to_owned(), id]]).await;
                    return true;
                }
                match self.policy.set(&json!({ "service": service, "grant": { "peer": friend_peer, "label": friend_label, "hours": hours } }), &self.exposed, at).await {
                    Ok(_) => {
                        let _ = self.db.writer().execute("UPDATE assist_requests SET status = 'accepted', decided_at = ?3 WHERE id = ?1 AND direction = 'out' AND other = ?2", vec![id.clone().into(), sender.to_owned().into(), at.into()]).await;
                        if exclusive == 1 {
                            // Les autres sollicités n'ont plus rien à faire.
                            let i = id.clone();
                            let others: Vec<String> = self.db.readers().read(move |c| Ok(c.prepare("SELECT other FROM assist_requests WHERE id = ?1 AND direction = 'out' AND status = 'pending'")?.query_map([i], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?)).await.unwrap_or_default();
                            for o in others {
                                let _ = self.send(me, &o, "closed", vec![vec!["id".to_owned(), id.clone()]]).await;
                                let _ = self.db.writer().execute("UPDATE assist_requests SET status = 'closed', decided_at = ?3 WHERE id = ?1 AND direction = 'out' AND other = ?2", vec![id.clone().into(), o.into(), at.into()]).await;
                            }
                        }
                        self.tell(me, format!("✅ {friend_label} accepte de vous aider : {service} ouvert pour lui pendant {hours} h.")).await;
                    }
                    Err(e) => self.tell(me, format!("{friend_label} accepte, mais l'accès {service} n'a pas pu être ouvert : {e}")).await,
                }
                true
            }
            // L'ami qui demandait n'a plus besoin de moi.
            Some("closed" | "cancel") => {
                let _ = self.db.writer().execute("UPDATE assist_requests SET status = 'closed', decided_at = ?3 WHERE id = ?1 AND direction = 'in' AND other = ?2 AND status = 'pending'", vec![id.into(), sender.to_owned().into(), at.into()]).await;
                true
            }
            _ => false,
        }
    }

    /// Travailleur de fond : réception des demandes et réponses jusqu'au signal d'arrêt.
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
    use astro_db::events::insert_event;
    use astro_facecloud::OutboxError;
    use astro_nostr_proto::{UnsignedEvent, pubkey_of};

    use super::*;

    const ME: [u8; 32] = [0xA1; 32];
    const LEA: [u8; 32] = [0xB2; 32];
    const MAX: [u8; 32] = [0xC3; 32];
    const STRANGER: [u8; 32] = [0xD4; 32];
    const LEA_PEER: &str = "12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG";
    const MAX_PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTf";

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

    async fn rig(tag: &str) -> (std::path::PathBuf, Arc<Db>, Arc<Capture>, Arc<TunnelPolicy>, Assist<Capture>) {
        let dir = std::env::temp_dir().join(format!("astroportd-assist-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'captain', 1)", vec![hex(&ME).into()]).await.unwrap();
        for (secret, peer) in [(&LEA, LEA_PEER), (&MAX, MAX_PEER)] {
            db.writer().execute("INSERT INTO identities(hex, role, hosted, home_station, email) VALUES (?1, 'captain', 0, ?2, ?3)", vec![hex(secret).into(), format!("{peer}:{}", "11".repeat(32)).into(), format!("{}@example.org", if secret == &LEA { "lea" } else { "max" }).into()]).await.unwrap();
            db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ME).into(), hex(secret).into()]).await.unwrap();
            let follows = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(&ME)]], content: String::new() }.sign(secret).unwrap();
            db.writer().transaction(move |tx| insert_event(tx, &follows).map(|_| ())).await.unwrap();
        }
        let bus = ActivityBus::new();
        let policy = TunnelPolicy::load(db.clone(), false, None, bus.clone()).await;
        let outbox = Arc::new(Capture::default());
        let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ME));
        let assist = Assist::new(db.clone(), keys, outbox.clone(), policy.clone(), vec!["ssh".into(), "ollama".into()], Some("12D3KooWMyStation".into()), bus);
        (dir, db, outbox, policy, assist)
    }

    async fn deliver(db: &Db, from: &[u8; 32], to: &[u8; 32], op: &str, id: &str, extra: Vec<Vec<String>>) {
        let at = u64::try_from(now()).unwrap();
        let mut tags = vec![vec!["t".to_owned(), TAG.to_owned()], vec!["op".into(), op.to_owned()], vec!["id".into(), id.to_owned()]];
        tags.extend(extra);
        let rumor = Rumor { pubkey: hex(from), created_at: at, kind: kind::CHAT, tags, content: String::new() };
        let wrap = gift_wrap(from, &hex(to), &rumor, at).unwrap();
        db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
    }

    fn sent_ops(outbox: &Capture, secret: &[u8; 32]) -> Vec<(String, String)> {
        outbox.0.lock().unwrap().iter().filter(|(to, _)| *to == hex(secret)).map(|(_, w)| {
            let r = nip59::unwrap(secret, w).unwrap().rumor;
            (tag(&r.tags, "op").unwrap().to_owned(), tag(&r.tags, "id").unwrap().to_owned())
        }).collect()
    }

    #[tokio::test]
    async fn nothing_opens_until_a_friend_accepts_and_only_the_first_one_gets_access_when_exclusive() {
        let (dir, db, outbox, policy, assist) = rig("ask").await;
        assert!(assist.request("gpu", 2, "", None, false).await.is_err(), "service non offert");
        assert!(assist.request("ssh", 0, "", None, false).await.is_err() && assist.request("ssh", 999, "", None, false).await.is_err());
        let r = assist.request("ssh", 2, "ma carte ne démarre plus", None, true).await.unwrap();
        assert_eq!(r["sent"], 2);
        let id = r["id"].as_str().unwrap().to_owned();
        assert_eq!(sent_ops(&outbox, &LEA), [("request".to_owned(), id.clone())]);
        assert!(!policy.permits(LEA_PEER, "ssh", now()).await && !policy.permits(MAX_PEER, "ssh", now()).await, "rien n'est ouvert à l'envoi");

        // Léa accepte la première : accès pour elle seule ; Max, ensuite, est remercié sans accès.
        deliver(&db, &LEA, &ME, "accept", &id, vec![]).await;
        assert_eq!(assist.poll_once(now()).await, 1);
        assert!(policy.permits(LEA_PEER, "ssh", now()).await && !policy.permits(MAX_PEER, "ssh", now() ).await);
        assert!(!policy.permits(LEA_PEER, "ssh", now() + 3 * 3600).await, "l'accès expire après la durée demandée");
        assert!(sent_ops(&outbox, &MAX).iter().any(|(op, i)| op == "closed" && *i == id), "Max est prévenu que c'est réglé");
        deliver(&db, &MAX, &ME, "accept", &id, vec![]).await;
        assist.poll_once(now()).await;
        assert!(!policy.permits(MAX_PEER, "ssh", now()).await);

        // Un inconnu ne peut rien : sa réponse est ignorée.
        deliver(&db, &STRANGER, &ME, "accept", &id, vec![]).await;
        assert_eq!(assist.poll_once(now()).await, 0);
        let v = assist.view().await.unwrap();
        let statuses: Vec<&str> = v["outgoing"][0]["recipients"].as_array().unwrap().iter().filter_map(|r| r["status"].as_str()).collect();
        assert!(statuses.contains(&"accepted") && statuses.contains(&"closed"), "{statuses:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_request_from_a_friend_waits_for_the_captain_and_strangers_are_ignored() {
        let (dir, db, outbox, _, assist) = rig("help").await;
        let ask = |n: &str| vec![vec!["service".to_owned(), "ssh".into()], vec!["hours".into(), "4".into()], vec!["note".into(), n.to_owned()], vec!["station".into(), LEA_PEER.into()]];
        deliver(&db, &STRANGER, &ME, "request", "aaaaaaaaaaaaaaaa", ask("hello")).await;
        deliver(&db, &LEA, &ME, "request", "bbbbbbbbbbbbbbbb", ask("aide-moi")).await;
        deliver(&db, &LEA, &ME, "request", "cccccccccccccccc", vec![vec!["service".to_owned(), "../x".into()], vec!["hours".into(), "4".into()]]).await;
        assert_eq!(assist.poll_once(now()).await, 1, "ni l'inconnu, ni la demande mal formée");
        let v = assist.view().await.unwrap();
        assert_eq!(v["incoming"].as_array().unwrap().len(), 1);
        assert_eq!((v["incoming"][0]["from"].as_str(), v["incoming"][0]["status"].as_str(), v["incoming"][0]["note"].as_str()), (Some("lea"), Some("pending"), Some("aide-moi")));
        assert!(sent_ops(&outbox, &LEA).is_empty(), "aucune réponse automatique");

        let v = assist.respond("bbbbbbbbbbbbbbbb", &hex(&LEA), true).await.unwrap();
        assert_eq!(v["incoming"][0]["status"], "accepted");
        assert_eq!(sent_ops(&outbox, &LEA), [("accept".to_owned(), "bbbbbbbbbbbbbbbb".to_owned())]);
        assert!(assist.respond("bbbbbbbbbbbbbbbb", &hex(&LEA), false).await.is_err(), "déjà traitée");
        assert!(assist.respond("zzzz", &hex(&LEA), true).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
