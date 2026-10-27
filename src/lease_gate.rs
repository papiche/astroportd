//! Contrôle d'accès des flux DRAGON vers les services lourds : pas de bail valide, pas de flux.
//!
//! `comfyui` et `ollama` consomment le GPU de la station : un pair ne peut ouvrir `/x/comfyui-<PeerID>` ou
//! `/x/ollama-<PeerID>` que si lui-même (son NODE) ou un MULTIPASS qu'il héberge détient, à cet instant, un bail
//! `gpu_1_24` actif chez ce nœud (voir `astro_economy::lease`). Les autres services (SSH, relai, Qdrant…) ne sont pas
//! concernés. La garde échoue fermée : une base illisible refuse le flux.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use astro_core::ActivityBus;
use astro_core::week::{date_string, week_start};
use astro_db::{Db, DbError};
use astro_economy::lease::{self, Resource};
use astro_p2p::PeerId;
use astro_p2p::tunnel::{GateFuture, StreamGate};

/// Ressource que consomme un service exposé, s'il est « lourd ».
pub fn resource_for(service: &str) -> Option<Resource> {
    (service == "comfyui" || service == "ollama" || service.starts_with("comfyui-") || service.starts_with("ollama-")).then_some(Resource::Gpu1of24)
}

/// Plus longue durée facturée entre deux contrôles d'un même flux (une horloge qui saute ne vide pas le quota).
const MAX_CHARGE_SECS: u64 = 180;

/// Portée du compteur dans la table `kv` ; clé `<PeerID>:<AAAA-MM-JJ>`, valeur = secondes consommées ce jour-là (UTC).
pub const METER_SCOPE: &str = "gpu_meter";

/// Jours de compteurs conservés (au-delà d'une semaine ISO, ils ne servent plus).
const METER_KEEP_DAYS: u64 = 14;

/// Compteur de calcul GPU par pair et par jour UTC, persisté dans `kv` : un redémarrage de la station ne remet pas
/// le compteur du jour à zéro. Une part hebdomadaire donne [`lease::GPU_DAILY_QUOTA_SECS`] par jour, dans la limite
/// de [`lease::GPU_WEEKLY_QUOTA_SECS`] pour la semaine ISO. Le temps est facturé à chaque contrôle du watchdog.
pub struct QuotaMeter {
    db: Arc<Db>,
    /// Dernier contrôle de chaque pair (mémoire : sert seulement à mesurer l'intervalle, jamais le solde).
    last: Mutex<HashMap<String, u64>>,
}

/// Consommation d'un pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Used {
    /// Secondes consommées aujourd'hui (UTC).
    pub day: u64,
    /// Secondes consommées depuis le lundi de la semaine ISO courante.
    pub week: u64,
}

impl Used {
    fn within_quota(self) -> bool {
        self.day < lease::GPU_DAILY_QUOTA_SECS && self.week < lease::GPU_WEEKLY_QUOTA_SECS
    }
}

impl QuotaMeter {
    /// Compteur adossé à `db`.
    pub fn new(db: Arc<Db>) -> Self {
        Self { db, last: Mutex::new(HashMap::new()) }
    }

    /// Ouverture d'un flux : refusée si le quota du jour ou de la semaine est épuisé.
    pub async fn open(&self, peer: &str, now: u64) -> Result<bool, DbError> {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).insert(peer.to_owned(), now);
        Ok(self.charge(peer, now, 0).await?.within_quota())
    }

    /// Contrôle d'un flux ouvert : facture le temps écoulé depuis le contrôle précédent (au plus
    /// [`MAX_CHARGE_SECS`], et rien au-delà d'une frontière de jour), refuse si un quota est atteint.
    pub async fn tick(&self, peer: &str, now: u64) -> Result<bool, DbError> {
        let prev = self.last.lock().unwrap_or_else(|e| e.into_inner()).insert(peer.to_owned(), now);
        let delta = prev.filter(|p| p / 86_400 == now / 86_400).map_or(0, |p| now.saturating_sub(p).min(MAX_CHARGE_SECS));
        Ok(self.charge(peer, now, delta).await?.within_quota())
    }

    /// Consommation de `peer` à l'instant `now`.
    pub async fn used(&self, peer: &str, now: u64) -> Result<Used, DbError> {
        self.charge(peer, now, 0).await
    }

    /// Ajoute `delta` secondes au jour de `now`, élague les vieux jours et rend la consommation (une transaction).
    async fn charge(&self, peer: &str, now: u64, delta: u64) -> Result<Used, DbError> {
        let peer = peer.to_owned();
        self.db
            .writer()
            .transaction(move |tx| {
                let key = format!("{peer}:{}", date_string(now));
                if delta > 0 {
                    tx.execute(
                        "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, CAST(?3 AS TEXT), ?4) \
                         ON CONFLICT(scope, key) DO UPDATE SET value = CAST(CAST(value AS INTEGER) + ?3 AS TEXT), updated_at = ?4",
                        rusqlite::params![METER_SCOPE, key, delta as i64, now as i64],
                    )?;
                }
                let sum = |from: &str, to: &str| -> Result<u64, DbError> {
                    let n: i64 = tx.query_row(
                        "SELECT COALESCE(SUM(CAST(value AS INTEGER)), 0) FROM kv WHERE scope = ?1 AND key BETWEEN ?2 AND ?3",
                        rusqlite::params![METER_SCOPE, format!("{peer}:{from}"), format!("{peer}:{to}")],
                        |r| r.get(0),
                    )?;
                    Ok(u64::try_from(n).unwrap_or(0))
                };
                let today = date_string(now);
                let monday = week_start(now);
                let used = Used { day: sum(&today, &today)?, week: sum(&date_string(monday), &date_string(monday + 6 * 86_400))? };
                // Élagage : les 10 derniers caractères de la clé sont la date.
                tx.execute(
                    "DELETE FROM kv WHERE scope = ?1 AND substr(key, -10) < ?2",
                    rusqlite::params![METER_SCOPE, date_string(now.saturating_sub(METER_KEEP_DAYS * 86_400))],
                )?;
                Ok(used)
            })
            .await
    }
}

/// Garde de baux.
pub struct LeaseGate {
    quota: Arc<QuotaMeter>,
    db: Arc<Db>,
    node_hex: String,
    activity: Option<ActivityBus>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl LeaseGate {
    /// Garde du nœud `node_hex` (la clé Nostr qui encaisse les baux), horloge système.
    pub fn new(db: Arc<Db>, node_hex: String, activity: Option<ActivityBus>) -> Self {
        let clock = Arc::new(|| SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()));
        Self { quota: Arc::new(QuotaMeter::new(db.clone())), db, node_hex, activity, clock }
    }

    /// Horloge imposée (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }
}

impl LeaseGate {
    /// Décision commune à l'ouverture et au watchdog ; `announce` : journaliser aussi les autorisations.
    fn decide(&self, peer: PeerId, service: &str, announce: bool) -> GateFuture {
        let Some(resource) = resource_for(service) else { return Box::pin(async { true }) };
        let quota = self.quota.clone();
        let (db, node, now, activity, service, peer_s) = (self.db.clone(), self.node_hex.clone(), (self.clock)(), self.activity.clone(), service.to_owned(), peer.to_string());
        Box::pin(async move {
            let who = peer_s.clone();
            let granted = db.readers().read(move |c| Ok(lease::has_active_lease(c, &node, resource, now, &who)?)).await;
            let metered = match granted {
                Ok(true) if announce => Some(quota.open(&peer_s, now).await),
                Ok(true) => Some(quota.tick(&peer_s, now).await),
                _ => None,
            };
            let (ok, note) = match (granted, metered) {
                (Ok(true), Some(Ok(false))) => (false, "quota de calcul épuisé (1 h/jour, 7 h/semaine)"),
                (Ok(true), Some(Err(_))) => (false, "compteur illisible : refus par précaution"),
                (Ok(true), _) => (true, "bail valide"),
                (Ok(false), _) => (false, "pas de bail valide pour ce créneau"),
                (Err(_), _) => (false, "base illisible : refus par précaution"),
            };
            if let (Some(bus), true) = (activity, announce || !ok) {
                let short: String = peer_s.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
                bus.publish("p2p", "p2p:gate", format!("flux {service} {} pour …{short} ({note})", match (ok, announce) { (true, _) => "ouvert", (false, true) => "refusé", (false, false) => "coupé" }), None);
            }
            ok
        })
    }
}

impl StreamGate for LeaseGate {
    fn allow(&self, peer: PeerId, service: &str) -> GateFuture {
        self.decide(peer, service, true)
    }

    fn still_allowed(&self, peer: PeerId, service: &str) -> GateFuture {
        self.decide(peer, service, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn meter(dir: &std::path::Path) -> (Arc<Db>, QuotaMeter) {
        let db = Arc::new(Db::open(&dir.join("m.db"), 2).unwrap());
        let q = QuotaMeter::new(db.clone());
        (db, q)
    }

    #[tokio::test]
    async fn the_daily_quota_is_one_hour_persists_across_restarts_and_resets_each_utc_day() {
        let dir = std::env::temp_dir().join(format!("astroportd-meter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (db, q) = meter(&dir).await;
        let day = 1_800_000_000 / 86_400 * 86_400;
        assert!(q.open("p", day + 10).await.unwrap());
        // 20 contrôles de 180 s = 1 h : le dernier atteint le quota.
        let mut t = day + 10;
        let mut cut = None;
        for i in 1..=25 {
            t += 180;
            if !q.tick("p", t).await.unwrap() {
                cut = Some(i);
                break;
            }
        }
        assert_eq!(cut, Some(20));
        // Redémarrage : un nouveau compteur sur la même base retrouve la consommation du jour.
        drop(q);
        let q = QuotaMeter::new(db.clone());
        assert!(!q.open("p", t + 1).await.unwrap(), "quota épuisé : le redémarrage ne le remet pas à zéro");
        assert_eq!(q.used("p", t).await.unwrap().day, 3_600);
        let raw: String = db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'gpu_meter' AND key = ?1", [format!("p:{}", date_string(t))], |r| r.get(0))?)).await.unwrap();
        assert_eq!(raw, "3600", "forme documentée : kv gpu_meter / <peer>:<jour> = secondes");
        assert!(q.open("autre", t).await.unwrap(), "le quota est par pair");
        assert!(q.open("p", day + 86_400 + 5).await.unwrap(), "nouveau jour, nouveau quota");
        // Une horloge qui saute ne facture pas plus qu'un intervalle de contrôle.
        assert!(q.tick("p", day + 86_400 + 5 + 50_000).await.unwrap());
        assert_eq!(q.used("p", day + 86_400 + 50_005).await.unwrap().day, 180);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_weekly_capital_is_seven_hours_and_old_days_are_pruned() {
        let dir = std::env::temp_dir().join(format!("astroportd-meter-w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (db, q) = meter(&dir).await;
        let monday = week_start(1_800_000_000);
        assert_eq!(lease::GPU_WEEKLY_QUOTA_SECS, 25_200);
        // Sept jours d'une heure : le septième atteint le capital de la part (25 200 s).
        for d in 0..7u64 {
            let t = monday + d * 86_400 + 100;
            assert!(q.open("p", t).await.unwrap(), "jour {d}");
            let mut now = t;
            for _ in 0..20 {
                now += 180;
                let _ = q.tick("p", now).await.unwrap();
            }
        }
        let end = monday + 6 * 86_400 + 4_000;
        let u = q.used("p", end).await.unwrap();
        assert_eq!(u, Used { day: 3_600, week: 25_200 });
        // La semaine suivante : le compteur hebdomadaire repart de zéro, et les jours de plus de 14 jours sont élagués.
        let next = monday + 7 * 86_400 + 100;
        assert_eq!(q.used("p", next).await.unwrap(), Used { day: 0, week: 0 });
        let later = monday + 30 * 86_400;
        q.used("p", later).await.unwrap();
        let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM kv WHERE scope = 'gpu_meter'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_gpu_services_are_gated() {
        for s in ["comfyui", "ollama", "comfyui-2", "ollama-gpu"] {
            assert_eq!(resource_for(s), Some(Resource::Gpu1of24), "{s}");
        }
        for s in ["ssh", "strfry", "qdrant", "ipfs", "ollamaproxy"] {
            assert_eq!(resource_for(s), None, "{s}");
        }
    }
}
