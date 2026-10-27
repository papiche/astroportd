//! Courtier autonome (Proactive Broker) : une station **sans GPU** réserve elle-même du calcul quand sa file s'allonge.
//!
//! Règle : si des tâches GPU (`gpu`, `comfyui`, `ollama`, `faceid`…) attendent et que le délai d'attente moyen estimé
//! par Erlang-C ([`astro_core::telemetry`]) dépasse **30 s**, la station choisit le Brain le plus proche qui offre le
//! service, lit son tarif publié, et signe un virement Ẑen `t=lease` d'**1 semaine** de part GPU (`gpu_1_24`). Le
//! virement part par le relai local et les relais de la constellation ; le bail devient actif quand le Brain le
//! projette, et le tunnel P2P s'ouvre alors tout seul.
//!
//! Garde-fous : jamais avec un GPU local ; un seul achat par période de refroidissement ; prix unitaire plafonné ;
//! chaque refus (aucun Brain, tarif absent ou trop cher, publication échouée) est dit sur le bus d'activité, pas
//! avalé. Le solde n'est pas vérifié ici : le registre Ğ1-N² du Brain refuse un virement à découvert.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_core::telemetry::{CRITICAL_WAIT_SECS, DEFAULT_SERVICE_SECS, LoadModel, TrafficReport, estimate_load_for};
use astro_db::Db;
use astro_economy::Amount;
use astro_economy::lease::{LeaseRequest, Resource};
use astro_nostr_proto::{Event, UnsignedEvent, kind, pubkey_of};
use astro_p2p::routing::{Candidate, Mode, select};
use rusqlite::params;
use tokio::sync::watch;
use zeroize::Zeroizing;

use crate::BroError;

/// Réglages du courtier.
#[derive(Debug, Clone)]
pub struct BrokerConfig {
    /// Natures de tâches (préfixes de `tasks.kind`) qui demandent du GPU.
    pub gpu_kinds: Vec<String>,
    /// Service DRAGON à acheter (`comfyui`, `ollama`).
    pub service: String,
    /// Délai moyen au-delà duquel on réserve, en secondes.
    pub critical_wait_secs: f64,
    /// Durée moyenne d'une tâche GPU, en secondes.
    pub mean_service_secs: f64,
    /// Semaines réservées par achat (une part hebdomadaire donne 1 h de calcul par jour).
    pub weeks: u32,
    /// Prix unitaire maximal accepté, en centimes de Ẑen par semaine.
    pub max_unit_price_cents: u64,
    /// Pause entre deux achats, en secondes.
    pub cooldown_secs: u64,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            gpu_kinds: ["gpu", "comfyui", "ollama", "faceid"].map(String::from).to_vec(),
            service: "comfyui".into(),
            critical_wait_secs: CRITICAL_WAIT_SECS,
            mean_service_secs: DEFAULT_SERVICE_SECS,
            weeks: 1,
            max_unit_price_cents: 3_000,
            cooldown_secs: 600,
        }
    }
}

/// Publie un événement signé (relai local, relais de la constellation).
pub trait Publisher: Send + Sync + 'static {
    /// Publie `event`.
    fn publish(&self, event: Event) -> impl Future<Output = Result<(), String>> + Send;
}

/// Ce que la station observe d'elle-même.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// Tâches GPU en attente.
    pub waiting: u32,
    /// Tâches GPU en cours.
    pub running: u32,
    /// Parts de GPU actuellement louées par cette station.
    pub leased_shares: u32,
}

/// Décision d'un passage.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Rien à faire (pas de file, ou GPU local).
    Idle,
    /// Réservation émise.
    Bought {
        /// Identifiant du virement.
        event_id: String,
        /// Clé du Brain.
        node_hex: String,
        /// Coût total en centimes de Ẑen.
        cost_cents: u64,
    },
    /// La file est critique mais aucun achat n'a été fait (raison lisible).
    Skipped(String),
}

/// Source des pairs connus et des coordonnées de la station : `(pairs, (latitude, longitude))`.
pub type SwarmSource = Arc<dyn Fn() -> (Vec<Candidate>, Option<(f64, f64)>) + Send + Sync>;

/// Le courtier.
pub struct Broker<P> {
    db: Arc<Db>,
    secret: Zeroizing<[u8; 32]>,
    node_hex: String,
    has_local_gpu: bool,
    config: BrokerConfig,
    publisher: Arc<P>,
    activity: Option<ActivityBus>,
}

fn sql_kinds(kinds: &[String]) -> (String, Vec<String>) {
    // `kind = ?` ou `kind LIKE ?||'.%'` pour chaque préfixe ; les valeurs sont liées, jamais concaténées.
    let clause = (0..kinds.len()).map(|i| format!("kind = ?{n} OR kind LIKE ?{n} || '.%'", n = i + 1)).collect::<Vec<_>>().join(" OR ");
    (if clause.is_empty() { "0".to_owned() } else { clause }, kinds.to_vec())
}

impl<P: Publisher> Broker<P> {
    /// Courtier de la station dont la clé Nostr est `secret`. `has_local_gpu` : un GPU local désactive l'achat.
    pub fn new(db: Arc<Db>, secret: [u8; 32], has_local_gpu: bool, config: BrokerConfig, publisher: Arc<P>, activity: Option<ActivityBus>) -> Result<Self, BroError> {
        let node_hex = pubkey_of(&secret).map_err(|e| BroError::Model(format!("clé de station invalide : {e}")))?;
        Ok(Self { db, secret: Zeroizing::new(secret), node_hex, has_local_gpu, config, publisher, activity })
    }

    fn say(&self, message: String) {
        if let Some(bus) = &self.activity {
            bus.publish("p2p", "bro:broker", message, None);
        }
    }

    /// Lit la file de tâches et les baux de la station.
    pub async fn observe(&self, now: u64) -> Result<Observation, BroError> {
        let (clause, binds) = sql_kinds(&self.config.gpu_kinds);
        let (node, now_i) = (self.node_hex.clone(), i64::try_from(now).unwrap_or(i64::MAX));
        let (waiting, running, leased) = self
            .db
            .readers()
            .read(move |c| {
                let count = |status: &str| -> rusqlite::Result<i64> {
                    let sql = format!("SELECT COUNT(*) FROM tasks WHERE status = '{status}' AND ({clause})");
                    c.query_row(&sql, rusqlite::params_from_iter(binds.iter()), |r| r.get(0))
                };
                let leased: i64 = c.query_row(
                    "SELECT COUNT(*) FROM resource_leases WHERE beneficiary = ?1 AND resource = 'gpu_1_24' AND status = 'active' AND start_at <= ?2 AND end_at > ?2",
                    params![node, now_i],
                    |r| r.get(0),
                )?;
                Ok((count("pending")?, count("running")?, leased))
            })
            .await?;
        let u = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
        Ok(Observation { waiting: u(waiting), running: u(running), leased_shares: u(leased) })
    }

    /// Délai d'attente estimé (Erlang-C) pour cette observation.
    pub fn estimate(&self, o: &Observation) -> TrafficReport {
        estimate_load_for(o.running, o.waiting, LoadModel { servers: o.leased_shares, mean_service_secs: self.config.mean_service_secs })
    }

    /// Un passage : observe, estime, et réserve si nécessaire. `candidates` : les pairs connus (leurs `12345.json`).
    pub async fn tick(&self, now: u64, candidates: &[Candidate], origin: Option<(f64, f64)>) -> Result<Decision, BroError> {
        if self.has_local_gpu {
            return Ok(Decision::Idle);
        }
        let obs = self.observe(now).await?;
        let report = self.estimate(&obs);
        if obs.waiting == 0 || !report.wait_exceeds(self.config.critical_wait_secs) {
            return Ok(Decision::Idle);
        }
        let why = |reason: String| {
            self.say(format!("file GPU critique ({} en attente, délai estimé {}) mais pas d'achat : {reason}", obs.waiting, fmt_wait(report.mean_wait_secs)));
            Ok(Decision::Skipped(reason))
        };
        if let Some(last) = self.last_purchase().await? {
            if now < last + self.config.cooldown_secs {
                return why(format!("achat récent (il y a {} s)", now.saturating_sub(last)));
            }
        }
        let Some(pick) = select(candidates, &self.config.service, Mode::Closest, origin, 0) else {
            return why(format!("aucun Brain ne fournit « {} »", self.config.service));
        };
        let node_id = pick.node.node_id.clone();
        let (dest, price) = match self.brain_terms(&node_id).await? {
            Some(t) => t,
            None => return why(format!("le Brain …{} n'a pas de clé connue ou de tarif gpu_1_24 publié", tail(&node_id))),
        };
        if price.0 > self.config.max_unit_price_cents {
            return why(format!("tarif de …{} trop élevé ({} > {} centimes/sem.)", tail(&node_id), price.0, self.config.max_unit_price_cents));
        }
        let request = LeaseRequest { resource: Resource::Gpu1of24, start: now, units: self.config.weeks, unit_price: price };
        let cost = price.0.saturating_mul(u64::from(self.config.weeks));
        let event = self.purchase_event(&dest, &request, Amount(cost), now).await?;
        let id = event.id.clone();
        match self.publisher.publish(event).await {
            Ok(()) => {
                self.remember_purchase(now).await?;
                self.say(format!("achat autonome : {} sem. de GPU 1/24 chez …{} pour {} Ẑ (délai estimé {}, {} tâche(s) en attente)", self.config.weeks, tail(&node_id), Amount(cost), fmt_wait(report.mean_wait_secs), obs.waiting));
                Ok(Decision::Bought { event_id: id, node_hex: dest, cost_cents: cost })
            }
            Err(e) => why(format!("publication du virement impossible : {e}")),
        }
    }

    /// Clé et prix du Brain `peer_id`, s'ils sont connus.
    async fn brain_terms(&self, peer_id: &str) -> Result<Option<(String, Amount)>, BroError> {
        let peer = peer_id.to_owned();
        let row: Option<(String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                Ok(rusqlite::OptionalExtension::optional(c.query_row(
                    "SELECT i.hex, t.price_cents FROM identities i JOIN tariffs t ON t.node_hex = i.hex AND t.resource = 'gpu_1_24' WHERE i.peer_id = ?1 LIMIT 1",
                    [peer],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ))?)
            })
            .await?;
        Ok(row.map(|(hex, p)| (hex, Amount(u64::try_from(p).unwrap_or(0)))))
    }

    /// Virement Ğ1-N² signé portant le bail. `prev` : dernier virement de cette station dans le registre.
    async fn purchase_event(&self, dest: &str, req: &LeaseRequest, paid: Amount, now: u64) -> Result<Event, BroError> {
        let me = self.node_hex.clone();
        let prev: String = self
            .db
            .readers()
            .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT event_id FROM ledger_n2 WHERE author = ?1 ORDER BY created_at DESC, event_id LIMIT 1", [me], |r| r.get::<_, String>(0)))?))
            .await?
            .unwrap_or_else(|| "genesis".to_owned());
        let mut tags = vec![
            vec!["d".into(), format!("lease-auto-{now}-{}", &dest[..8.min(dest.len())])],
            vec!["p".into(), dest.to_owned()],
            vec!["amount".into(), paid.to_string()],
            vec!["prev".into(), prev],
            vec!["t".into(), "g1-n2".into()],
        ];
        tags.extend(req.to_tags());
        UnsignedEvent { created_at: now, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(&self.secret).map_err(|e| BroError::Model(format!("signature impossible : {e}")))
    }

    async fn last_purchase(&self) -> Result<Option<u64>, BroError> {
        Ok(self
            .db
            .readers()
            .read(|c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = 'broker' AND key = 'last_purchase'", [], |r| r.get::<_, String>(0)))?))
            .await?
            .and_then(|v| v.parse().ok()))
    }

    async fn remember_purchase(&self, now: u64) -> Result<(), BroError> {
        self.db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES ('broker', 'last_purchase', ?1, ?1) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![rusqlite::types::Value::Integer(i64::try_from(now).unwrap_or(0))],
            )
            .await?;
        Ok(())
    }

    /// Boucle de service : un passage toutes les `every`. `swarm` lit les pairs et les coordonnées de la station.
    pub async fn run(self: Arc<Self>, every: Duration, swarm: SwarmSource, mut stop: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.changed() => return,
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let (candidates, origin) = swarm();
            if let Err(e) = self.tick(now, &candidates, origin).await {
                self.say(format!("courtier : passage impossible ({e})"));
            }
        }
    }
}

fn tail(id: &str) -> String {
    id.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect()
}

fn fmt_wait(secs: f64) -> String {
    if secs.is_finite() { format!("{secs:.0} s") } else { "infini".to_owned() }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rusqlite::types::Value as V;

    use super::*;
    use crate::testkit::temp_db;

    const ME: [u8; 32] = [7; 32];
    const BRAIN: [u8; 32] = [1; 32];
    const NOW: u64 = 1_800_000_000;

    #[derive(Default)]
    struct Sink(Mutex<Vec<Event>>);
    impl Publisher for Sink {
        async fn publish(&self, e: Event) -> Result<(), String> {
            self.0.lock().unwrap().push(e);
            Ok(())
        }
    }

    struct Broken;
    impl Publisher for Broken {
        async fn publish(&self, _: Event) -> Result<(), String> {
            Err("aucun relai joignable".into())
        }
    }

    fn brain(id: &str, power: f64, pos: (f64, f64)) -> Candidate {
        Candidate { node_id: id.into(), power_score: power, lat: Some(pos.0), lon: Some(pos.1), services: vec!["comfyui".into()] }
    }

    async fn queue(db: &Db, kind: &str, status: &str, n: usize) {
        for _ in 0..n {
            db.writer().execute("INSERT INTO tasks(kind, payload, status, created_at) VALUES (?1, '{}', ?2, 0)", vec![V::Text(kind.into()), V::Text(status.into())]).await.unwrap();
        }
    }

    async fn known_brain(db: &Db, peer: &str, secret: &[u8; 32], price: i64) {
        let hex = pubkey_of(secret).unwrap();
        db.writer().execute("INSERT INTO identities(hex, role, peer_id, hosted) VALUES (?1, 'node', ?2, 0)", vec![V::Text(hex.clone()), V::Text(peer.into())]).await.unwrap();
        db.writer().execute("INSERT INTO tariffs(node_hex, resource, price_cents, unit, effective_at) VALUES (?1, 'gpu_1_24', ?2, 'hour', 1)", vec![V::Text(hex), V::Integer(price)]).await.unwrap();
    }

    fn broker(db: Arc<Db>, gpu: bool) -> (Broker<Sink>, Arc<Sink>) {
        let sink = Arc::new(Sink::default());
        (Broker::new(db, ME, gpu, BrokerConfig::default(), sink.clone(), None).unwrap(), sink)
    }

    #[tokio::test]
    async fn a_long_queue_without_gpu_buys_one_hour_from_the_closest_brain() {
        let (db, dir) = temp_db("broker");
        known_brain(&db, "12D3KooWnear", &BRAIN, 50).await;
        queue(&db, "comfyui.image", "pending", 3).await;
        queue(&db, "bro.upload", "pending", 9).await; // pas du GPU : ignoré
        let (b, sink) = broker(db.clone(), false);
        let obs = b.observe(NOW).await.unwrap();
        assert_eq!(obs, Observation { waiting: 3, running: 0, leased_shares: 0 });
        assert!(b.estimate(&obs).mean_wait_secs.is_infinite(), "sans part de GPU la file ne se vide jamais");

        let swarm = [brain("12D3KooWfar", 200.0, (48.85, 2.35)), brain("12D3KooWnear", 90.0, (45.7, 4.8))];
        let d = b.tick(NOW, &swarm, Some((45.76, 4.83))).await.unwrap();
        let Decision::Bought { event_id, node_hex, cost_cents } = d else { unreachable!("{d:?}") };
        assert_eq!((node_hex.as_str(), cost_cents), (pubkey_of(&BRAIN).unwrap().as_str(), 50), "le plus proche, pas le plus puissant");
        let sent = sink.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        let e = &sent[0];
        e.verify().unwrap();
        assert_eq!((e.id.as_str(), e.kind, e.pubkey.as_str()), (event_id.as_str(), kind::LEDGER_N2, pubkey_of(&ME).unwrap().as_str()));
        assert_eq!((e.tag_value("p"), e.tag_value("amount"), e.tag_value("prev")), (Some(pubkey_of(&BRAIN).unwrap().as_str()), Some("0.50"), Some("genesis")));
        let req = LeaseRequest::from_tags(&e.tags, e.created_at).unwrap();
        assert_eq!((req.resource, req.units, req.unit_price, req.start), (Resource::Gpu1of24, 1, Amount(50), NOW));
        assert!(LeaseRequest::is_lease(&e.tags));

        // Refroidissement : pas de second achat dans la foulée.
        let again = b.tick(NOW + 60, &swarm, None).await.unwrap();
        assert!(matches!(&again, Decision::Skipped(r) if r.contains("achat récent")), "{again:?}");
        assert_eq!(sink.0.lock().unwrap().len(), 1);
        drop(b);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn nothing_is_bought_without_need_with_a_local_gpu_or_when_terms_are_unacceptable() {
        let (db, dir) = temp_db("broker-no");
        let swarm = [brain("12D3KooWnear", 90.0, (45.7, 4.8))];
        // Pas de file.
        let (b, sink) = broker(db.clone(), false);
        assert_eq!(b.tick(NOW, &swarm, None).await.unwrap(), Decision::Idle);
        // File, mais un GPU local.
        queue(&db, "comfyui", "pending", 2).await;
        let (local, _) = broker(db.clone(), true);
        assert_eq!(local.tick(NOW, &swarm, None).await.unwrap(), Decision::Idle);
        // File, mais aucun Brain ne fournit le service.
        assert!(matches!(b.tick(NOW, &[], None).await.unwrap(), Decision::Skipped(r) if r.contains("aucun Brain")));
        // Brain inconnu de la base (ni clé ni tarif).
        assert!(matches!(b.tick(NOW, &swarm, None).await.unwrap(), Decision::Skipped(r) if r.contains("pas de clé")));
        // Tarif trop élevé (plafond 30 Ẑ/semaine).
        known_brain(&db, "12D3KooWnear", &BRAIN, 4_000).await;
        assert!(matches!(b.tick(NOW, &swarm, None).await.unwrap(), Decision::Skipped(r) if r.contains("trop élevé")));
        assert!(sink.0.lock().unwrap().is_empty());
        // Publication impossible : dite, et le refroidissement n'est PAS armé (on réessaiera).
        db.writer().execute("UPDATE tariffs SET price_cents = 50", vec![]).await.unwrap();
        let failing = Broker::new(db.clone(), ME, false, BrokerConfig::default(), Arc::new(Broken), None).unwrap();
        assert!(matches!(failing.tick(NOW, &swarm, None).await.unwrap(), Decision::Skipped(r) if r.contains("aucun relai joignable")));
        assert!(matches!(b.tick(NOW + 1, &swarm, None).await.unwrap(), Decision::Bought { .. }), "pas de refroidissement après un échec");
        drop((b, local, failing));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn leased_shares_shorten_the_estimate_and_running_tasks_count_as_load() {
        let (db, dir) = temp_db("broker-est");
        let (b, _) = broker(db.clone(), false);
        queue(&db, "comfyui", "running", 1).await;
        queue(&db, "comfyui", "pending", 1).await;
        // Un bail GPU actif de cette station : une part.
        db.writer()
            .execute("INSERT INTO resource_leases(src_event, beneficiary, node, resource, start_at, end_at, units, paid_cents, unit_price_cents, status) VALUES ('e', ?1, 'n', 'gpu_1_24', ?2, ?3, 1, 50, 50, 'active')", vec![V::Text(pubkey_of(&ME).unwrap()), V::Integer(NOW as i64 - 10), V::Integer(NOW as i64 + 3000)])
            .await
            .unwrap();
        let obs = b.observe(NOW).await.unwrap();
        assert_eq!(obs, Observation { waiting: 1, running: 1, leased_shares: 1 });
        assert!(b.estimate(&obs).mean_wait_secs.is_infinite(), "2 erlangs sur 1 part : la file déborde");
        db.writer().execute("UPDATE resource_leases SET end_at = ?1", vec![V::Integer(NOW as i64 - 1)]).await.unwrap();
        assert_eq!(b.observe(NOW).await.unwrap().leased_shares, 0, "un bail expiré ne compte plus");
        drop(b);
        let _ = std::fs::remove_dir_all(dir);
    }
}
