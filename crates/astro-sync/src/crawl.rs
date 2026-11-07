//! Crawl N² : synchronise les événements des membres N1 et N2 depuis les relais de la constellation.
//!
//! Une connexion par relai et par passage ; les clés y sont demandées par lots (`authors` de `batch_keys` clés au
//! plus, borné par [`MAX_LIST_LEN`]), page par page en remontant le temps avec `until`. Le curseur de reprise reste
//! par couple `relai|clé` : un lot regroupe des clés aux curseurs voisins, part du plus ancien (`since` = minimum du
//! lot, la déduplication absorbe le surplus) et chaque curseur n'avance qu'avec les événements de sa propre clé.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use astro_db::Db;
use astro_nostr_proto::filter::MAX_LIST_LEN;
use astro_nostr_proto::{Filter, is_hex64};
use futures_util::StreamExt;
use tokio::sync::watch;

use crate::client::{RelayClient, RelaySession};
use crate::ingest::{Ingested, ingest_event, purge_claimed};
use crate::SyncError;

/// Propriétaire des réclamations du crawl dans `claimed`.
const CLAIM_OWNER: &str = "crawl";
/// Rétention des réclamations.
const CLAIM_RETENTION_SECS: u64 = 7 * 24 * 3600;
/// Plafond `limit` le plus bas qu'on prête à un relai : certains relais bornent `limit` en dessous de `page_size`. Une
/// page d'au moins ce nombre d'événements est traitée comme pleine (on remonte le temps), pour qu'un lot de clés ne
/// perde pas ce qu'une requête par clé aurait reçu.
const RELAY_CAP_FLOOR: usize = 100;

/// Réglages du crawl.
#[derive(Debug, Clone)]
pub struct CrawlConfig {
    /// URL des relais de la constellation (`ws://…`).
    pub relays: Vec<String>,
    /// Kinds répliqués. Les kinds privés (4, 1059, 30078) n'en font pas partie par défaut.
    pub kinds: Vec<u32>,
    /// Événements demandés par page.
    pub page_size: usize,
    /// Pages au plus par clé et par relai (un lot de `n` clés dispose de `n × max_pages` pages).
    pub max_pages: usize,
    /// Marge relue avant le dernier `created_at` connu (rattrape les événements tardifs ; la déduplication absorbe le reste).
    pub overlap_secs: u64,
    /// Délai de grâce par page.
    pub fetch_timeout: Duration,
    /// Relais interrogés de front (une connexion chacun). Le curseur reste par couple : rien ne change pour la reprise.
    pub concurrency: usize,
    /// Clés par requête (`authors`), ramené entre 1 et [`MAX_LIST_LEN`].
    pub batch_keys: usize,
}

impl Default for CrawlConfig {
    fn default() -> Self {
        Self {
            relays: Vec::new(),
            kinds: vec![0, 1, 3, 1063, 10_002, 30_023, 30_800, 30_850, 30_851, 30_852],
            page_size: 500,
            max_pages: 20,
            overlap_secs: 3600,
            fetch_timeout: Duration::from_secs(30),
            concurrency: 8,
            batch_keys: MAX_LIST_LEN,
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

/// Moteur de crawl (relais en parallèle borné, clés groupées par lots sur une connexion par relai).
pub struct Crawler<C: RelayClient> {
    db: Arc<Db>,
    client: C,
    config: CrawlConfig,
    source: Option<RelaySource>,
    hook: Option<PassHook>,
    extra_keys: Vec<String>,
    nudge: Option<Arc<tokio::sync::Notify>>,
}

/// Scope `kv` des clés demandées au crawl par un autre module (clé = hex, valeur = date UNIX d'expiration) : par exemple le
/// compte d'un membre qui se rétablit ici et dont il faut retrouver les contacts. Chaque passage les synchronise jusqu'à expiration.
pub const WANTED_SCOPE: &str = "crawl_wanted";

impl<C: RelayClient> Crawler<C> {
    /// Crée un crawler.
    pub fn new(db: Arc<Db>, client: C, config: CrawlConfig) -> Self {
        Self { db, client, config, source: None, hook: None, extra_keys: Vec::new(), nudge: None }
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

    /// Réveil : un `notify_one` sur `nudge` lance tout de suite le passage suivant de la boucle continue (sans attendre `every`).
    pub fn with_nudge(mut self, nudge: Arc<tokio::sync::Notify>) -> Self {
        self.nudge = Some(nudge);
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

    /// Clés à synchroniser : MULTIPASS hébergés ∪ N1 ∪ N2 ∪ clés demandées non expirées ([`WANTED_SCOPE`]), triées.
    pub async fn targets(&self) -> Result<Vec<String>, SyncError> {
        Ok(self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT hex FROM identities WHERE hosted = 1 \
                     UNION SELECT pubkey FROM social_graph WHERE level IN (1, 2) \
                     UNION SELECT key FROM kv WHERE scope = 'crawl_wanted' AND length(key) = 64 AND CAST(value AS INTEGER) > CAST(strftime('%s','now') AS INTEGER) \
                     ORDER BY 1",
                )?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?)
    }

    /// Un passage complet : chaque clé sur chaque relai, `concurrency` relais à la fois. `extra` ajoute des clés d'amorçage.
    pub async fn run_once(&self, extra: &[String]) -> Result<CrawlStats, SyncError> {
        let mut keys = self.targets().await?;
        for k in extra {
            if is_hex64(k) && !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        let mut stats = CrawlStats::default();
        let relays = self.current_relays();
        stats.keys = keys.len();
        let mut running = futures_util::stream::iter(relays).map(|relay| self.sync_relay(relay, &keys)).buffer_unordered(self.config.concurrency.max(1));
        while let Some(s) = running.next().await {
            stats.merge(s);
        }
        drop(running);
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

    /// Toutes les clés sur un relai, par lots, sur une seule session. Les échecs sont dans le bilan, par lot.
    async fn sync_relay(&self, relay: String, keys: &[String]) -> CrawlStats {
        let mut stats = CrawlStats::default();
        if keys.is_empty() {
            return stats;
        }
        let cursors = match self.cursors(&relay).await {
            Ok(c) => c,
            Err(e) => {
                stats.errors.push((relay, lot_label(keys), e.to_string()));
                return stats;
            }
        };
        let mut session: Option<Box<dyn RelaySession + '_>> = None;
        for lot in self.lots(keys, &cursors) {
            if session.is_none() {
                match tokio::time::timeout(self.config.fetch_timeout, self.client.session(&relay)).await {
                    Ok(Ok(s)) => session = Some(s),
                    Ok(Err(e)) => {
                        stats.errors.push((relay.clone(), lot_label(&lot.keys), relay_error(e.to_string()).to_string()));
                        continue;
                    }
                    Err(_) => {
                        stats.errors.push((relay.clone(), lot_label(&lot.keys), relay_error("délai dépassé".into()).to_string()));
                        continue;
                    }
                }
            }
            let Some(open) = session.as_mut() else { continue };
            match self.sync_lot(open.as_mut(), &relay, &lot, &cursors).await {
                Ok(s) => stats.merge(s),
                Err(e) => {
                    if stats.errors.len() < 20 {
                        stats.errors.push((relay.clone(), lot_label(&lot.keys), e.to_string()));
                    }
                    // Connexion d'état incertain : le lot suivant en ouvre une neuve.
                    session = None;
                }
            }
        }
        stats
    }

    /// Lots de clés : d'abord les clés jamais synchronisées sur ce relai (sans `since`), puis les autres triées par
    /// curseur, pour qu'un lot rassemble des curseurs voisins.
    fn lots(&self, keys: &[String], cursors: &HashMap<String, u64>) -> Vec<Lot> {
        let size = self.config.batch_keys.clamp(1, MAX_LIST_LEN);
        let (mut fresh, mut known) = (Vec::new(), Vec::new());
        for k in keys {
            match cursors.get(k).copied().unwrap_or(0) {
                0 => fresh.push(k.clone()),
                c => known.push((c, k.clone())),
            }
        }
        known.sort();
        let mut lots: Vec<Lot> = fresh.chunks(size).map(|c| Lot { keys: c.to_vec(), since: None }).collect();
        for chunk in known.chunks(size) {
            let oldest = chunk.iter().map(|(c, _)| *c).min().unwrap_or(0);
            lots.push(Lot { keys: chunk.iter().map(|(_, k)| k.clone()).collect(), since: Some(oldest.saturating_sub(self.config.overlap_secs)) });
        }
        lots
    }

    /// Synchronise une clé sur un relai, de façon incrémentale (une session, un lot d'une clé).
    pub async fn sync_key(&self, relay: &str, key: &str) -> Result<CrawlStats, SyncError> {
        let cursors = self.cursors(relay).await?;
        let lot = self.lots(&[key.to_owned()], &cursors).pop().unwrap_or(Lot { keys: vec![key.to_owned()], since: None });
        let mut session = match tokio::time::timeout(self.config.fetch_timeout, self.client.session(relay)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(relay_error(e.to_string())),
            Err(_) => return Err(relay_error("délai dépassé".into())),
        };
        self.sync_lot(session.as_mut(), relay, &lot, &cursors).await
    }

    /// Un lot de clés sur une session : pages remontant le temps, puis curseurs avancés clé par clé.
    async fn sync_lot(&self, session: &mut (dyn RelaySession + '_), relay: &str, lot: &Lot, cursors: &HashMap<String, u64>) -> Result<CrawlStats, SyncError> {
        let authors: HashSet<&str> = lot.keys.iter().map(String::as_str).collect();
        let mut newest: HashMap<&str, u64> = HashMap::new();
        let mut stats = CrawlStats::default();
        let mut until: Option<u64> = None;
        let full = self.config.page_size.clamp(1, RELAY_CAP_FLOOR);
        for _ in 0..self.config.max_pages.saturating_mul(lot.keys.len()) {
            let filter = Filter {
                authors: Some(lot.keys.clone()),
                kinds: Some(self.config.kinds.clone()),
                since: lot.since,
                until,
                limit: Some(self.config.page_size),
                ..Filter::default()
            };
            let page = match tokio::time::timeout(self.config.fetch_timeout, session.fetch(&filter)).await {
                Ok(Ok(page)) => page,
                Ok(Err(e)) => return Err(relay_error(e.to_string())),
                Err(_) => return Err(relay_error("délai dépassé".into())),
            };
            let full_page = page.len() >= full;
            let oldest = page.iter().map(|e| e.created_at).min();
            for event in page {
                stats.fetched += 1;
                let Some(author) = authors.get(event.pubkey.as_str()).copied() else {
                    stats.rejected += 1;
                    continue;
                };
                if !self.config.kinds.contains(&event.kind) {
                    stats.rejected += 1;
                    continue;
                }
                let at = event.created_at;
                // Seul un événement valide de la clé fait avancer son curseur : un faux daté du futur ne peut pas
                // geler la reprise de cette clé.
                match ingest_event(self.db.writer(), event, CLAIM_OWNER).await? {
                    Ingested::Inserted => stats.inserted += 1,
                    Ingested::Duplicate => stats.duplicates += 1,
                    Ingested::Superseded => stats.superseded += 1,
                    Ingested::Rejected(_) => {
                        stats.rejected += 1;
                        continue;
                    }
                }
                let slot = newest.entry(author).or_insert(0);
                *slot = (*slot).max(at);
            }
            // Page pleine : remonter dans le temps ; `until` inclusif, la déduplication absorbe le chevauchement.
            match (full_page, oldest) {
                (true, Some(o)) if until != Some(o) => until = Some(o),
                _ => break,
            }
        }
        for (key, at) in newest {
            if at > cursors.get(key).copied().unwrap_or(0) {
                self.set_cursor(&format!("{relay}|{key}"), at).await?;
            }
        }
        Ok(stats)
    }

    /// Curseurs de toutes les clés pour `relay` (clés `relai|clé` : intervalle `[relai|, relai})`, `}` suivant `|`).
    async fn cursors(&self, relay: &str) -> Result<HashMap<String, u64>, SyncError> {
        let (low, high) = (format!("{relay}|"), format!("{relay}}}"));
        let rows: Vec<(String, String)> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT key, value FROM kv WHERE scope = 'crawl_cursor' AND key >= ?1 AND key < ?2")?;
                Ok(stmt.query_map([&low, &high], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        let prefix = format!("{relay}|");
        Ok(rows.into_iter().filter_map(|(k, v)| Some((k.strip_prefix(&prefix)?.to_owned(), v.parse().ok()?))).collect())
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
            let nudge = self.nudge.clone();
            let nudged = async move {
                match nudge {
                    Some(n) => n.notified().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = nudged => {}
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

/// Clés d'un lot et borne basse commune.
struct Lot {
    keys: Vec<String>,
    since: Option<u64>,
}

/// Désignation d'un lot dans le bilan : la clé seule, ou la première et le nombre des autres.
fn lot_label(keys: &[String]) -> String {
    match keys {
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} (+{} clés)", rest.len()),
        [] => String::new(),
    }
}

/// Les événements déjà ingérés restent acquis ; le curseur n'avance pas, le prochain passage reprend.
fn relay_error(reason: String) -> SyncError {
    SyncError::Relay(reason)
}
