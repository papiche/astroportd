//! Présence visio (« en ligne », « en appel ») entre amis mutuels, **sur option** (NIP-101, kind 24315).
//!
//! Un membre est « là » tant qu'un de ses onglets écoute le flux de la station ([`astro_core::ActivityBus::listen`]). S'il a
//! choisi d'être joignable (paramètre 31903 `d=visio`, `{"reachable": true}` : une option du membre vit dans Nostr, pas sur
//! disque), ses **amis mutuels** le voient : ceux de la station par leur flux en direct, ceux d'ailleurs par un battement
//! éphémère 24315 remis à leur station (contenu vide, statut `online`, `visio` ou `offline`, 50 destinataires au plus, toutes
//! les 4 minutes et à chaque changement). Un battement reçu n'est retenu que d'un ami mutuel du membre adressé, dans une
//! fenêtre d'horloge de ±120 s ; passé 6 minutes sans nouvelle, l'ami est hors ligne. Rien n'est stocké : la présence est un
//! **état en mémoire** qui se refait tout seul.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_db::Db;
use astro_nostr_proto::{Event, UnsignedEvent, is_hex64, kind};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

use crate::chat::DeliverFn;

/// Statuts reconnus (NIP-101).
pub const STATUSES: [&str; 4] = ["online", "visio", "away", "offline"];
/// Battement : toutes les 4 minutes.
pub const BEAT_SECS: u64 = 240;
/// Passé ce délai sans battement, l'ami est hors ligne.
pub const FRESH_SECS: i64 = 360;
/// Écart d'horloge toléré.
pub const CLOCK_SKEW: i64 = 120;
/// Destinataires d'un battement.
pub const MAX_P: usize = 50;
/// Paramètre applicatif (31903) du choix « être joignable ».
const D_TAG: &str = "visio";

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Service de présence d'une station.
pub struct Presence {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    activity: ActivityBus,
    deliver: DeliverFn,
    publish_local: DeliverFn,
    /// Ce que les amis d'ailleurs ont annoncé : `(membre, ami) → (statut, horodatage)`.
    remote: Mutex<HashMap<(String, String), (String, i64)>>,
    /// Membres en appel (statut `visio`).
    in_call: Mutex<HashSet<String>>,
}

impl Presence {
    /// `deliver` remet un battement à la station d'un ami ; `publish_local` publie le choix du membre sur le relai local.
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, activity: ActivityBus, deliver: DeliverFn, publish_local: DeliverFn) -> Self {
        Self { db, keys, activity, deliver, publish_local, remote: Mutex::default(), in_call: Mutex::default() }
    }

    async fn hosted(&self, hex: &str) -> bool {
        let h = hex.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [h], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false)
    }

    /// Le membre a-t-il choisi d'être joignable ? (son dernier 31903 `d=visio`)
    pub async fn reachable(&self, owner: &str) -> bool {
        let o = owner.to_owned();
        let content: Option<String> = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT e.content FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'd' AND t.value = ?2 \
                     WHERE e.kind = ?3 AND e.pubkey = ?1 ORDER BY e.created_at DESC LIMIT 1",
                    rusqlite::params![o, D_TAG, kind::APP_PARAMS],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await
            .ok()
            .flatten();
        content.and_then(|c| serde_json::from_str::<Value>(&c).ok()).is_some_and(|v| v["reachable"] == true)
    }

    /// Amis mutuels du membre (il les suit, et leur liste kind 3 le contient), 50 au plus.
    async fn mutual_friends(&self, owner: &str) -> Vec<String> {
        let o = owner.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let mut st = c.prepare_cached(
                    "SELECT g.pubkey FROM social_graph g WHERE g.owner_hex = ?1 AND g.level = 1 AND EXISTS \
                     (SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'p' AND t.value = ?1 WHERE e.kind = 3 AND e.pubkey = g.pubkey) \
                     ORDER BY g.pubkey LIMIT ?2",
                )?;
                Ok(st.query_map(rusqlite::params![o, MAX_P as i64], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    async fn mutual(&self, owner: &str, friend: &str) -> bool {
        self.mutual_friends(owner).await.iter().any(|f| f == friend)
    }

    /// Statut d'un membre de la station tel que ses amis le voient.
    fn status_of(&self, hex: &str) -> &'static str {
        if !self.activity.is_listening(hex) {
            "offline"
        } else if self.in_call.lock().unwrap_or_else(|e| e.into_inner()).contains(hex) {
            "visio"
        } else {
            "online"
        }
    }

    /// Annonce le statut du membre à ses amis mutuels : en direct pour ceux de la station, par un battement 24315 pour les autres.
    /// Un membre qui n'a pas choisi d'être joignable n'annonce rien (sauf `force_offline`, au moment où il retire son choix).
    pub async fn announce(&self, owner: &str, force_offline: bool) {
        let status = if force_offline { "offline" } else if self.reachable(owner).await { self.status_of(owner) } else { return };
        let friends = self.mutual_friends(owner).await;
        let mut remote = Vec::new();
        for f in friends {
            if self.hosted(&f).await {
                self.activity.publish_to(Audience::Owner(f), "presence", "presence", status, Some(json!({ "hex": owner, "status": status })));
            } else {
                remote.push(f);
            }
        }
        if remote.is_empty() {
            return;
        }
        let Some(secret) = self.keys.secret(owner) else { return };
        let mut tags: Vec<Vec<String>> = remote.iter().map(|f| vec!["p".to_owned(), f.clone()]).collect();
        tags.push(vec!["status".into(), status.into()]);
        let Ok(beat) = (UnsignedEvent { created_at: u64::try_from(now()).unwrap_or(0), kind: kind::VISIO_PRESENCE, tags, content: String::new() }).sign(&secret) else { return };
        // Le même battement (même identifiant) pour chaque station : un relai qui le reçoit deux fois ne le diffuse qu'une fois.
        for f in remote {
            if let Err(e) = (self.deliver)(f.clone(), beat.clone()).await {
                eprintln!("présence : battement non remis à {}… : {e}", &f[..8]);
            }
        }
    }

    /// Choix « être joignable » du membre : publié (31903 `d=visio`), annoncé aussitôt.
    pub async fn set_reachable(&self, owner: &str, on: bool) -> Result<Value, String> {
        let secret = self.keys.secret(owner).ok_or("clé du compte indisponible sur cette station")?;
        // Strictement après le réglage précédent : à date égale, un événement remplaçable peut être écarté (le plus petit
        // identifiant l'emporte), et « ne plus être joignable » serait perdu.
        let o = owner.to_owned();
        let last: i64 = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE(MAX(e.created_at), 0) FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'd' AND t.value = ?2 WHERE e.kind = ?3 AND e.pubkey = ?1",
                    rusqlite::params![o, D_TAG, kind::APP_PARAMS],
                    |r| r.get(0),
                )?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let ev = UnsignedEvent { created_at: u64::try_from(now().max(last + 1)).unwrap_or(0), kind: kind::APP_PARAMS, tags: vec![vec!["d".into(), D_TAG.into()]], content: json!({ "reachable": on }).to_string() }
            .sign(&secret)
            .map_err(|e| e.to_string())?;
        (self.publish_local)(owner.to_owned(), ev).await?;
        self.announce(owner, !on).await;
        Ok(json!({ "reachable": on }))
    }

    /// Le membre entre en appel ou en sort (statut `visio`).
    pub async fn set_in_call(&self, owner: &str, on: bool) {
        let changed = {
            let mut c = self.in_call.lock().unwrap_or_else(|e| e.into_inner());
            if on { c.insert(owner.to_owned()) } else { c.remove(owner) }
        };
        if changed {
            self.announce(owner, false).await;
        }
    }

    /// Ce que voit le membre : son choix, et le statut de chacun de ses amis mutuels joignables (`{ hex: statut }`).
    pub async fn view(&self, owner: &str) -> Value {
        let mut friends = serde_json::Map::new();
        let at = now();
        for f in self.mutual_friends(owner).await {
            let status = if self.hosted(&f).await {
                if self.reachable(&f).await { self.status_of(&f) } else { continue }
            } else {
                match self.remote.lock().unwrap_or_else(|e| e.into_inner()).get(&(owner.to_owned(), f.clone())) {
                    Some((s, t)) if at - t <= FRESH_SECS => if s == "away" { "online" } else { STATUSES.iter().find(|x| **x == s).copied().unwrap_or("offline") },
                    _ => "offline",
                }
            };
            friends.insert(f, json!(status));
        }
        json!({ "reachable": self.reachable(owner).await, "friends": friends })
    }

    /// Battement reçu par le relai de la station. Rend le nombre de membres avertis.
    pub async fn on_event(&self, ev: &Event) -> usize {
        if ev.kind != kind::VISIO_PRESENCE || !ev.content.is_empty() {
            return 0;
        }
        let statuses: Vec<&str> = ev.tags.iter().filter(|t| t.first().is_some_and(|n| n == "status")).filter_map(|t| t.get(1).map(String::as_str)).collect();
        let to: Vec<&str> = ev.tags.iter().filter(|t| t.first().is_some_and(|n| n == "p")).filter_map(|t| t.get(1).map(String::as_str)).collect();
        let at = i64::try_from(ev.created_at).unwrap_or(0);
        let ([status], 1..=MAX_P) = (statuses.as_slice(), to.len()) else { return 0 };
        if !STATUSES.contains(status) || (at - now()).abs() > CLOCK_SKEW || to.iter().any(|p| !is_hex64(p)) {
            return 0;
        }
        let mut told = 0;
        for member in to {
            if !self.hosted(member).await || !self.mutual(member, &ev.pubkey).await {
                continue;
            }
            let newer = {
                let mut r = self.remote.lock().unwrap_or_else(|e| e.into_inner());
                let key = (member.to_owned(), ev.pubkey.clone());
                if r.get(&key).is_some_and(|(_, t)| *t > at) {
                    false
                } else {
                    r.insert(key, ((*status).to_owned(), at));
                    true
                }
            };
            if newer {
                self.activity.publish_to(Audience::Owner(member.to_owned()), "presence", "presence", *status, Some(json!({ "hex": ev.pubkey, "status": status })));
                told += 1;
            }
        }
        told
    }

    /// Boucle : arrivées et départs des membres, battements reçus par le relai, battement toutes les 4 minutes.
    pub async fn run(self: Arc<Self>, mut relay: broadcast::Receiver<Arc<Event>>, mut stop: watch::Receiver<bool>) {
        let mut changes = self.activity.presence_changes();
        let mut tick = tokio::time::interval(Duration::from_secs(BEAT_SECS));
        loop {
            tokio::select! {
                c = changes.recv() => match c {
                    Ok((hex, _)) => if self.hosted(&hex).await { self.announce(&hex, false).await },
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                e = relay.recv() => match e {
                    Ok(ev) if ev.kind == kind::VISIO_PRESENCE => { self.on_event(&ev).await; }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = tick.tick() => {
                    let members: Vec<String> = self.db.readers().read(|c| Ok(c.prepare("SELECT hex FROM identities WHERE hosted = 1")?.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap_or_default();
                    for m in members.iter().filter(|m| self.activity.is_listening(m)) {
                        self.announce(m, false).await;
                    }
                }
                _ = stop.changed() => return,
            }
        }
    }
}
