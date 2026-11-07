//! Vision de la constellation : portefeuilles de la coopérative (`UPLANETNAME_G1` et ceux que la transaction primale marque),
//! leurs soldes, et l'historique de leurs virements pour la carte animée.
//!
//! L'historique d'un portefeuille n'est pas dans le nœud Duniter : il vient de l'indexeur (squid, `--squid-url` ou découverte),
//! copié dans `coop_ledger`. **Sans indexeur, la table reste vide** et l'interface le dit (« historique partiel ») : les soldes,
//! eux, viennent toujours de la chaîne. Les adresses des membres ne sont jamais données : un virement vers ou depuis un compte
//! hébergé ici s'appelle `membres`, tout autre compte `extérieur`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use astro_db::{Db, Statement};
use astro_duniter::Chain;
use rusqlite::types::Value;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::charter_accept::CharterAccept;
use crate::coop_config;
use crate::coop_wallets::Wallets;
use crate::fetch::{FetchLimits, post_json};
use crate::station::zen_of_g1;

const SCOPE: &str = "coop_ledger";
/// Pages lues au plus à chaque passage (le reste au passage suivant : le curseur est gardé).
const MAX_PAGES: usize = 40;
const SYNC_EVERY: Duration = Duration::from_secs(15 * 60);
const FIRST_SYNC_AFTER: Duration = Duration::from_secs(45);
const BALANCE_TTL: Duration = Duration::from_secs(60);
const SQUID_MAX_BYTES: u64 = 512 * 1024;
/// Événements rendus au plus par appel.
pub const MAX_EVENTS: usize = 20_000;
/// Questions à l'indexeur au plus par affichage de la vigilance (les suivantes au prochain : les réponses sont gardées).
const MAX_PRIMAL_LOOKUPS: usize = 12;
/// Questions à l'indexeur au plus par passage de fond (toutes les 15 min).
const BACKGROUND_LOOKUPS: usize = 120;
/// Réserve que chaque portefeuille de la coopérative garde (1 Ğ1) : elle n'est jamais une alerte.
const RESERVE_CENTS: i64 = 100;

/// Un virement lu chez l'indexeur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    /// Identifiant chez l'indexeur.
    pub id: String,
    /// Numéro du bloc ; négatif pour l'historique de la Ğ1 v1 repris à la genèse de Duniter v2s.
    pub block: i64,
    /// Secondes Unix.
    pub at: u64,
    /// Expéditeur.
    pub from: Option<String>,
    /// Destinataire.
    pub to: Option<String>,
    /// Centimes de Ğ1.
    pub cents: u64,
}

/// Une page de la réponse de l'indexeur.
#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    /// Virements de la page.
    pub transfers: Vec<Transfer>,
    /// Curseur de la page suivante, s'il y en a une.
    pub next: Option<String>,
}

/// Requête GraphQL : les virements dont `wallets` est l'expéditeur ou le destinataire, du plus ancien au plus récent.
pub fn ledger_query(wallets: &[String], after: Option<&str>) -> String {
    json!({
        "query": "query($w:[String!],$a:Cursor){transfers(filter:{or:[{fromId:{in:$w}},{toId:{in:$w}}]},orderBy:BLOCK_NUMBER_ASC,first:200,after:$a){pageInfo{hasNextPage endCursor} nodes{id blockNumber timestamp amount fromId toId}}}",
        "variables": { "w": wallets, "a": after },
    })
    .to_string()
}

/// Secondes Unix d'une date `AAAA-MM-JJThh:mm:ss[.fff][Z|+hh:mm]` (lue comme UTC).
pub fn unix_of(stamp: &str) -> Option<u64> {
    let b = stamp.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> { std::str::from_utf8(b.get(from..from + len)?).ok()?.parse().ok() };
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, m, d, hh, mm, ss) = (num(0, 4)?, num(5, 2)?, num(8, 2)?, num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Jours depuis 1970 (algorithme civil de H. Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

/// Lit la réponse de l'indexeur. Une erreur GraphQL est une erreur, jamais « aucun virement ».
pub fn parse_page(body: &[u8]) -> Result<Page, String> {
    let v: Json = serde_json::from_slice(body).map_err(|e| format!("réponse illisible : {e}"))?;
    if let Some(msg) = v["errors"][0]["message"].as_str() {
        return Err(format!("erreur de l'indexeur : {msg}"));
    }
    let t = &v["data"]["transfers"];
    let nodes = t["nodes"].as_array().ok_or("réponse sans data.transfers.nodes")?;
    let mut transfers = Vec::with_capacity(nodes.len());
    for n in nodes {
        let text = |k: &str| n[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let (Some(id), Some(block), Some(at), Some(cents)) = (text("id"), n["blockNumber"].as_i64(), text("timestamp").and_then(|s| unix_of(&s)), text("amount").and_then(|s| s.parse::<u64>().ok())) else {
            return Err(format!("virement incomplet chez l'indexeur : {n}"));
        };
        transfers.push(Transfer { id, block, at, from: text("fromId"), to: text("toId"), cents });
    }
    let next = (t["pageInfo"]["hasNextPage"].as_bool() == Some(true)).then(|| t["pageInfo"]["endCursor"].as_str().map(str::to_owned)).flatten();
    Ok(Page { transfers, next })
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Vue de la constellation : portefeuilles, historique, nom et charte.
pub struct Constellation<C: Chain> {
    db: Arc<Db>,
    chain: Option<Arc<C>>,
    wallets: Arc<Wallets>,
    uplanet_name: String,
    squid: Arc<RwLock<Vec<String>>>,
    limits: FetchLimits,
    balances: Mutex<Option<(Instant, Vec<Json>)>>,
    accept: Option<Arc<CharterAccept>>,
    billets: Option<Arc<dyn BilletView>>,
}

/// Ce que la vue de la constellation lit des billets (implémenté par [`crate::billet::Billets`]) : un suivi agrégé et les adresses des billets.
pub trait BilletView: Send + Sync {
    /// Suivi agrégé.
    fn stats(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Json> + Send + '_>>;
    /// Adresses des billets connus.
    fn addresses(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = HashSet<String>> + Send + '_>>;
}

impl<C: Chain, P: crate::funds::PrimalSource> BilletView for crate::billet::Billets<C, P> {
    fn stats(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Json> + Send + '_>> {
        Box::pin(crate::billet::Billets::stats(self))
    }
    fn addresses(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = HashSet<String>> + Send + '_>> {
        Box::pin(crate::billet::Billets::addresses(self))
    }
}

impl<C: Chain> Constellation<C> {
    /// Vue pour `uplanet_name` ; `squid` : indexeurs (`https://…/v1/graphql`), vide s'il n'y en a pas.
    pub fn new(db: Arc<Db>, chain: Option<Arc<C>>, wallets: Arc<Wallets>, uplanet_name: String, squid: Vec<String>) -> Self {
        let mut limits = FetchLimits::new(SQUID_MAX_BYTES);
        limits.timeout = Duration::from_secs(20);
        Self { db, chain, wallets, uplanet_name, squid: Arc::new(RwLock::new(squid)), limits, balances: Mutex::new(None), accept: None, billets: None }
    }

    /// Joint le service d'acceptation de la charte : la vue liste alors les Capitaines qui l'ont signée.
    #[must_use]
    pub fn with_acceptances(mut self, accept: Arc<CharterAccept>) -> Self {
        self.accept = Some(accept);
        self
    }

    /// Joint le suivi des Ğ1Billets.
    #[must_use]
    pub fn with_billets(mut self, billets: Arc<dyn BilletView>) -> Self {
        self.billets = Some(billets);
        self
    }

    /// Le Capitaine `captain` signe la charte d'empreinte `sha256` (la version courante).
    pub async fn accept_charter(&self, captain: &str, sha256: &str) -> Result<Json, String> {
        self.accept.as_ref().ok_or("acceptation de la charte non activée")?.accept(captain, sha256).await
    }

    /// Indexeurs configurés ?
    pub fn has_indexer(&self) -> bool {
        !self.squid.read().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    /// Les portefeuilles : `UPLANETNAME_G1` (le maître) d'abord, puis ceux que marque la transaction primale. `(nom, ss58)`.
    async fn named(&self) -> Result<Vec<(String, String)>, String> {
        let master = self.wallets.master().await?;
        let mut out = vec![("UPLANETNAME_G1".to_owned(), master.account().ss58())];
        // Sans NODE connu (station neuve), seuls les portefeuilles qui n'en dépendent pas sont rendus : la vision ne tombe pas pour autant.
        match self.wallets.targets().await {
            Ok(t) => out.extend(t.iter().map(|(n, a)| (n.clone(), a.ss58()))),
            Err(e) => eprintln!("constellation : portefeuilles coopératifs incomplets : {e}"),
        }
        Ok(out)
    }

    async fn kv(&self, scope: &'static str, key: String) -> Option<String> {
        self.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![scope, key], |r| r.get::<_, String>(0)))?)).await.ok().flatten()
    }

    async fn set_kv(&self, key: &str, value: &str) {
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![SCOPE.to_owned().into(), key.to_owned().into(), value.to_owned().into(), Value::Integer(now() as i64)]).await;
    }

    /// Un passage de lecture de l'indexeur ; rend le nombre de virements nouveaux. Reprend au curseur gardé.
    pub async fn sync_once(&self) -> Result<usize, String> {
        let urls = self.squid.read().unwrap_or_else(|e| e.into_inner()).clone();
        if urls.is_empty() {
            return Err("aucun indexeur".into());
        }
        let wallets = self.named().await?;
        let addrs: Vec<String> = wallets.iter().map(|(_, a)| a.clone()).collect();
        // Le curseur ne vaut que pour la liste de portefeuilles qui l'a produit.
        let tag = hex::encode(&Sha256::digest(addrs.join(",").as_bytes())[..8]);
        let mut cursor = self.kv(SCOPE, "cursor".into()).await.and_then(|v| v.split_once('|').filter(|(t, _)| *t == tag).map(|(_, c)| c.to_owned())).filter(|c| !c.is_empty());
        let mut added = 0usize;
        for _ in 0..MAX_PAGES {
            let query = ledger_query(&addrs, cursor.as_deref());
            let mut errors = Vec::new();
            let mut got = None;
            for url in &urls {
                match post_json(url, &query, &self.limits).await.map_err(|e| e.to_string()).and_then(|b| parse_page(&b)) {
                    Ok(p) => {
                        got = Some(p);
                        break;
                    }
                    Err(e) => errors.push(format!("{url} : {e}")),
                }
            }
            let page = got.ok_or_else(|| format!("aucun indexeur n'a répondu ({})", errors.join(" ; ")))?;
            let stmts: Vec<Statement> = page
                .transfers
                .iter()
                .map(|t| Statement {
                    sql: "INSERT OR IGNORE INTO coop_ledger(id, block, at, from_id, to_id, cents) VALUES (?1, ?2, ?3, ?4, ?5, ?6)".into(),
                    params: vec![t.id.clone().into(), Value::Integer(t.block), Value::Integer(t.at as i64), t.from.clone().map_or(Value::Null, Value::Text), t.to.clone().map_or(Value::Null, Value::Text), Value::Integer(t.cents as i64)],
                })
                .collect();
            if !stmts.is_empty() {
                let out = self.db.writer().batch(stmts).await.map_err(|e| e.to_string())?;
                added += out.changes;
            }
            match page.next {
                Some(next) => {
                    self.set_kv("cursor", &format!("{tag}|{next}")).await;
                    cursor = Some(next);
                }
                None => break,
            }
        }
        self.set_kv("synced_at", &now().to_string()).await;
        self.set_kv("error", "").await;
        Ok(added)
    }

    /// Lecture périodique de l'indexeur, jusqu'à l'arrêt. Une panne est dite une fois dans le bilan, jamais avalée.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        if !self.has_indexer() {
            return;
        }
        let mut wait = FIRST_SYNC_AFTER;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => return,
            }
            match self.sync_once().await {
                Ok(n) if n > 0 => println!("constellation : {n} virement(s) de la coopérative lus chez l'indexeur"),
                Ok(_) => {}
                Err(e) => {
                    eprintln!("constellation : historique des portefeuilles : {e}");
                    self.set_kv("error", &e).await;
                }
            }
            // Les renvois vers INTRUSION sont analysés ici, en tâche de fond (émetteur, primales) : l'écran du Capitaine trouve
            // les réponses déjà gardées au lieu d'interroger l'indexeur à chaque affichage.
            match self.vigilance_with(&[], BACKGROUND_LOOKUPS).await {
                Ok(v) if v["pending"].as_u64().unwrap_or(0) > 0 => println!("constellation : vigilance, {} renvoi(s) vers INTRUSION encore à analyser", v["pending"]),
                Ok(_) => {}
                Err(e) => eprintln!("constellation : vigilance des portefeuilles : {e}"),
            }
            wait = SYNC_EVERY;
        }
    }

    async fn balances(&self) -> Result<Vec<Json>, String> {
        if let Some((at, v)) = self.balances.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(at, _)| at.elapsed() < BALANCE_TTL) {
            let _ = at;
            return Ok(v.clone());
        }
        let wallets = self.named().await?;
        let master = wallets.first().map(|(_, a)| a.clone()).unwrap_or_default();
        let mut markers: HashMap<String, String> = HashMap::new();
        for (name, _) in &wallets {
            if let Some(m) = self.kv(crate::primal_init::SCOPE, name.clone()).await {
                markers.insert(name.clone(), m);
            }
        }
        let mut out = Vec::with_capacity(wallets.len());
        for (name, ss58) in wallets {
            let account = ss58.parse::<astro_duniter::Account>().ok();
            let g1 = match (&self.chain, account) {
                (Some(c), Some(a)) => c.balance(&a).await.ok().map(|b| b.total.0),
                _ => None,
            };
            // Le portefeuille maître finance les autres : il n'a pas de transaction primale. Pour les autres, ce que **la chaîne**
            // dit (primale confirmée par les indexeurs) prime sur le marqueur local « envoyée » : `verified` = reçue de
            // UPLANETNAME_G1, `foreign` = premier virement venu d'ailleurs (ce n'est pas un portefeuille officiel), `done` = envoyée
            // mais pas encore vérifiée, `pending` = pas encore envoyée.
            let primal = if name == "UPLANETNAME_G1" {
                "master"
            } else {
                match self.primal(&ss58, false).await {
                    Ok(Some(p)) if p == master => "verified",
                    Ok(Some(_)) => "foreign",
                    _ => markers.get(&name).map_or("pending", |m| if matches!(m.as_str(), "paid" | "sending") { "done" } else { "pending" }),
                }
            };
            out.push(json!({ "name": name, "address": ss58, "g1_cents": g1, "zen_cents": g1.map(zen_of_g1), "primal": primal }));
        }
        *self.balances.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), out.clone()));
        Ok(out)
    }

    /// Nom de l'UPlanet et charte (texte type tant que le Capitaine n'en a pas publié une), signés par la clé coopérative, avec
    /// les Capitaines qui l'ont acceptée.
    async fn charter(&self, viewer: &str) -> Json {
        let Ok((_, hex)) = coop_config::coop_keys(&self.uplanet_name) else { return Json::Null };
        let stored = coop_config::load(&self.db, &hex).await;
        let c = coop_config::charter_of(stored.as_ref());
        let signatories = match &self.accept {
            Some(a) => a.signatories(viewer).await.unwrap_or(Json::Null),
            None => Json::Null,
        };
        json!({
            "name": c.name, "version": c.version, "text": c.text, "custom": c.custom, "sha256": c.sha256, "signed_by": hex,
            "event_id": stored.as_ref().map(|s| s.event_id.clone()), "updated_at": stored.as_ref().map(|s| s.created_at), "signatories": signatories,
        })
    }

    /// Adresses Ğ1 des comptes hébergés ici (jamais montrées : elles s'appellent `membres`).
    async fn member_addresses(&self) -> Result<HashSet<String>, String> {
        self.db
            .readers()
            .read(|c| {
                let mut st = c.prepare("SELECT g1pub, g1pub_nostr FROM identities WHERE hosted = 1")?;
                let mut set = HashSet::new();
                for r in st.query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))? {
                    let (a, b) = r?;
                    set.extend(a.into_iter().chain(b).filter(|s| !s.is_empty()));
                }
                Ok(set)
            })
            .await
            .map_err(|e| e.to_string())
    }

    async fn is_captain(&self, viewer: &str) -> bool {
        let v = viewer.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT EXISTS (SELECT 1 FROM identities WHERE hex = ?1 AND role = 'captain' AND hosted = 1)", [v], |r| r.get::<_, bool>(0))?)).await.unwrap_or(false)
    }

    /// Primale d'un portefeuille (émetteur de son premier virement reçu), **confirmée par au moins deux indexeurs**
    /// ([`crate::funds::primal_consensus`]) : gardée en base (cache partagé avec le contrôle des virements, `funds`), sinon
    /// demandée si `ask` le permet. `Err` : pas encore confirmée (jamais une primale devinée).
    async fn primal(&self, who: &str, ask: bool) -> Result<Option<String>, String> {
        let k = who.to_owned();
        let cached: Option<String> = self.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![crate::funds::PRIMAL_SCOPE, k], |r| r.get(0)))?)).await.ok().flatten();
        if cached.is_some() {
            return Ok(cached);
        }
        if !ask {
            return Err("pas encore vérifiée".into());
        }
        let urls = self.squid.read().unwrap_or_else(|e| e.into_inner()).clone();
        let primal = crate::funds::primal_consensus(&urls, &self.limits, who).await?;
        if let Some(p) = &primal {
            let _ = self.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO NOTHING", vec![crate::funds::PRIMAL_SCOPE.to_owned().into(), who.to_owned().into(), p.clone().into(), Value::Integer(now() as i64)]).await;
        }
        Ok(primal)
    }

    /// Émetteur d'un virement redirigé vers `INTRUSION` : le virement de même montant reçu juste avant par le
    /// portefeuille `via` (historique local, sinon indexeur). Gardé en base (`intrusion_source`, `<adresse>|<secondes>`) une
    /// fois trouvé. `Ok(None)` : introuvable (un vidage total, `DRAIN`, n'a pas de montant correspondant).
    async fn intrusion_source(&self, redirect: &str, via: &str, cents: i64, at: i64, block: i64, ask: bool) -> Result<Option<(String, i64)>, String> {
        let key = redirect.to_owned();
        let cached: Option<String> = self.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = 'intrusion_source' AND key = ?1", [key], |r| r.get(0)))?)).await.ok().flatten();
        if let Some(v) = cached {
            return Ok(v.split_once('|').map(|(a, t)| (a.to_owned(), t.parse().unwrap_or(0))));
        }
        let v = via.to_owned();
        let local: Vec<(String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT from_id, at FROM coop_ledger WHERE to_id = ?1 AND cents = ?2 AND at <= ?3 AND from_id IS NOT NULL ORDER BY at DESC, block DESC LIMIT 20")?;
                let rows = st.query_map(rusqlite::params![v, cents, at], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| e.to_string())?;
        let mut found = local.into_iter().next();
        if found.is_none() {
            if !ask {
                return Err("pas encore recherché".into());
            }
            let urls = self.squid.read().unwrap_or_else(|e| e.into_inner()).clone();
            let query = json!({
                "query": "query($w:String!,$b:Int!){transfers(condition:{toId:$w},filter:{blockNumber:{lessThanOrEqualTo:$b}},orderBy:BLOCK_NUMBER_DESC,first:50){nodes{fromId amount timestamp}}}",
                "variables": { "w": via, "b": block },
            })
            .to_string();
            let mut last = "aucun indexeur (--squid-url)".to_owned();
            let mut answered = false;
            for url in &urls {
                match post_json(url, &query, &self.limits).await {
                    Ok(body) => {
                        let v: Json = serde_json::from_slice(&body).map_err(|e| format!("réponse illisible : {e}"))?;
                        let nodes = v["data"]["transfers"]["nodes"].as_array().ok_or("réponse sans data.transfers.nodes")?;
                        found = nodes
                            .iter()
                            .filter(|n| n["amount"].as_str().and_then(|a| a.parse::<i64>().ok()).or_else(|| n["amount"].as_i64()) == Some(cents))
                            .filter_map(|n| Some((n["fromId"].as_str()?.to_owned(), n["timestamp"].as_str().and_then(unix_of).map_or(at, |t| t as i64))))
                            .next();
                        answered = true;
                        break;
                    }
                    Err(e) => last = e.to_string(),
                }
            }
            if !answered {
                return Err(last);
            }
        }
        let value = found.as_ref().map_or_else(|| "?".to_owned(), |(a, t)| format!("{a}|{t}"));
        let _ = self.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('intrusion_source', ?1, ?2, ?3) ON CONFLICT(scope, key) DO NOTHING", vec![redirect.to_owned().into(), value.into(), Value::Integer(now() as i64)]).await;
        Ok(found)
    }

    /// Vigilance du Capitaine. Règle de l'UPlanet : tout virement reçu doit venir d'un portefeuille dont la primale (émetteur
    /// de son premier virement reçu) est `UPLANETNAME_G1` ; sinon il est renvoyé vers `INTRUSION`. Rend, par émetteur
    /// extérieur, les virements qui ont **déclenché un renvoi vers INTRUSION**, avec sa chaîne de primales (pour remonter à
    /// la source des faux Ẑen). Chaque portefeuille garde 1 Ğ1 de réserve : l'alerte ne porte que sur ce qui la dépasse.
    async fn vigilance(&self, wallets: &[Json]) -> Result<Json, String> {
        self.vigilance_with(wallets, MAX_PRIMAL_LOOKUPS).await
    }

    /// [`Self::vigilance`] avec `budget` questions à l'indexeur au plus.
    async fn vigilance_with(&self, wallets: &[Json], budget: usize) -> Result<Json, String> {
        let named = self.named().await?;
        let master = named.first().map(|(_, a)| a.clone()).unwrap_or_default();
        let Some(intrusion) = named.iter().find(|(n, _)| n == "INTRUSION").map(|(_, a)| a.clone()) else {
            return Ok(json!({ "alert": false, "intruders": [], "error": "portefeuille INTRUSION inconnu" }));
        };
        let coop: HashSet<String> = named.iter().map(|(_, a)| a.clone()).collect();
        let names: HashMap<String, String> = named.iter().map(|(n, a)| (a.clone(), n.clone())).collect();
        let members = self.member_addresses().await?;
        let (intr, m) = (intrusion.clone(), master.clone());
        let redirects: Vec<(String, i64, i64, String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT id, block, at, from_id, cents FROM coop_ledger WHERE to_id = ?1 AND from_id IS NOT NULL AND from_id <> ?2 ORDER BY at DESC LIMIT 200")?;
                let rows = st.query_map([intr, m], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| e.to_string())?;
        let mut budget = budget;
        // Les portefeuilles officiels d'abord : chacun doit avoir reçu sa primo TX de UPLANETNAME_G1, constaté sur la chaîne.
        let mut officials_bad = Vec::new();
        let mut officials_unverified = 0;
        for (name, a) in named.iter().skip(1) {
            let ask = budget > 0 && self.has_indexer();
            let p = self.primal(a, ask).await;
            if ask && p.is_ok() {
                budget = budget.saturating_sub(1);
            }
            match p {
                Ok(Some(x)) if x == master => {}
                Ok(Some(x)) => officials_bad.push(json!({ "name": name, "address": a, "primal": x })),
                // Rien reçu : pas encore ouvert (carte « Carburant » du cockpit), ni conforme ni étranger.
                Ok(None) | Err(_) => officials_unverified += 1,
            }
        }
        let mut by_sender: BTreeMap<String, (i64, i64, i64, Vec<String>)> = BTreeMap::new();
        let (mut unknown, mut pending) = (0i64, 0usize);
        let mut wrong: Vec<Json> = Vec::new();
        for (id, block, at, via, cents) in &redirects {
            let ask = budget > 0 && self.has_indexer();
            let source = self.intrusion_source(id, via, *cents, *at, *block, ask).await;
            if ask {
                budget -= 1;
            }
            match source {
                Ok(Some((sender, when))) => {
                    let label = names.get(via).cloned().unwrap_or_else(|| if members.contains(via) { "un membre".into() } else { via.clone() });
                    // Source de cette UPlanet (portefeuille coopératif, ou primale = maître) : renvoi à tort, pas une intrusion.
                    let ours = coop.contains(&sender) || {
                        let ask = budget > 0 && self.has_indexer();
                        let p = self.primal(&sender, ask).await;
                        if ask && p.is_ok() {
                            budget = budget.saturating_sub(1);
                        }
                        matches!(p, Ok(Some(ref x)) if *x == master)
                    };
                    if ours {
                        wrong.push(json!({ "source": names.get(&sender).cloned().unwrap_or_else(|| sender.clone()), "via": label, "cents": cents, "at": at }));
                        continue;
                    }
                    let e = by_sender.entry(sender).or_insert((0, 0, 0, Vec::new()));
                    e.0 += cents;
                    e.1 += 1;
                    e.2 = e.2.max(when);
                    if !e.3.contains(&label) {
                        e.3.push(label);
                    }
                }
                Ok(None) => unknown += cents,
                Err(_) => pending += 1,
            }
        }
        // Chaîne de primales : émetteur → sa primale → la primale de celle-ci… jusqu'au maître, à l'inconnu ou à 6 pas.
        let mut intruders = Vec::new();
        let mut order: Vec<_> = by_sender.into_iter().collect();
        order.sort_by_key(|(_, v)| std::cmp::Reverse(v.2));
        for (sender, (cents, count, last, via)) in order {
            let mut chain = vec![json!({ "address": sender })];
            let mut cur = sender.clone();
            for _ in 0..6 {
                let ask = budget > 0 && self.has_indexer();
                let step = self.primal(&cur, ask).await;
                if ask && step.is_ok() {
                    budget = budget.saturating_sub(1);
                }
                match step {
                    Ok(Some(p)) => {
                        let is_master = p == master;
                        chain.push(json!({ "address": p, "master": is_master }));
                        if is_master || chain.len() > 6 || chain.iter().filter(|c| c["address"] == p.as_str()).count() > 1 {
                            break;
                        }
                        cur = p;
                    }
                    Ok(None) => break,
                    Err(_) => {
                        chain.push(json!({ "address": null }));
                        break;
                    }
                }
            }
            intruders.push(json!({ "address": sender, "cents": cents, "count": count, "last_at": last, "via": via, "chain": chain }));
        }
        let intrusion_cents = wallets.iter().find(|w| w["name"] == "INTRUSION").and_then(|w| w["g1_cents"].as_i64());
        let above = intrusion_cents.map(|c| (c - RESERVE_CENTS).max(0));
        let alert = !intruders.is_empty() || above.is_some_and(|c| c > 0) || !officials_bad.is_empty();
        Ok(json!({ "alert": alert, "intrusion": { "address": intrusion, "g1_cents": intrusion_cents, "above_reserve_cents": above }, "intruders": intruders, "wrongful": wrong, "officials": { "checked": named.len().saturating_sub(1), "unverified": officials_unverified, "bad": officials_bad }, "unknown_source_cents": unknown, "pending": pending }))
    }

    /// Émetteurs déjà reconnus comme intrus (pour la carte du Capitaine).
    async fn known_intruders(&self) -> HashSet<String> {
        self.db
            .readers()
            .read(|c| {
                let mut st = c.prepare("SELECT value FROM kv WHERE scope = 'intrusion_source' AND value <> '?'")?;
                let rows = st.query_map([], |r| r.get::<_, String>(0))?.filter_map(Result::ok).filter_map(|v| v.split_once('|').map(|(a, _)| a.to_owned())).collect();
                Ok(rows)
            })
            .await
            .unwrap_or_default()
    }

    /// Portefeuilles, soldes, nom, charte et état de l'historique ; pour un Capitaine hébergé ici, la vigilance en plus.
    pub async fn overview(&self, viewer: &str) -> Result<Json, String> {
        let wallets = self.balances().await?;
        let vigilance = if self.is_captain(viewer).await { self.vigilance(&wallets).await.unwrap_or_else(|e| json!({ "error": e })) } else { Json::Null };
        let (count, first, last): (i64, Option<i64>, Option<i64>) = self.db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*), MIN(at), MAX(at) FROM coop_ledger", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?)).await.map_err(|e| e.to_string())?;
        let synced = self.kv(SCOPE, "synced_at".into()).await.and_then(|v| v.parse::<u64>().ok());
        let error = self.kv(SCOPE, "error".into()).await.filter(|e| !e.is_empty());
        Ok(json!({
            "charter": self.charter(viewer).await, "wallets": wallets, "vigilance": vigilance,
            "chain": self.chain.is_some(),
            "billets": match &self.billets { Some(b) => b.stats().await, None => Json::Null },
            // Partiel : pas d'indexeur, jamais lu, ou dernière lecture en échec.
            "history": { "indexer": self.has_indexer(), "partial": !self.has_indexer() || synced.is_none() || error.is_some(), "events": count, "first": first, "last": last, "synced_at": synced, "error": error },
        }))
    }

    /// Historique compact pour la carte : `nodes` (portefeuilles, `membres`, `extérieur`) et `events` = `[secondes, de, vers, centimes de Ğ1]`.
    pub async fn ledger(&self, viewer: &str, since: u64, limit: usize) -> Result<Json, String> {
        let wallets = self.named().await?;
        let limit = limit.clamp(1, MAX_EVENTS) as i64;
        let rows: Vec<(i64, Option<String>, Option<String>, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT at, from_id, to_id, cents FROM coop_ledger WHERE at >= ?1 ORDER BY at, block, id LIMIT ?2")?;
                let rows = st.query_map(rusqlite::params![since as i64, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| e.to_string())?;
        let members = self.member_addresses().await?;
        let billet_addrs = match &self.billets { Some(b) => b.addresses().await, None => HashSet::new() };
        let mut nodes: Vec<Json> = wallets.iter().map(|(n, _)| json!({ "id": n, "kind": "wallet" })).collect();
        let index: BTreeMap<&str, usize> = wallets.iter().enumerate().map(|(i, (_, a))| (a.as_str(), i)).collect();
        let (billets_ix, members_ix, outside_ix) = (nodes.len(), nodes.len() + 1, nodes.len() + 2);
        nodes.push(json!({ "id": "billets", "kind": "billets" }));
        nodes.push(json!({ "id": "membres", "kind": "members" }));
        nodes.push(json!({ "id": "extérieur", "kind": "outside" }));
        // Le Capitaine voit chaque intrus à part (son adresse complète : il doit pouvoir enquêter) ; un membre, jamais.
        let mut intrus: BTreeMap<String, usize> = BTreeMap::new();
        if self.is_captain(viewer).await {
            for a in self.known_intruders().await {
                intrus.insert(a.clone(), nodes.len());
                nodes.push(json!({ "id": a, "kind": "intrus" }));
            }
        }
        let place = |a: &Option<String>| a.as_deref().map_or(outside_ix, |a| index.get(a).or_else(|| intrus.get(a)).copied().unwrap_or_else(|| if billet_addrs.contains(a) { billets_ix } else if members.contains(a) { members_ix } else { outside_ix }));
        let events: Vec<Json> = rows.iter().map(|(at, f, t, c)| json!([at, place(f), place(t), c])).collect();
        Ok(json!({ "nodes": nodes, "events": events, "truncated": rows.len() as i64 == limit }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_of_the_indexer_become_unix_seconds() {
        assert_eq!(unix_of("1970-01-01T00:00:00"), Some(0));
        assert_eq!(unix_of("2000-03-01T00:00:00.000Z"), Some(951_868_800));
        assert_eq!(unix_of("2024-02-29T12:30:45+00:00"), Some(1_709_209_845));
        assert_eq!(unix_of("hier"), None);
        assert_eq!(unix_of("2024-13-01T00:00:00"), None);
    }

    #[test]
    fn a_page_is_read_and_an_indexer_error_is_not_an_empty_history() {
        let body = br#"{"data":{"transfers":{"pageInfo":{"hasNextPage":true,"endCursor":"abc"},"nodes":[{"id":"t1","blockNumber":7,"timestamp":"2025-01-01T00:00:00","amount":"5000","fromId":"g1a","toId":null}]}}}"#;
        let p = parse_page(body).unwrap();
        assert_eq!(p.next.as_deref(), Some("abc"));
        assert_eq!(p.transfers, vec![Transfer { id: "t1".into(), block: 7, at: 1_735_689_600, from: Some("g1a".into()), to: None, cents: 5000 }]);
        // Virement de la Ğ1 v1 repris à la genèse : bloc négatif, réponse réelle de cinq indexeurs (2026-11-03).
        let genesis = r#"{"data":{"transfers":{"pageInfo":{"hasNextPage":false},"nodes":[{"amount":"99300","blockNumber":-1868,"fromId":"g1Kj6f","id":"genesis-tx_n°812049","timestamp":"2026-02-28T21:54:56+00:00","toId":"g1LbTd"}]}}}"#.as_bytes();
        assert_eq!(parse_page(genesis).unwrap().transfers[0].block, -1868);
        assert!(parse_page(br#"{"errors":[{"message":"boom"}]}"#).unwrap_err().contains("boom"));
        assert!(parse_page(br#"{"data":{"transfers":{"nodes":[{"id":"x"}]}}}"#).is_err());
        let last = br#"{"data":{"transfers":{"pageInfo":{"hasNextPage":false,"endCursor":"z"},"nodes":[]}}}"#;
        assert_eq!(parse_page(last).unwrap().next, None);
    }

    #[test]
    fn the_query_names_every_wallet_and_the_cursor() {
        let q: Json = serde_json::from_str(&ledger_query(&["g1a".into(), "g1b".into()], Some("c1"))).unwrap();
        assert_eq!(q["variables"]["w"], json!(["g1a", "g1b"]));
        assert_eq!(q["variables"]["a"], "c1");
    }

    type Pool = astro_duniter::RpcPool<astro_duniter::SubxtConnector>;

    async fn rig(tag: &str) -> (std::path::PathBuf, Constellation<Pool>, Vec<(String, String)>) {
        use astro_duniter::{Account, Ed25519Signer};
        let dir = std::env::temp_dir().join(format!("astroportd-constellation-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let coop = crate::coop_wallets::Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let wallets = Arc::new(Wallets::fixed(db.clone(), Ed25519Signer::from_seed([0x44; 32]), coop).with_master(Ed25519Signer::from_seed([0x01; 32])));
        let c = Constellation::new(db, None, wallets, "0".repeat(64), Vec::new());
        let named = c.named().await.unwrap();
        (dir, c, named)
    }

    #[tokio::test]
    async fn the_ledger_names_wallets_and_hides_member_addresses() {
        let (dir, c, named) = rig("ledger").await;
        assert_eq!(named[0].0, "UPLANETNAME_G1", "le maître d'abord");
        let (master, treasury) = (named[0].1.clone(), named.iter().find(|(n, _)| n == "TREASURY").unwrap().1.clone());
        c.db.writer().execute("INSERT INTO identities(hex, role, email, g1pub, hosted) VALUES (?1, 'multipass', 'a@exemple.org', 'g1membre', 1)", vec!["aa".repeat(32).into()]).await.unwrap();
        let rows = [("t1", 10, 1_700_000_000, Some("g1dehors"), &master, 5000), ("t2", 11, 1_700_000_100, Some(master.as_str()), &treasury, 1000), ("t3", 12, 1_700_000_200, Some(treasury.as_str()), &"g1membre".to_owned(), 300)];
        for (id, block, at, from, to, cents) in rows {
            c.db.writer().execute("INSERT INTO coop_ledger(id, block, at, from_id, to_id, cents) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", vec![id.to_owned().into(), Value::Integer(block), Value::Integer(at), from.map_or(Value::Null, |f| f.to_owned().into()), to.clone().into(), Value::Integer(cents)]).await.unwrap();
        }
        let v = c.ledger("", 0, 100).await.unwrap();
        let nodes = v["nodes"].as_array().unwrap();
        let (members, outside) = (nodes.len() - 2, nodes.len() - 1);
        assert_eq!(nodes[members]["id"], "membres");
        let ev = v["events"].as_array().unwrap();
        assert_eq!(ev[0], json!([1_700_000_000, outside, 0, 5000]), "un compte inconnu est « extérieur »");
        assert_eq!(ev[2][2], members, "un compte hébergé ici est « membres »");
        assert!(!v.to_string().contains("g1membre") && !v.to_string().contains("g1dehors"), "aucune adresse de membre ni d'extérieur");
        assert_eq!(c.ledger("", 1_700_000_050, 100).await.unwrap()["events"].as_array().unwrap().len(), 2, "depuis une date");
        let o = c.overview("").await.unwrap();
        assert_eq!(o["history"]["partial"], true, "sans indexeur : historique partiel");
        assert_eq!(o["history"]["events"], 3);
        assert!(o["charter"]["text"].as_str().unwrap().contains("trois tiers"), "charte type par défaut");
        assert_eq!(o["wallets"][0]["primal"], "master");
        assert!(o["wallets"].as_array().unwrap().iter().skip(1).all(|w| w["primal"] == "pending"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_captain_sees_only_senders_whose_funds_were_sent_on_to_intrusion_with_their_primal_chain() {
        use astro_duniter::{Account, Ed25519Signer};
        let dir = std::env::temp_dir().join(format!("astroportd-constellation-vigil-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let coop = crate::coop_wallets::Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let wallets = Arc::new(Wallets::fixed(db.clone(), Ed25519Signer::from_seed([0x44; 32]), coop).with_master(Ed25519Signer::from_seed([0x01; 32])).with_target("INTRUSION", Account([9; 32])));
        let c: Constellation<Pool> = Constellation::new(db, None, wallets, "0".repeat(64), Vec::new());
        let named = c.named().await.unwrap();
        let addr = |n: &str| named.iter().find(|(x, _)| x == n).unwrap().1.clone();
        let (master, treasury, intrusion) = (named[0].1.clone(), addr("TREASURY"), addr("INTRUSION"));
        let cap = "cc".repeat(32);
        c.db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'captain', 1)", vec![cap.clone().into()]).await.unwrap();
        // g1intrus (primale g1source, elle-même issue de g1racine) envoie 37 Ğ1 à TREASURY, renvoyés vers INTRUSION ;
        // g1ami (même UPlanet) envoie 5 Ğ1, jamais renvoyés ; le maître verse la réserve primale de 1 Ğ1 à INTRUSION.
        for (who, primal) in [("g1intrus", "g1source"), ("g1source", "g1racine"), ("g1ami", master.as_str())] {
            c.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, 0)", vec![crate::funds::PRIMAL_SCOPE.to_owned().into(), who.to_owned().into(), primal.to_owned().into()]).await.unwrap();
        }
        // g1ami envoie aussi 2,8 Ğ1 à ASSETS, qui les renvoie à tort vers INTRUSION : ce n'est pas une intrusion.
        let assets = addr("ASSETS");
        let rows = [("p0", master.as_str(), intrusion.as_str(), 100), ("v1", "g1ami", treasury.as_str(), 500), ("v2", "g1intrus", treasury.as_str(), 3700), ("r2", treasury.as_str(), intrusion.as_str(), 3700), ("v3", "g1ami", assets.as_str(), 280), ("r3", assets.as_str(), intrusion.as_str(), 280)];
        for (i, (id, from, to, cents)) in rows.into_iter().enumerate() {
            c.db.writer().execute("INSERT INTO coop_ledger(id, block, at, from_id, to_id, cents) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", vec![id.to_owned().into(), Value::Integer(i as i64), Value::Integer(1_700_000_000 + i as i64), from.to_owned().into(), to.to_owned().into(), Value::Integer(cents)]).await.unwrap();
        }
        assert!(c.overview("").await.unwrap()["vigilance"].is_null(), "un membre ne voit pas la vigilance");
        let v = c.overview(&cap).await.unwrap()["vigilance"].clone();
        let list = v["intruders"].as_array().unwrap();
        assert_eq!(list.len(), 1, "seul l'émetteur renvoyé vers INTRUSION : {v}");
        assert_eq!((list[0]["address"].as_str(), list[0]["cents"].as_i64(), list[0]["via"][0].as_str()), (Some("g1intrus"), Some(3700), Some("TREASURY")));
        let chain: Vec<&str> = list[0]["chain"].as_array().unwrap().iter().filter_map(|x| x["address"].as_str()).collect();
        assert_eq!(chain, ["g1intrus", "g1source", "g1racine"], "on remonte de primale en primale");
        assert_eq!(v["alert"], true);
        assert!(v["officials"]["bad"].as_array().unwrap().is_empty(), "aucun portefeuille officiel n'a de primale étrangère ici");
        let wrong = v["wrongful"].as_array().unwrap();
        assert_eq!((wrong.len(), wrong[0]["source"].as_str(), wrong[0]["via"].as_str(), wrong[0]["cents"].as_i64()), (1, Some("g1ami"), Some("ASSETS"), Some(280)), "renvoi à tort signalé à part : {v}");
        // La carte du Capitaine montre l'intrus à part ; celle d'un membre, jamais.
        let led = c.ledger(&cap, 0, 100).await.unwrap();
        assert!(led["nodes"].as_array().unwrap().iter().any(|n| n["kind"] == "intrus" && n["id"] == "g1intrus"));
        assert!(!c.ledger("", 0, 100).await.unwrap().to_string().contains("g1intrus"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_official_wallet_is_verified_on_chain_and_a_foreign_primal_is_an_alert() {
        let (dir, c, named) = rig("officials").await;
        let (master, treasury, rnd) = (named[0].1.clone(), named.iter().find(|(n, _)| n == "TREASURY").unwrap().1.clone(), named.iter().find(|(n, _)| n == "RND").unwrap().1.clone());
        for (who, primal) in [(treasury.as_str(), master.as_str()), (rnd.as_str(), "g1ailleurs")] {
            c.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, 0)", vec![crate::funds::PRIMAL_SCOPE.to_owned().into(), who.to_owned().into(), primal.to_owned().into()]).await.unwrap();
        }
        let o = c.overview("").await.unwrap();
        let status = |n: &str| o["wallets"].as_array().unwrap().iter().find(|w| w["name"] == n).unwrap()["primal"].as_str().unwrap().to_owned();
        assert_eq!((status("TREASURY").as_str(), status("RND").as_str(), status("IMPOT").as_str()), ("verified", "foreign", "pending"), "la chaîne prime sur le marqueur local");
        let v = c.vigilance_with(&[], 0).await.unwrap();
        // Sans INTRUSION dans ce jeu, la vigilance le dit ; avec, le portefeuille étranger lève l'alerte (test suivant).
        assert!(v["error"].is_string());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_one_g1_reserve_of_intrusion_is_never_an_alert() {
        use astro_duniter::{Account, Ed25519Signer};
        let dir = std::env::temp_dir().join(format!("astroportd-constellation-reserve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let coop = crate::coop_wallets::Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let wallets = Arc::new(Wallets::fixed(db.clone(), Ed25519Signer::from_seed([0x44; 32]), coop).with_master(Ed25519Signer::from_seed([0x01; 32])).with_target("INTRUSION", Account([9; 32])));
        let c: Constellation<Pool> = Constellation::new(db, None, wallets, "0".repeat(64), Vec::new());
        let reserve = [json!({ "name": "INTRUSION", "g1_cents": 100 })];
        assert_eq!(c.vigilance(&reserve).await.unwrap()["alert"], false, "1 Ğ1 = la réserve de tout portefeuille");
        // Un portefeuille officiel dont la primale ne vient pas du maître : alerte, même sans intrus.
        let rnd = c.named().await.unwrap().iter().find(|(n, _)| n == "RND").unwrap().1.clone();
        c.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, 'g1ailleurs', 0)", vec![crate::funds::PRIMAL_SCOPE.to_owned().into(), rnd.into()]).await.unwrap();
        let v = c.vigilance(&reserve).await.unwrap();
        assert_eq!((v["alert"].as_bool(), v["officials"]["bad"][0]["name"].as_str()), (Some(true), Some("RND")));
        let more = [json!({ "name": "INTRUSION", "g1_cents": 3800 })];
        let v = c.vigilance(&more).await.unwrap();
        assert_eq!((v["alert"].as_bool(), v["intrusion"]["above_reserve_cents"].as_i64()), (Some(true), Some(3700)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn without_an_indexer_the_sync_says_so() {
        let (dir, c, _) = rig("sync").await;
        assert_eq!(c.sync_once().await.unwrap_err(), "aucun indexeur");
        let _ = std::fs::remove_dir_all(dir);
    }
}
