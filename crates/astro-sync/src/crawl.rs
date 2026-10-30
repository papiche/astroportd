//! Crawl N² : synchronise, clef par clef, les événements des membres N1 et N2 depuis les relais de la constellation.

use std::sync::Arc;
use std::time::Duration;

use astro_db::Db;
use astro_nostr_proto::{Event, Filter, is_hex64};
use tokio::sync::watch;

use crate::client::RelayClient;
use crate::ingest::{Ingested, ingest_event, purge_claimed};
use crate::SyncError;

/// Propriétaire des réclamations du crawl dans `claimed`.
const CLAIM_OWNER: &str = "crawl";
/// Rétention des réclamations.
const CLAIM_RETENTION_SECS: u64 = 7 * 24 * 3600;

/// Réglages du crawl.
#[derive(Debug, Clone)]
pub struct CrawlConfig {
    /// URL des relais de la constellation (`ws://…`).
    pub relays: Vec<String>,
    /// Kinds répliqués. Les kinds privés (4, 1059, 30078) n'en font pas partie par défaut.
    pub kinds: Vec<u32>,
    /// Événements demandés par page.
    pub page_size: usize,
    /// Pages au plus par clé et par relai.
    pub max_pages: usize,
    /// Marge relue avant le dernier `created_at` connu (rattrape les événements tardifs ; la déduplication absorbe le reste).
    pub overlap_secs: u64,
    /// Délai de grâce par page.
    pub fetch_timeout: Duration,
}

impl Default for CrawlConfig {
    fn default() -> Self {
        Self {
            relays: Vec::new(),
            kinds: vec![0, 1, 3, 1063, 10_002, 30_023, 30_800, 30_850, 30_851, 30_852, 30_853],
            page_size: 500,
            max_pages: 20,
            overlap_secs: 3600,
            fetch_timeout: Duration::from_secs(30),
        }
    }
}

/// Bilan d'un crawl.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CrawlStats {
    /// Clés interrogées.
    pub keys: usize,
    /// Événements reçus.
    pub fetched: u64,
    /// Événements nouveaux stockés.
    pub inserted: u64,
    /// Doublons ignorés.
    pub duplicates: u64,
    /// Versions remplaçables plus anciennes que celle stockée.
    pub superseded: u64,
    /// Événements refusés (signature, auteur ou kind inattendus).
    pub rejected: u64,
    /// Les 20 premières erreurs `(relai, clé, raison)`.
    pub errors: Vec<(String, String, String)>,
}

impl CrawlStats {
    fn merge(&mut self, other: CrawlStats) {
        self.keys += other.keys;
        self.fetched += other.fetched;
        self.inserted += other.inserted;
        self.duplicates += other.duplicates;
        self.superseded += other.superseded;
        self.rejected += other.rejected;
        for e in other.errors {
            if self.errors.len() < 20 {
                self.errors.push(e);
            }
        }
    }
}

/// Relais à interroger, relus à chaque passage (la constellation change : une station arrive, une autre disparaît).
pub type RelaySource = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Bilan d'un passage de la boucle continue.
#[derive(Debug, Clone)]
pub struct PassReport {
    /// Secondes UNIX de fin.
    pub at: u64,
    /// Durée du passage.
    pub millis: u64,
    /// Relais interrogés.
    pub relays: usize,
    /// Résultat : bilan, ou l'erreur qui a interrompu le passage.
    pub result: Result<CrawlStats, String>,
}

/// Appelé après chaque passage de la boucle continue.
pub type PassHook = Arc<dyn Fn(&PassReport) + Send + Sync>;

/// Scope et clé du dernier bilan dans `kv` (lu par l'interface).
pub const STATUS_SCOPE: &str = "crawl_status";
/// Clé du dernier passage.
pub const STATUS_KEY: &str = "last";

/// Moteur de crawl séquentiel.
pub struct Crawler<C: RelayClient> {
    db: Arc<Db>,
    client: C,
    config: CrawlConfig,
    source: Option<RelaySource>,
    hook: Option<PassHook>,
    extra_keys: Vec<String>,
}

impl<C: RelayClient> Crawler<C> {
    /// Crée un crawler.
    pub fn new(db: Arc<Db>, client: C, config: CrawlConfig) -> Self {
        Self { db, client, config, source: None, hook: None, extra_keys: Vec::new() }
    }

    /// Ajoute aux relais fixes ceux que `source` donne à chaque passage.
    pub fn with_relay_source(mut self, source: RelaySource) -> Self {
        self.source = Some(source);
        self
    }

    /// Clés suivies en plus des membres hébergés et de leurs proches (la clé coopérative, dont le kind 30800 porte la
    /// configuration de la constellation) : chaque passage de la boucle continue les synchronise.
    pub fn with_extra_keys(mut self, keys: Vec<String>) -> Self {
        self.extra_keys = keys;
        self
    }

    /// Fonction appelée après chaque passage de [`Crawler::run_forever`] (journal d'activité, alertes).
    pub fn with_hook(mut self, hook: PassHook) -> Self {
        self.hook = Some(hook);
        self
    }

    /// Relais fixes puis relais découverts, sans doublon.
    pub fn current_relays(&self) -> Vec<String> {
        let mut all = self.config.relays.clone();
        for r in self.source.as_ref().map(|f| f()).unwrap_or_default() {
            if !all.contains(&r) {
                all.push(r);
            }
        }
        all
    }

    /// Clés à synchroniser : MULTIPASS hébergés ∪ N1 ∪ N2, triées.
    pub async fn targets(&self) -> Result<Vec<String>, SyncError> {
        Ok(self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT hex FROM identities WHERE hosted = 1 \
                     UNION SELECT pubkey FROM social_graph WHERE level IN (1, 2) \
                     ORDER BY 1",
                )?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?)
    }

    /// Un passage complet : chaque clé, l'une après l'autre, sur chaque relai. `extra` ajoute des clés d'amorçage.
    pub async fn run_once(&self, extra: &[String]) -> Result<CrawlStats, SyncError> {
        let mut keys = self.targets().await?;
        for k in extra {
            if is_hex64(k) && !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        let mut stats = CrawlStats::default();
        let relays = self.current_relays();
        for key in &keys {
            stats.keys += 1;
            for relay in &relays {
                match self.sync_key(relay, key).await {
                    Ok(s) => stats.merge(s),
                    Err(e) => {
                        if stats.errors.len() < 20 {
                            stats.errors.push((relay.clone(), key.clone(), e.to_string()));
                        }
                    }
                }
            }
        }
        purge_claimed(self.db.writer(), CLAIM_RETENTION_SECS).await?;
        Ok(stats)
    }

    /// Repète [`Crawler::run_once`] tant que de nouveaux événements arrivent (au plus `max_rounds` fois) :
    /// les contacts découverts à un tour sont interrogés au suivant (N1 puis N2).
    pub async fn run_until_stable(&self, extra: &[String], max_rounds: usize) -> Result<CrawlStats, SyncError> {
        let mut total = CrawlStats::default();
        for _ in 0..max_rounds.max(1) {
            let round = self.run_once(extra).await?;
            let progressed = round.inserted > 0;
            total.merge(round);
            if !progressed {
                break;
            }
        }
        Ok(total)
    }

    /// Synchronise une clé sur un relai, de façon incrémentale.
    pub async fn sync_key(&self, relay: &str, key: &str) -> Result<CrawlStats, SyncError> {
        let cursor_key = format!("{relay}|{key}");
        let cursor = self.cursor(&cursor_key).await?;
        let since = (cursor > 0).then(|| cursor.saturating_sub(self.config.overlap_secs));
        let mut stats = CrawlStats::default();
        let mut until: Option<u64> = None;
        let mut newest = cursor;
        for _ in 0..self.config.max_pages {
            let filter = Filter {
                authors: Some(vec![key.to_owned()]),
                kinds: Some(self.config.kinds.clone()),
                since,
                until,
                limit: Some(self.config.page_size),
                ..Filter::default()
            };
            let page = match tokio::time::timeout(self.config.fetch_timeout, self.client.fetch(relay, &filter)).await {
                Ok(Ok(page)) => page,
                Ok(Err(e)) => return Err(relay_error(e.to_string())),
                Err(_) => return Err(relay_error("délai dépassé".into())),
            };
            let full_page = page.len() >= self.config.page_size;
            let oldest = page.iter().map(|e| e.created_at).min();
            for event in page {
                stats.fetched += 1;
                newest = newest.max(event.created_at);
                match self.accept(&event, key) {
                    Err(_) => stats.rejected += 1,
                    Ok(()) => match ingest_event(self.db.writer(), event, CLAIM_OWNER).await? {
                        Ingested::Inserted => stats.inserted += 1,
                        Ingested::Duplicate => stats.duplicates += 1,
                        Ingested::Superseded => stats.superseded += 1,
                        Ingested::Rejected(_) => stats.rejected += 1,
                    },
                }
            }
            // Page pleine : remonter dans le temps ; `until` inclusif, la déduplication absorbe le chevauchement.
            match (full_page, oldest) {
                (true, Some(o)) if until != Some(o) => until = Some(o),
                _ => break,
            }
        }
        if newest > cursor {
            self.set_cursor(&cursor_key, newest).await?;
        }
        Ok(stats)
    }

    /// Un relai ne peut pas nous imposer d'autres auteurs ni d'autres kinds que ceux demandés.
    fn accept(&self, e: &Event, key: &str) -> Result<(), &'static str> {
        if e.pubkey != key {
            return Err("auteur inattendu");
        }
        if !self.config.kinds.contains(&e.kind) {
            return Err("kind non demandé");
        }
        Ok(())
    }

    async fn cursor(&self, key: &str) -> Result<u64, SyncError> {
        let key = key.to_owned();
        let v: Option<String> = self
            .db
            .readers()
            .read(move |c| {
                Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = 'crawl_cursor' AND key = ?1", [&key], |r| r.get(0)))?)
            })
            .await?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    async fn set_cursor(&self, key: &str, value: u64) -> Result<(), SyncError> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES ('crawl_cursor', ?1, ?2, ?3) \
                 ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![key.to_owned().into(), value.to_string().into(), i64::try_from(now).unwrap_or(i64::MAX).into()],
            )
            .await?;
        Ok(())
    }

    /// Un passage de la boucle continue : crawl jusqu'à stabilité (3 tours), bilan enregistré dans `kv` et transmis au crochet.
    /// Une erreur n'est jamais avalée : elle est dans le bilan.
    pub async fn pass(&self) -> PassReport {
        let started = std::time::Instant::now();
        let relays = self.current_relays().len();
        let result = self.run_until_stable(&self.extra_keys, 3).await.map_err(|e| e.to_string());
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let report = PassReport { at, millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX), relays, result };
        let json = match &report.result {
            Ok(s) => serde_json::json!({
                "at": at, "ms": report.millis, "relays": relays, "keys": s.keys, "fetched": s.fetched, "inserted": s.inserted,
                "duplicates": s.duplicates, "superseded": s.superseded, "rejected": s.rejected,
                "errors": s.errors.iter().map(|(relay, key, reason)| serde_json::json!({ "relay": relay, "key": key, "reason": reason })).collect::<Vec<_>>(),
            }),
            Err(e) => serde_json::json!({ "at": at, "ms": report.millis, "relays": relays, "error": e }),
        };
        let _ = self
            .db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![STATUS_SCOPE.to_owned().into(), STATUS_KEY.to_owned().into(), json.to_string().into(), i64::try_from(at).unwrap_or(i64::MAX).into()],
            )
            .await;
        if let Some(hook) = &self.hook {
            hook(&report);
        }
        report
    }

    /// Boucle de planification : un passage toutes les `every`, jusqu'au signal d'arrêt.
    pub async fn run_forever(self: Arc<Self>, every: Duration, shutdown: watch::Receiver<bool>) {
        self.run_forever_after(Duration::ZERO, every, shutdown).await;
    }

    /// Comme [`Crawler::run_forever`], le premier passage n'ayant lieu qu'après `first` (le démarrage n'est pas ralenti).
    pub async fn run_forever_after(self: Arc<Self>, first: Duration, every: Duration, mut shutdown: watch::Receiver<bool>) {
        let mut wait = first;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                    continue;
                }
            }
            tokio::select! {
                _ = self.pass() => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
            wait = every;
        }
    }
}

/// Les événements déjà ingérés restent acquis ; le curseur n'avance pas, le prochain passage reprend.
fn relay_error(reason: String) -> SyncError {
    SyncError::Relay(reason)
}
