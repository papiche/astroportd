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
use astro_nostr_proto::{Event, Filter, is_hex64};
use futures_util::StreamExt;
use tokio::sync::watch;

use crate::client::{ClientError, RelayClient, RelaySession};
use crate::ingest::{Ingested, ingest_events, purge_claimed};
use crate::SyncError;

/// Propriétaire des réclamations du crawl dans `claimed`.
const CLAIM_OWNER: &str = "crawl";
/// Rétention des réclamations.
const CLAIM_RETENTION_SECS: u64 = 7 * 24 * 3600;
/// Plafond `limit` le plus bas qu'on prête à un relai : certains relais bornent `limit` en dessous de `page_size`. Une
/// page d'au moins ce nombre d'événements est traitée comme pleine (on remonte le temps), pour qu'un lot de clés ne
/// perde pas ce qu'une requête par clé aurait reçu.
const RELAY_CAP_FLOOR: usize = 100;
/// Événements locaux au plus dans une réconciliation de lot (le relai astroportd en accepte autant) : au-delà, les pages.
const NEG_LOCAL_CAP: usize = 100_000;

/// Réglages du crawl.
#[derive(Debug, Clone)]
pub struct CrawlConfig {
    /// URL des relais de la constellation (`ws://…`).
    pub relays: Vec<String>,
    /// Kinds répliqués pour les membres hébergés, les N1 et les clés demandées. Les kinds privés (4, 1059, 30078) n'en font
    /// pas partie par défaut ; le kind 5 (suppressions NIP-09) si, pour qu'une note retirée chez l'auteur le soit ici aussi.
    pub kinds: Vec<u32>,
    /// Kinds répliqués pour les N2 : seulement ce dont le pare-feu social a besoin (profil, contacts, relais, DID), les
    /// suppressions, et le registre Ğ1-N² (30852, qui s'étend par définition au N²). Leurs notes et articles (1, 30023) ne
    /// sont pas rapatriés.
    pub n2_kinds: Vec<u32>,
    /// Kinds ajoutés pour les seuls membres **hébergés** (31903 : paramètres applicatifs, chiffrés pour eux).
    pub hosted_kinds: Vec<u32>,
    /// Kinds demandés pour une clé connue seulement par le maillage (Capitaine annoncé par la carte d'une station, que
    /// personne ici ne suit encore) : les kinds **publics** seulement, car la station distante refuse toute requête qui en
    /// contient un autre à une clé hors de son cercle.
    pub mesh_kinds: Vec<u32>,
    /// Kinds que tout relai d'astroportd livre sans authentification (`public_kinds` du relai) : quand un relai refuse un
    /// lot parce que la clé de cette station n'est pas dans son cercle, le lot est relu avec ces kinds seulement.
    pub public_kinds: Vec<u32>,
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
            kinds: vec![0, 1, 3, 5, 1063, 10_002, 30_023, 30_800, 30_850, 30_851, 30_852],
            n2_kinds: vec![0, 3, 5, 10_002, 30_800, 30_852],
            hosted_kinds: vec![31_903],
            mesh_kinds: vec![0, 3, 10_002, 30_800],
            public_kinds: vec![0, 1, 3, 10_002, 30_023, 30_800, 30_850],
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
    /// Lots réconciliés par Negentropy (NIP-77) plutôt que lus par pages.
    pub negentropy: u64,
    /// Lots qu'un relai a refusés (clé de la station hors de son cercle) et relus en kinds publics seulement.
    pub public_only: u64,
}

impl CrawlStats {
    fn merge(&mut self, other: CrawlStats) {
        self.keys += other.keys;
        self.negentropy += other.negentropy;
        self.public_only += other.public_only;
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

/// Clés à synchroniser en plus, relues à chaque passage : par exemple les Capitaines annoncés par les cartes du maillage P2P
/// (une clé disparaît d'elle-même quand sa carte n'est plus là). Aucune écriture en base.
pub type KeysSource = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Clés données par une [`KeysSource`] à chaque passage, au plus (une carte hostile ne gonfle pas le crawl).
pub const MAX_SOURCE_KEYS: usize = 256;

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
    keys_source: Option<KeysSource>,
    nudge: Option<Arc<tokio::sync::Notify>>,
}

/// Scope `kv` des clés demandées au crawl par un autre module (clé = hex, valeur = date UNIX d'expiration) : par exemple le
/// compte d'un membre qui se rétablit ici et dont il faut retrouver les contacts. Chaque passage les synchronise jusqu'à expiration.
pub const WANTED_SCOPE: &str = "crawl_wanted";

impl<C: RelayClient> Crawler<C> {
    /// Crée un crawler.
    pub fn new(db: Arc<Db>, client: C, config: CrawlConfig) -> Self {
        Self { db, client, config, source: None, hook: None, extra_keys: Vec::new(), keys_source: None, nudge: None }
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

    /// Clés suivies en plus, relues à chaque passage (cercle [`Circle::Mesh`] : profil, contacts, relais, DID — les kinds
    /// publics). Une clé déjà dans un cercle garde ses kinds. Seules les clés hexadécimales de 64 caractères sont retenues,
    /// [`MAX_SOURCE_KEYS`] au plus.
    pub fn with_keys_source(mut self, source: KeysSource) -> Self {
        self.keys_source = Some(source);
        self
    }

    /// Clés données par la source du maillage, valides et sans doublon, [`MAX_SOURCE_KEYS`] au plus.
    pub fn mesh_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for k in self.keys_source.as_ref().map(|f| f()).unwrap_or_default().into_iter().filter(|k| is_hex64(k)) {
            if keys.len() >= MAX_SOURCE_KEYS {
                break;
            }
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        keys
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

    /// Clés à synchroniser : MULTIPASS hébergés ∪ N1 ∪ N2 ∪ clés demandées non expirées ([`WANTED_SCOPE`]), triées. En
    /// Forteresse (mode de la station), pas de N².
    pub async fn targets(&self) -> Result<Vec<String>, SyncError> {
        Ok(self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(&format!(
                    "SELECT hex FROM identities WHERE hosted = 1 \
                     UNION SELECT pubkey FROM social_graph WHERE level = 1 OR (level = 2 AND NOT {}) \
                     UNION SELECT key FROM kv WHERE scope = 'crawl_wanted' AND length(key) = 64 AND CAST(value AS INTEGER) > CAST(strftime('%s','now') AS INTEGER) \
                     ORDER BY 1",
                    astro_db::station_mode::FORTRESS_SQL
                ))?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?)
    }

    /// Comme [`Crawler::targets`], avec le cercle de chaque clé (ce qui décide des kinds demandés pour elle).
    pub async fn circles(&self) -> Result<Vec<(String, Circle)>, SyncError> {
        Ok(self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(&format!(
                    "SELECT hex, 0 FROM identities WHERE hosted = 1 \
                     UNION ALL SELECT pubkey, 1 FROM social_graph WHERE level = 1 \
                     UNION ALL SELECT key, 1 FROM kv WHERE scope = 'crawl_wanted' AND length(key) = 64 AND CAST(value AS INTEGER) > CAST(strftime('%s','now') AS INTEGER) \
                     UNION ALL SELECT pubkey, 2 FROM social_graph WHERE level = 2 AND NOT {}",
                    astro_db::station_mode::FORTRESS_SQL
                ))?;
                let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                // Le cercle le plus proche l'emporte (hébergé, puis N1, puis N2).
                let mut best: HashMap<String, i64> = HashMap::new();
                for (k, lvl) in rows {
                    let slot = best.entry(k).or_insert(lvl);
                    *slot = (*slot).min(lvl);
                }
                let mut out: Vec<(String, Circle)> = best.into_iter().map(|(k, l)| (k, match l { 0 => Circle::Hosted, 1 => Circle::Near, _ => Circle::Far })).collect();
                out.sort();
                Ok(out)
            })
            .await?)
    }

    /// Kinds demandés pour un cercle.
    fn kinds_for(&self, circle: Circle) -> Vec<u32> {
        match circle {
            Circle::Hosted => self.config.kinds.iter().chain(&self.config.hosted_kinds).copied().collect(),
            Circle::Near => self.config.kinds.clone(),
            Circle::Far => self.config.n2_kinds.clone(),
            Circle::Mesh => self.config.mesh_kinds.clone(),
        }
    }

    /// Un passage complet : chaque clé sur chaque relai, `concurrency` relais à la fois. `extra` ajoute des clés d'amorçage.
    pub async fn run_once(&self, extra: &[String]) -> Result<CrawlStats, SyncError> {
        let mut keys = self.circles().await?;
        for k in extra {
            match keys.iter_mut().find(|(x, _)| x == k) {
                Some(slot) => slot.1 = slot.1.min(Circle::Near),
                None if is_hex64(k) => keys.push((k.clone(), Circle::Near)),
                None => {}
            }
        }
        // Clés du maillage : seulement celles qui ne sont dans aucun cercle (un ami garde ses kinds N¹).
        for k in self.mesh_keys() {
            if !keys.iter().any(|(x, _)| *x == k) {
                keys.push((k, Circle::Mesh));
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
    async fn sync_relay(&self, relay: String, keys: &[(String, Circle)]) -> CrawlStats {
        let mut stats = CrawlStats::default();
        if keys.is_empty() {
            return stats;
        }
        let cursors = match self.cursors(&relay).await {
            Ok(c) => c,
            Err(e) => {
                let all: Vec<String> = keys.iter().map(|(k, _)| k.clone()).collect();
                stats.errors.push((relay, lot_label(&all), e.to_string()));
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
                // Relai qui ne nous compte pas dans son cercle (Capitaine ami sur une autre station : c'est son Capitaine, pas
                // notre NODE, qui est suivi) : ce qui est public passe quand même — profil, contacts, DID… — sans curseur.
                Err(e) if refused(&e) && self.public_lot(&lot).is_some() => {
                    let Some(public) = self.public_lot(&lot) else { continue };
                    match self.sync_lot(open.as_mut(), &relay, &public, &cursors).await {
                        Ok(s) => {
                            stats.merge(s);
                            stats.public_only += 1;
                        }
                        Err(e) => {
                            if stats.errors.len() < 20 {
                                stats.errors.push((relay.clone(), lot_label(&lot.keys), e.to_string()));
                            }
                            session = None;
                        }
                    }
                }
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

    /// Le lot réduit à ses kinds publics, s'il en a et s'il en demandait d'autres. Jamais de curseur : les événements privés
    /// restent à rapatrier le jour où ce relai nous ouvre son cercle.
    fn public_lot(&self, lot: &Lot) -> Option<Lot> {
        let kinds: Vec<u32> = lot.kinds.iter().copied().filter(|k| self.config.public_kinds.contains(k)).collect();
        (!kinds.is_empty() && kinds.len() < lot.kinds.len()).then(|| Lot { keys: lot.keys.clone(), since: None, kinds, cursor: false })
    }

    /// Lots de clés, par cercle (un lot = un même jeu de kinds) : d'abord les clés jamais synchronisées sur ce relai (sans
    /// `since`), puis les autres triées par curseur, pour qu'un lot rassemble des curseurs voisins.
    fn lots(&self, keys: &[(String, Circle)], cursors: &HashMap<String, u64>) -> Vec<Lot> {
        let size = self.config.batch_keys.clamp(1, MAX_LIST_LEN);
        let mut lots = Vec::new();
        for circle in [Circle::Hosted, Circle::Near, Circle::Far, Circle::Mesh] {
            let (kinds, cursor) = (self.kinds_for(circle), circle != Circle::Mesh);
            let (mut fresh, mut known) = (Vec::new(), Vec::new());
            for (k, _) in keys.iter().filter(|(_, c)| *c == circle) {
                match if cursor { cursors.get(k).copied().unwrap_or(0) } else { 0 } {
                    0 => fresh.push(k.clone()),
                    c => known.push((c, k.clone())),
                }
            }
            known.sort();
            lots.extend(fresh.chunks(size).map(|c| Lot { keys: c.to_vec(), since: None, kinds: kinds.clone(), cursor }));
            for chunk in known.chunks(size) {
                let oldest = chunk.iter().map(|(c, _)| *c).min().unwrap_or(0);
                lots.push(Lot { keys: chunk.iter().map(|(_, k)| k.clone()).collect(), since: Some(oldest.saturating_sub(self.config.overlap_secs)), kinds: kinds.clone(), cursor });
            }
        }
        lots
    }

    /// Synchronise une clé sur un relai, de façon incrémentale (une session, un lot d'une clé).
    pub async fn sync_key(&self, relay: &str, key: &str) -> Result<CrawlStats, SyncError> {
        let cursors = self.cursors(relay).await?;
        let lot = self.lots(&[(key.to_owned(), Circle::Near)], &cursors).pop().unwrap_or(Lot { keys: vec![key.to_owned()], since: None, kinds: self.config.kinds.clone(), cursor: true });
        let mut session = match tokio::time::timeout(self.config.fetch_timeout, self.client.session(relay)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(relay_error(e.to_string())),
            Err(_) => return Err(relay_error("délai dépassé".into())),
        };
        self.sync_lot(session.as_mut(), relay, &lot, &cursors).await
    }

    /// Une page reçue pour le lot : chaque événement est vérifié (auteur du lot, kind demandé pour son cercle), puis la page est
    /// stockée et projetée en **une** transaction ([`ingest_events`]). Les curseurs des clés avancent avec leurs événements valides.
    async fn absorb_page<'k>(&self, page: Vec<Event>, lot: &Lot, authors: &HashSet<&'k str>, newest: &mut HashMap<&'k str, u64>, stats: &mut CrawlStats) -> Result<(), SyncError> {
        let mut keep = Vec::with_capacity(page.len());
        for event in page {
            stats.fetched += 1;
            match authors.get(event.pubkey.as_str()).copied() {
                Some(author) if lot.kinds.contains(&event.kind) => keep.push((author, event)),
                _ => stats.rejected += 1,
            }
        }
        let (who, events): (Vec<(&'k str, u64)>, Vec<Event>) = keep.into_iter().map(|(a, e)| ((a, e.created_at), e)).unzip();
        let outcomes = ingest_events(self.db.writer(), events, CLAIM_OWNER).await?;
        for ((author, at), outcome) in who.into_iter().zip(outcomes) {
            match outcome {
                Ingested::Inserted => stats.inserted += 1,
                Ingested::Duplicate => stats.duplicates += 1,
                Ingested::Superseded => stats.superseded += 1,
                Ingested::Rejected(_) => {
                    stats.rejected += 1;
                    continue;
                }
            }
            // Seul un événement valide de la clé fait avancer son curseur : un faux daté du futur ne peut pas geler la reprise.
            let slot = newest.entry(author).or_insert(0);
            *slot = (*slot).max(at);
        }
        Ok(())
    }

    /// Réconciliation Negentropy (NIP-77) du lot : l'ensemble local est tout ce que la base a déjà pour ces clés et cette
    /// fenêtre, **quel que soit le relai qui l'a apporté** ; seuls les événements manquants voyagent, et la fenêtre de
    /// recouvrement ne coûte plus rien. `None` : le relai ne la connaît pas, ou la fenêtre est trop grosse — les pages prennent
    /// le relais.
    async fn reconcile_lot(&self, session: &mut (dyn RelaySession + '_), relay: &str, lot: &Lot, cursors: &HashMap<String, u64>) -> Result<Option<CrawlStats>, SyncError> {
        let (keys, kinds, since) = (lot.keys.clone(), lot.kinds.clone(), lot.since.unwrap_or(0));
        let local: Vec<(u64, String, String)> = self
            .db
            .readers()
            .read(move |c| {
                let (pk, kd) = (vec!["?"; keys.len()].join(","), kinds.iter().map(u32::to_string).collect::<Vec<_>>().join(","));
                let sql = format!("SELECT created_at, id, pubkey FROM nostr_events WHERE pubkey IN ({pk}) AND kind IN ({kd}) AND created_at >= {since} LIMIT {}", NEG_LOCAL_CAP + 1);
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(keys.iter()), |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;
        if local.len() > NEG_LOCAL_CAP {
            return Ok(None);
        }
        let filter = Filter { authors: Some(lot.keys.clone()), kinds: Some(lot.kinds.clone()), since: lot.since, ..Filter::default() };
        let pairs: Vec<(u64, String)> = local.iter().map(|(t, id, _)| (*t, id.clone())).collect();
        let result = match tokio::time::timeout(self.config.fetch_timeout.saturating_mul(4), session.sync(&filter, &pairs)).await {
            Ok(Ok(r)) => r,
            Ok(Err(ClientError::Unsupported(_) | ClientError::Closed(_))) => return Ok(None),
            Ok(Err(e)) => return Err(relay_error(e.to_string())),
            Err(_) => return Err(relay_error("délai dépassé".into())),
        };
        let authors: HashSet<&str> = lot.keys.iter().map(String::as_str).collect();
        let mut newest: HashMap<&str, u64> = HashMap::new();
        for (t, _, pk) in &local {
            if let Some(a) = authors.get(pk.as_str()).copied() {
                let slot = newest.entry(a).or_insert(0);
                *slot = (*slot).max(*t);
            }
        }
        let mut stats = CrawlStats { negentropy: 1, ..CrawlStats::default() };
        let mut found = HashMap::new();
        for page in result.missing.chunks(self.config.page_size.max(1)) {
            self.absorb_page(page.to_vec(), lot, &authors, &mut found, &mut stats).await?;
        }
        for (a, at) in found {
            let slot = newest.entry(a).or_insert(0);
            *slot = (*slot).max(at);
        }
        for (key, at) in newest.into_iter().filter(|_| lot.cursor) {
            if at > cursors.get(key).copied().unwrap_or(0) {
                self.set_cursor(&format!("{relay}|{key}"), at).await?;
            }
        }
        Ok(Some(stats))
    }

    /// Un lot de clés sur une session : réconciliation NIP-77 si le relai la connaît, sinon pages remontant le temps ; puis
    /// curseurs avancés clé par clé.
    async fn sync_lot(&self, session: &mut (dyn RelaySession + '_), relay: &str, lot: &Lot, cursors: &HashMap<String, u64>) -> Result<CrawlStats, SyncError> {
        if let Some(stats) = self.reconcile_lot(session, relay, lot, cursors).await? {
            return Ok(stats);
        }
        let authors: HashSet<&str> = lot.keys.iter().map(String::as_str).collect();
        let mut newest: HashMap<&str, u64> = HashMap::new();
        let mut stats = CrawlStats::default();
        let mut until: Option<u64> = None;
        let full = self.config.page_size.clamp(1, RELAY_CAP_FLOOR);
        for _ in 0..self.config.max_pages.saturating_mul(lot.keys.len()) {
            let filter = Filter {
                authors: Some(lot.keys.clone()),
                kinds: Some(lot.kinds.clone()),
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
            self.absorb_page(page, lot, &authors, &mut newest, &mut stats).await?;
            // Page pleine : remonter dans le temps ; `until` inclusif, la déduplication absorbe le chevauchement.
            match (full_page, oldest) {
                (true, Some(o)) if until != Some(o) => until = Some(o),
                _ => break,
            }
        }
        for (key, at) in newest.into_iter().filter(|_| lot.cursor) {
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
                "duplicates": s.duplicates, "superseded": s.superseded, "rejected": s.rejected, "negentropy": s.negentropy,
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

/// Clés d'un lot, borne basse commune et kinds demandés (ceux de leur cercle).
struct Lot {
    keys: Vec<String>,
    since: Option<u64>,
    kinds: Vec<u32>,
    /// Le lot fait avancer les curseurs de ses clés. Pas pour le cercle [`Circle::Mesh`] : le jour où la clé est suivie, ses
    /// événements plus anciens (privés) doivent encore être rapatriés depuis le début.
    cursor: bool,
}

/// Cercle d'une clé suivie : il décide des kinds demandés pour elle (voir [`CrawlConfig`]). Ordonné du plus proche au plus loin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Circle {
    /// Membre hébergé ici.
    Hosted,
    /// N1, clé demandée ou clé d'amorçage.
    Near,
    /// N2 seulement.
    Far,
    /// Annoncé par une carte du maillage, hors de tout cercle : kinds publics seulement, sans curseur.
    Mesh,
}

/// Kinds dont l'auteur a quitté le cercle de la station (ni hébergé, ni dans `social_graph`) : la nuit les purge.
pub const PURGED_KINDS: [u32; 2] = [1, 30_023];

/// Purge nocturne (`solar_cron`) : notes et articles ([`PURGED_KINDS`]) d'auteurs qui ne sont plus ni hébergés ni dans le
/// graphe social. Ce qui cite un membre hébergé (tag `p`) est gardé : c'est sa conversation. Rien n'est purgé tant que le
/// graphe est vide (base neuve, replay pas encore passé). Rend le nombre d'événements retirés.
pub async fn purge_strangers(writer: &astro_db::Writer) -> Result<usize, SyncError> {
    let kinds = PURGED_KINDS.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
    let out = writer
        .execute(
            format!(
                "DELETE FROM nostr_events WHERE kind IN ({kinds}) AND EXISTS (SELECT 1 FROM social_graph) \
                 AND NOT EXISTS (SELECT 1 FROM identities i WHERE i.hex = nostr_events.pubkey AND i.hosted = 1) \
                 AND NOT EXISTS (SELECT 1 FROM social_graph s WHERE s.pubkey = nostr_events.pubkey) \
                 AND NOT EXISTS (SELECT 1 FROM nostr_tags t JOIN identities h ON h.hex = t.value AND h.hosted = 1 WHERE t.event_id = nostr_events.seq AND t.name = 'p')"
            ),
            vec![],
        )
        .await?;
    Ok(out.changes)
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

/// Le relai a refusé la lecture : authentification exigée (sans clé pour la donner) ou clé hors de son cercle.
fn refused(e: &SyncError) -> bool {
    matches!(e, SyncError::Relay(r) if r.contains("authentification requise") || r.contains("restricted:") || r.contains("auth-required"))
}

#[cfg(test)]
mod tests {
    use astro_db::Statement;
    use astro_nostr_proto::{UnsignedEvent, pubkey_of};

    use super::*;
    use crate::client::WsClient;

    const ALICE: [u8; 32] = [0xA1; 32];
    const BOB: [u8; 32] = [0xB2; 32];
    const CAROL: [u8; 32] = [0xC3; 32];
    const ZED: [u8; 32] = [0xE9; 32];

    fn hex(s: &[u8; 32]) -> String {
        pubkey_of(s).unwrap()
    }

    fn note(s: &[u8; 32], kind: u32, ts: u64, tags: Vec<Vec<String>>) -> Event {
        UnsignedEvent { created_at: ts, kind, tags, content: "bonjour".into() }.sign(s).unwrap()
    }

    fn db(tag: &str) -> (std::path::PathBuf, Arc<Db>) {
        let dir = std::env::temp_dir().join(format!("astro-sync-crawl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        (dir, db)
    }

    async fn graph(db: &Db) {
        db.writer()
            .batch(vec![
                crate::Projector::mark_hosted(&hex(&ALICE), "multipass"),
                Statement::new("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(&BOB).into()]),
                Statement::new("INSERT INTO social_graph(owner_hex, pubkey, level, via, updated_at) VALUES (?1, ?2, 2, ?3, 0)", vec![hex(&ALICE).into(), hex(&CAROL).into(), hex(&BOB).into()]),
            ])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn each_circle_gets_only_the_kinds_it_needs() {
        let (dir, db) = db("circles");
        graph(&db).await;
        let crawler = Crawler::new(db.clone(), WsClient::new(Duration::from_secs(1)), CrawlConfig::default());
        let circles = crawler.circles().await.unwrap();
        let lots = crawler.lots(&circles, &HashMap::new());
        let kinds_of = |k: &[u8; 32]| lots.iter().find(|l| l.keys.contains(&hex(k))).unwrap().kinds.clone();
        let (alice, bob, carol) = (kinds_of(&ALICE), kinds_of(&BOB), kinds_of(&CAROL));
        assert!(alice.contains(&31_903) && alice.contains(&5) && alice.contains(&1), "{alice:?}");
        assert!(!bob.contains(&31_903) && bob.contains(&1) && bob.contains(&30_023) && bob.contains(&5), "{bob:?}");
        assert!(!carol.contains(&1) && !carol.contains(&30_023) && [0, 3, 10_002, 30_800].iter().all(|k| carol.contains(k)), "{carol:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_page_is_ingested_in_one_go_and_a_forged_event_is_refused_alone() {
        let (dir, db) = db("page");
        let a = note(&BOB, 1, 10, vec![]);
        let mut forged = note(&BOB, 1, 11, vec![]);
        forged.content = "falsifié".into();
        let out = ingest_events(db.writer(), vec![a.clone(), forged, a], "t").await.unwrap();
        assert_eq!(out, [Ingested::Inserted, Ingested::Rejected("signature ou identifiant invalide"), Ingested::Duplicate]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_night_purges_notes_of_authors_who_left_the_circle() {
        let (dir, db) = db("purge");
        // Graphe vide : rien n'est purgé (base neuve ou replay pas encore passé).
        ingest_events(db.writer(), vec![note(&ZED, 1, 1, vec![])], "t").await.unwrap();
        assert_eq!(purge_strangers(db.writer()).await.unwrap(), 0);
        graph(&db).await;
        let keep_reply = note(&ZED, 1, 2, vec![vec!["p".into(), hex(&ALICE)]]);
        let events = vec![note(&ALICE, 1, 3, vec![]), note(&BOB, 30_023, 4, vec![vec!["d".into(), "a".into()]]), note(&CAROL, 1, 5, vec![]), note(&ZED, 30_023, 6, vec![vec!["d".into(), "b".into()]]), note(&ZED, 0, 7, vec![]), keep_reply];
        ingest_events(db.writer(), events, "t").await.unwrap();
        assert_eq!(purge_strangers(db.writer()).await.unwrap(), 2, "la note et l'article de Zed, sorti du cercle");
        let left: Vec<(String, i64)> = db.readers().read(|c| Ok(c.prepare("SELECT pubkey, kind FROM nostr_events WHERE pubkey = ?1 ORDER BY kind")?.query_map([hex(&ZED)], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap();
        assert_eq!(left, [(hex(&ZED), 0), (hex(&ZED), 1)], "son profil reste, et sa réponse à un membre aussi");
        let _ = std::fs::remove_dir_all(dir);
    }
}
