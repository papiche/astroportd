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
            wait = SYNC_EVERY;
        }
    }

    async fn balances(&self) -> Result<Vec<Json>, String> {
        if let Some((at, v)) = self.balances.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(at, _)| at.elapsed() < BALANCE_TTL) {
            let _ = at;
            return Ok(v.clone());
        }
        let wallets = self.named().await?;
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
            // Le portefeuille maître finance les autres : il n'a pas de transaction primale.
            let primal = if name == "UPLANETNAME_G1" { "master" } else { markers.get(&name).map_or("pending", |m| if matches!(m.as_str(), "paid" | "sending") { "done" } else { "pending" }) };
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

    /// Portefeuilles, soldes, nom, charte et état de l'historique.
    pub async fn overview(&self, viewer: &str) -> Result<Json, String> {
        let wallets = self.balances().await?;
        let (count, first, last): (i64, Option<i64>, Option<i64>) = self.db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*), MIN(at), MAX(at) FROM coop_ledger", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?)).await.map_err(|e| e.to_string())?;
        let synced = self.kv(SCOPE, "synced_at".into()).await.and_then(|v| v.parse::<u64>().ok());
        let error = self.kv(SCOPE, "error".into()).await.filter(|e| !e.is_empty());
        Ok(json!({
            "charter": self.charter(viewer).await, "wallets": wallets,
            "chain": self.chain.is_some(),
            "billets": match &self.billets { Some(b) => b.stats().await, None => Json::Null },
            // Partiel : pas d'indexeur, jamais lu, ou dernière lecture en échec.
            "history": { "indexer": self.has_indexer(), "partial": !self.has_indexer() || synced.is_none() || error.is_some(), "events": count, "first": first, "last": last, "synced_at": synced, "error": error },
        }))
    }

    /// Historique compact pour la carte : `nodes` (portefeuilles, `membres`, `extérieur`) et `events` = `[secondes, de, vers, centimes de Ğ1]`.
    pub async fn ledger(&self, since: u64, limit: usize) -> Result<Json, String> {
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
        let members: HashSet<String> = self
            .db
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
            .map_err(|e| e.to_string())?;
        let billet_addrs = match &self.billets { Some(b) => b.addresses().await, None => HashSet::new() };
        let mut nodes: Vec<Json> = wallets.iter().map(|(n, _)| json!({ "id": n, "kind": "wallet" })).collect();
        let index: BTreeMap<&str, usize> = wallets.iter().enumerate().map(|(i, (_, a))| (a.as_str(), i)).collect();
        let (billets_ix, members_ix, outside_ix) = (nodes.len(), nodes.len() + 1, nodes.len() + 2);
        nodes.push(json!({ "id": "billets", "kind": "billets" }));
        nodes.push(json!({ "id": "membres", "kind": "members" }));
        nodes.push(json!({ "id": "extérieur", "kind": "outside" }));
        let place = |a: &Option<String>| a.as_deref().map_or(outside_ix, |a| index.get(a).copied().unwrap_or_else(|| if billet_addrs.contains(a) { billets_ix } else if members.contains(a) { members_ix } else { outside_ix }));
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
        let v = c.ledger(0, 100).await.unwrap();
        let nodes = v["nodes"].as_array().unwrap();
        let (members, outside) = (nodes.len() - 2, nodes.len() - 1);
        assert_eq!(nodes[members]["id"], "membres");
        let ev = v["events"].as_array().unwrap();
        assert_eq!(ev[0], json!([1_700_000_000, outside, 0, 5000]), "un compte inconnu est « extérieur »");
        assert_eq!(ev[2][2], members, "un compte hébergé ici est « membres »");
        assert!(!v.to_string().contains("g1membre") && !v.to_string().contains("g1dehors"), "aucune adresse de membre ni d'extérieur");
        assert_eq!(c.ledger(1_700_000_050, 100).await.unwrap()["events"].as_array().unwrap().len(), 2, "depuis une date");
        let o = c.overview("").await.unwrap();
        assert_eq!(o["history"]["partial"], true, "sans indexeur : historique partiel");
        assert_eq!(o["history"]["events"], 3);
        assert!(o["charter"]["text"].as_str().unwrap().contains("trois tiers"), "charte type par défaut");
        assert_eq!(o["wallets"][0]["primal"], "master");
        assert!(o["wallets"].as_array().unwrap().iter().skip(1).all(|w| w["primal"] == "pending"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn without_an_indexer_the_sync_says_so() {
        let (dir, c, _) = rig("sync").await;
        assert_eq!(c.sync_once().await.unwrap_err(), "aucun indexeur");
        let _ = std::fs::remove_dir_all(dir);
    }
}
