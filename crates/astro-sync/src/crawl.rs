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
            kinds: vec![0, 1, 3, 30_023, 30_800, 30_850, 30_851, 30_852],
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

/// Moteur de crawl séquentiel.
pub struct Crawler<C: RelayClient> {
    db: Arc<Db>,
    client: C,
    config: CrawlConfig,
}

impl<C: RelayClient> Crawler<C> {
    /// Crée un crawler.
    pub fn new(db: Arc<Db>, client: C, config: CrawlConfig) -> Self {
        Self { db, client, config }
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
        for key in &keys {
            stats.keys += 1;
            for relay in &self.config.relays {
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

    /// Boucle de planification : un passage toutes les `every`, jusqu'au signal d'arrêt.
    pub async fn run_forever(self: Arc<Self>, every: Duration, mut shutdown: watch::Receiver<bool>) {
        loop {
            let _ = self.run_until_stable(&[], 3).await;
            tokio::select! {
                () = tokio::time::sleep(every) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }
}

/// Les événements déjà ingérés restent acquis ; le curseur n'avance pas, le prochain passage reprend.
fn relay_error(reason: String) -> SyncError {
    SyncError::Relay(reason)
}
