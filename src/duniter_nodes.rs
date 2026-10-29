//! Découverte automatique des nœuds Duniter v2s (RPC) et des indexeurs squid : port de `tools/duniter_getnode.sh`.
//!
//! Sources, par ordre : le cache (une heure), les amorces codées en dur, `duniter_peerings` d'un nœud qui répond, le
//! fichier réseau officiel (`git.duniter.org/nodes/networks`) et les annonces on-chain (`duniter endpoint g1 add …`).
//! Chaque candidat est sondé (RPC : `system_chain` et `system_syncState`, à moins de 10 blocs de la pointe ; squid :
//! `squidStatus`), puis retenu par latence croissante. Si aucun ne répond, les amorces sont gardées (comme le script) et
//! l'état est marqué dégradé.
//!
//! Les requêtes passent par le client HTTPS durci de la station (adresses publiques seulement, tailles bornées).

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astro_db::Db;
use serde_json::{Value, json};

use crate::fetch::{FetchLimits, fetch, post_json};

/// Nœuds RPC d'amorce.
pub const BOOTSTRAP_RPC: [&str; 6] = ["wss://g1.1000i100.fr/ws", "wss://g1-v2s.cgeek.fr", "wss://g1.coinduf.eu", "wss://g1.gyroi.de", "wss://g1.p2p.legal/ws", "wss://rpc.duniter.org"];
/// Indexeurs squid d'amorce.
pub const BOOTSTRAP_SQUID: [&str; 5] = [
    "https://g1-squid.axiom-team.fr/v1/graphql",
    "https://squid.g1.gyroi.de/v1/graphql",
    "https://squid.g1.coinduf.eu/v1/graphql",
    "https://indexer.duniter.org/v1/graphql",
    "https://g1-squid.cgeek.fr/v1/graphql",
];
/// Fichier réseau officiel.
pub const NETWORK_JSON_URL: &str = "https://git.duniter.org/nodes/networks/-/raw/main/g1.json";
/// Durée de validité du cache.
pub const CACHE_TTL_SECS: i64 = 3600;
/// Nœuds retenus par type.
const KEEP: usize = 10;
/// Retard maximal toléré d'un nœud RPC sur la pointe de la chaîne.
const MAX_LAG: i64 = 10;
/// Sondes simultanées.
const PARALLEL: usize = 8;

/// Un nœud retenu.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// Adresse.
    pub url: String,
    /// Latence de la sonde, en millisecondes (9999 : amorce non sondée).
    pub latency_ms: u64,
    /// Bloc (RPC) ou hauteur d'indexation (squid).
    pub height: u64,
}

/// Résultat de la découverte.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Nodes {
    /// Nœuds RPC, du plus rapide au plus lent.
    pub rpc: Vec<Scored>,
    /// Indexeurs squid, du plus rapide au plus lent.
    pub squid: Vec<Scored>,
    /// Aucun candidat n'a répondu : ce sont les amorces, non sondées.
    pub degraded: bool,
}

impl Nodes {
    /// Adresses RPC.
    pub fn rpc_urls(&self) -> Vec<String> {
        self.rpc.iter().map(|n| n.url.clone()).collect()
    }

    /// Adresses des squid.
    pub fn squid_urls(&self) -> Vec<String> {
        self.squid.iter().map(|n| n.url.clone()).collect()
    }
}

/// Accès HTTP(S) : remplacé par un simulateur dans les tests.
pub trait Http: Send + Sync + 'static {
    /// `GET` ; `Err` : réseau ou statut d'erreur.
    fn get(&self, url: &str) -> impl Future<Output = Result<Vec<u8>, String>> + Send;
    /// `POST` JSON.
    fn post(&self, url: &str, json: &str) -> impl Future<Output = Result<Vec<u8>, String>> + Send;
}

/// Client réel.
pub struct RealHttp {
    limits: FetchLimits,
}

impl Default for RealHttp {
    fn default() -> Self {
        let mut limits = FetchLimits::new(512 * 1024);
        limits.timeout = Duration::from_secs(5);
        Self { limits }
    }
}

impl Http for RealHttp {
    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        use tokio::io::AsyncReadExt;
        let mut got = fetch(url, &self.limits).await.map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        got.body.read_to_end(&mut out).await.map_err(|e| e.to_string())?;
        if out.len() as u64 > self.limits.max_bytes {
            return Err("réponse trop grosse".into());
        }
        Ok(out)
    }

    async fn post(&self, url: &str, json: &str) -> Result<Vec<u8>, String> {
        post_json(url, json, &self.limits).await.map_err(|e| e.to_string())
    }
}

/// `wss://hôte/ws` → `https://hôte` : le JSON-RPC répond aussi en HTTPS.
pub fn rpc_http_url(ws: &str) -> String {
    let url = ws.replacen("wss://", "https://", 1).replacen("ws://", "http://", 1);
    url.strip_suffix("/ws").unwrap_or(&url).to_owned()
}

fn rpc_call(method: &str, id: u32) -> String {
    json!({ "jsonrpc": "2.0", "method": method, "params": [], "id": id }).to_string()
}

fn parse(body: &[u8]) -> Option<Value> {
    serde_json::from_slice(body).ok()
}

/// Adresses d'un `duniter_peerings` : `(rpc, squid)`.
pub fn parse_peerings(body: &[u8]) -> (Vec<String>, Vec<String>) {
    let Some(v) = parse(body) else { return Default::default() };
    let (mut rpc, mut squid) = (Vec::new(), Vec::new());
    for ep in v["result"]["peerings"].as_array().into_iter().flatten().flat_map(|p| p["endpoints"].as_array().into_iter().flatten()) {
        let (Some(proto), Some(addr)) = (ep["protocol"].as_str(), ep["address"].as_str()) else { continue };
        match proto {
            "rpc" => rpc.push(addr.to_owned()),
            "squid" => squid.push(addr.to_owned()),
            _ => {}
        }
    }
    (rpc, squid)
}

/// Adresses du fichier réseau : `{"endpoints":{"rpc":[…],"squid":[…]}}`, `{"rpc":[…],"squid":[…]}` ou une simple liste.
pub fn parse_network(body: &[u8]) -> (Vec<String>, Vec<String>) {
    let Some(v) = parse(body) else { return Default::default() };
    let strings = |x: &Value| x.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_owned)).collect::<Vec<_>>();
    let root = if v["endpoints"].is_object() { &v["endpoints"] } else { &v };
    let (rpc, squid) = (strings(&root["rpc"]), strings(&root["squid"]));
    if rpc.is_empty() && squid.is_empty() { (strings(&v), Vec::new()) } else { (rpc, squid) }
}

/// Annonces on-chain `duniter endpoint g1 add <rpc|squid> <url>` : `(rpc, squid)`.
pub fn parse_remarks(body: &[u8]) -> (Vec<String>, Vec<String>) {
    let Some(v) = parse(body) else { return Default::default() };
    let (mut rpc, mut squid) = (Vec::new(), Vec::new());
    for msg in v["data"]["txComments"]["nodes"].as_array().into_iter().flatten().filter_map(|n| n["message"].as_str()) {
        let words: Vec<&str> = msg.split_whitespace().collect();
        if words.len() < 6 || words[..4] != ["duniter", "endpoint", "g1", "add"] {
            continue;
        }
        match words[4] {
            "rpc" => rpc.push(words[5].to_owned()),
            "squid" => squid.push(words[5].to_owned()),
            _ => {}
        }
    }
    (rpc, squid)
}

/// Adresse utilisable : `wss://`/`ws://` pour un RPC, `https://` pour un squid.
fn usable(url: &str, rpc: bool) -> bool {
    if rpc { url.starts_with("wss://") } else { url.starts_with("https://") }
}

/// Sonde un nœud RPC : `(latence, bloc courant)` s'il est de la Ğ1 et à moins de [`MAX_LAG`] blocs de la pointe.
pub async fn check_rpc<H: Http>(http: &H, url: &str) -> Option<(u64, u64)> {
    let http_url = rpc_http_url(url);
    let started = Instant::now();
    let chain = parse(&http.post(&http_url, &rpc_call("system_chain", 1)).await.ok()?)?;
    chain["result"].as_str().filter(|c| !c.is_empty())?;
    let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX).max(1);
    let sync = parse(&http.post(&http_url, &rpc_call("system_syncState", 2)).await.ok()?)?;
    let (current, highest) = (sync["result"]["currentBlock"].as_i64().unwrap_or(0), sync["result"]["highestBlock"].as_i64().unwrap_or(0));
    (highest - current <= MAX_LAG && current > 0).then_some((latency, u64::try_from(current).unwrap_or(0)))
}

/// Sonde un indexeur : `(latence, hauteur)` s'il répond `squidStatus`.
pub async fn check_squid<H: Http>(http: &H, url: &str) -> Option<(u64, u64)> {
    let started = Instant::now();
    let body = http.post(url, r#"{"query":"{ squidStatus { height } }"}"#).await.ok()?;
    let height = parse(&body)?["data"]["squidStatus"]["height"].as_u64()?;
    Some((u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX).max(1), height))
}

async fn probe_all<H: Http, F, Fut>(http: &Arc<H>, urls: Vec<String>, check: F) -> Vec<Scored>
where
    F: Fn(Arc<H>, String) -> Fut + Copy,
    Fut: Future<Output = Option<(u64, u64)>>,
{
    let mut out = Vec::new();
    for chunk in urls.chunks(PARALLEL) {
        let results = futures_util::future::join_all(chunk.iter().map(|u| {
            let (h, u) = (http.clone(), u.clone());
            async move { check(h, u.clone()).await.map(|(latency_ms, height)| Scored { url: u, latency_ms, height }) }
        }))
        .await;
        out.extend(results.into_iter().flatten());
        if out.len() >= KEEP * 2 {
            break;
        }
    }
    out.sort_by_key(|n| n.latency_ms);
    out.truncate(KEEP);
    out
}

/// Découverte complète (réseau). Jamais vide : à défaut de réponse, les amorces (`degraded`).
pub async fn discover<H: Http>(http: Arc<H>) -> Nodes {
    let (mut rpc, mut squid): (BTreeSet<String>, BTreeSet<String>) = (BOOTSTRAP_RPC.iter().map(|s| (*s).to_owned()).collect(), BOOTSTRAP_SQUID.iter().map(|s| (*s).to_owned()).collect());
    // 1. Pairs connus d'un nœud d'amorce qui répond.
    for boot in BOOTSTRAP_RPC {
        if let Ok(body) = http.post(&rpc_http_url(boot), &rpc_call("duniter_peerings", 1)).await {
            let (r, s) = parse_peerings(&body);
            if !r.is_empty() || !s.is_empty() {
                rpc.extend(r);
                squid.extend(s);
                break;
            }
        }
    }
    // 2. Fichier réseau officiel.
    if let Ok(body) = http.get(NETWORK_JSON_URL).await {
        let (r, s) = parse_network(&body);
        rpc.extend(r);
        squid.extend(s);
    }
    // 3. Annonces on-chain, lues par le premier indexeur d'amorce qui répond.
    let query = r#"{"query":"{ txComments(filter:{message:{startsWith:\"duniter endpoint g1 add \"}}) { nodes { message } } }"}"#;
    for boot in BOOTSTRAP_SQUID {
        if let Ok(body) = http.post(boot, query).await {
            let (r, s) = parse_remarks(&body);
            rpc.extend(r);
            squid.extend(s);
            break;
        }
    }
    let rpc: Vec<String> = rpc.into_iter().filter(|u| usable(u, true)).collect();
    let squid: Vec<String> = squid.into_iter().filter(|u| usable(u, false)).collect();
    let (rpc_ok, squid_ok) = tokio::join!(probe_all(&http, rpc, |h, u| async move { check_rpc(h.as_ref(), &u).await }), probe_all(&http, squid, |h, u| async move { check_squid(h.as_ref(), &u).await }));
    let degraded = rpc_ok.is_empty() || squid_ok.is_empty();
    let fallback = |list: &[&str]| list.iter().map(|u| Scored { url: (*u).to_owned(), latency_ms: 9999, height: 0 }).collect::<Vec<_>>();
    Nodes { rpc: if rpc_ok.is_empty() { fallback(&BOOTSTRAP_RPC) } else { rpc_ok }, squid: if squid_ok.is_empty() { fallback(&BOOTSTRAP_SQUID) } else { squid_ok }, degraded }
}

const SCOPE: &str = "duniter_nodes";

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Cache dans `kv` (jamais purgé par la nuit) : liste valide pendant [`CACHE_TTL_SECS`].
pub async fn cached(db: &Db) -> Option<Nodes> {
    let (value, at): (String, i64) = db.readers().read(|c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value, updated_at FROM kv WHERE scope = ?1 AND key = 'list'", [SCOPE], |r| Ok((r.get(0)?, r.get(1)?)))).ok().flatten())).await.ok().flatten()?;
    if now() - at > CACHE_TTL_SECS {
        return None;
    }
    let v: Value = serde_json::from_str(&value).ok()?;
    let list = |k: &str| -> Vec<Scored> { v[k].as_array().into_iter().flatten().filter_map(|n| Some(Scored { url: n["url"].as_str()?.to_owned(), latency_ms: n["latency_ms"].as_u64()?, height: n["height"].as_u64()? })).collect() };
    let nodes = Nodes { rpc: list("rpc"), squid: list("squid"), degraded: v["degraded"].as_bool().unwrap_or(false) };
    (!nodes.rpc.is_empty() && !nodes.squid.is_empty() && !nodes.degraded).then_some(nodes)
}

/// Enregistre la liste dans le cache.
pub async fn store(db: &Db, nodes: &Nodes) {
    let list = |l: &[Scored]| l.iter().map(|n| json!({ "url": n.url, "latency_ms": n.latency_ms, "height": n.height })).collect::<Vec<_>>();
    let doc = json!({ "rpc": list(&nodes.rpc), "squid": list(&nodes.squid), "degraded": nodes.degraded });
    let _ = db
        .writer()
        .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, 'list', ?2, ?3) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![SCOPE.to_owned().into(), doc.to_string().into(), now().into()])
        .await;
}

/// Liste à jour : le cache s'il est frais, sinon la découverte (puis mise en cache si elle a abouti).
pub async fn nodes<H: Http>(db: &Db, http: Arc<H>) -> Nodes {
    if let Some(n) = cached(db).await {
        return n;
    }
    let found = discover(http).await;
    if !found.degraded {
        store(db, &found).await;
    }
    found
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// Réseau simulé : chaque URL (ou `url|méthode`) a sa réponse ; les autres échouent.
    #[derive(Default)]
    struct Fake(Mutex<HashMap<String, String>>, Mutex<Vec<String>>);

    impl Fake {
        fn answer(&self, key: &str, body: &str) {
            self.0.lock().unwrap().insert(key.to_owned(), body.to_owned());
        }
    }

    impl Http for Fake {
        async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.1.lock().unwrap().push(format!("GET {url}"));
            self.0.lock().unwrap().get(url).map(|b| b.clone().into_bytes()).ok_or_else(|| "injoignable".into())
        }
        async fn post(&self, url: &str, json: &str) -> Result<Vec<u8>, String> {
            let method = serde_json::from_str::<Value>(json).ok().and_then(|v| v["method"].as_str().map(str::to_owned)).unwrap_or_else(|| if json.contains("squidStatus") { "squidStatus".into() } else { "remarks".into() });
            self.1.lock().unwrap().push(format!("POST {url} {method}"));
            self.0.lock().unwrap().get(&format!("{url}|{method}")).map(|b| b.clone().into_bytes()).ok_or_else(|| "injoignable".into())
        }
    }

    fn healthy_rpc(fake: &Fake, ws: &str, block: u64, lag: u64) {
        let http = rpc_http_url(ws);
        fake.answer(&format!("{http}|system_chain"), r#"{"result":"Ğ1"}"#);
        fake.answer(&format!("{http}|system_syncState"), &format!(r#"{{"result":{{"currentBlock":{block},"highestBlock":{}}}}}"#, block + lag));
    }

    #[test]
    fn rpc_addresses_are_turned_into_their_https_form() {
        assert_eq!(rpc_http_url("wss://g1.p2p.legal/ws"), "https://g1.p2p.legal");
        assert_eq!(rpc_http_url("wss://g1.gyroi.de"), "https://g1.gyroi.de");
        assert_eq!(rpc_http_url("ws://127.0.0.1:9944"), "http://127.0.0.1:9944");
    }

    #[test]
    fn the_three_discovery_sources_are_read() {
        let (rpc, squid) = parse_peerings(br#"{"result":{"peerings":[{"endpoints":[{"protocol":"rpc","address":"wss://a/ws"},{"protocol":"squid","address":"https://s/v1/graphql"},{"protocol":"p2p","address":"x"}]},{"endpoints":[]}]}}"#);
        assert_eq!((rpc, squid), (vec!["wss://a/ws".to_owned()], vec!["https://s/v1/graphql".to_owned()]));
        assert_eq!(parse_network(br#"{"endpoints":{"rpc":["wss://b"],"squid":["https://t"]}}"#), (vec!["wss://b".to_owned()], vec!["https://t".to_owned()]));
        assert_eq!(parse_network(br#"{"rpc":["wss://c"]}"#).0, ["wss://c"]);
        assert_eq!(parse_network(br#"["wss://d","wss://e"]"#).0, ["wss://d", "wss://e"], "simple liste");
        let (rpc, squid) = parse_remarks(br#"{"data":{"txComments":{"nodes":[{"message":"duniter endpoint g1 add rpc wss://r"},{"message":"duniter endpoint g1 add squid https://q"},{"message":"autre chose"},{"message":"duniter endpoint g1 add web https://w"}]}}}"#);
        assert_eq!((rpc, squid), (vec!["wss://r".to_owned()], vec!["https://q".to_owned()]));
        for junk in [&b"<html>"[..], b"{}", b""] {
            assert_eq!(parse_peerings(junk), Default::default());
            assert_eq!(parse_remarks(junk), Default::default());
        }
    }

    #[tokio::test]
    async fn a_node_that_lags_or_is_not_the_g1_chain_is_rejected() {
        let fake = Fake::default();
        healthy_rpc(&fake, "wss://ok.example/ws", 1000, 2);
        healthy_rpc(&fake, "wss://late.example/ws", 1000, 500);
        let http = rpc_http_url("wss://other.example/ws");
        fake.answer(&format!("{http}|system_chain"), r#"{"error":"méthode inconnue"}"#);
        assert_eq!(check_rpc(&fake, "wss://ok.example/ws").await.map(|n| n.1), Some(1000));
        assert!(check_rpc(&fake, "wss://late.example/ws").await.is_none(), "500 blocs de retard");
        assert!(check_rpc(&fake, "wss://other.example/ws").await.is_none(), "pas une chaîne");
        assert!(check_rpc(&fake, "wss://down.example/ws").await.is_none(), "injoignable");
        fake.answer("https://sq.example/v1/graphql|squidStatus", r#"{"data":{"squidStatus":{"height":4242}}}"#);
        assert_eq!(check_squid(&fake, "https://sq.example/v1/graphql").await.map(|n| n.1), Some(4242));
        assert!(check_squid(&fake, "https://nope.example/v1/graphql").await.is_none());
    }

    #[tokio::test]
    async fn discovery_merges_all_sources_probes_each_and_keeps_the_healthy_ones() {
        let fake = Arc::new(Fake::default());
        // Un nœud d'amorce annonce un pair RPC et un squid ; le fichier réseau en ajoute un autre ; deux sont sains.
        fake.answer(&format!("{}|duniter_peerings", rpc_http_url(BOOTSTRAP_RPC[0])), r#"{"result":{"peerings":[{"endpoints":[{"protocol":"rpc","address":"wss://pair.example/ws"},{"protocol":"squid","address":"https://pair-squid.example/v1/graphql"}]}]}}"#);
        fake.answer(NETWORK_JSON_URL, r#"{"endpoints":{"rpc":["wss://officiel.example"]}}"#);
        healthy_rpc(&fake, "wss://pair.example/ws", 5000, 1);
        healthy_rpc(&fake, "wss://officiel.example", 5001, 0);
        healthy_rpc(&fake, BOOTSTRAP_RPC[3], 4999, 3);
        fake.answer("https://pair-squid.example/v1/graphql|squidStatus", r#"{"data":{"squidStatus":{"height":5000}}}"#);
        let n = discover(fake.clone()).await;
        assert!(!n.degraded, "{n:?}");
        let urls = n.rpc_urls();
        assert_eq!(urls.len(), 3, "{urls:?}");
        for expected in ["wss://pair.example/ws", "wss://officiel.example", BOOTSTRAP_RPC[3]] {
            assert!(urls.iter().any(|u| u == expected), "{expected} manque : {urls:?}");
        }
        assert_eq!(n.squid_urls(), ["https://pair-squid.example/v1/graphql"]);
        assert!(n.rpc.windows(2).all(|w| w[0].latency_ms <= w[1].latency_ms), "trié par latence");
    }

    #[tokio::test]
    async fn without_any_answer_the_bootstraps_are_kept_and_the_state_is_degraded_and_not_cached() {
        let dir = std::env::temp_dir().join(format!("astroportd-duniter-nodes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 2).unwrap();
        let fake = Arc::new(Fake::default());
        let n = nodes(&db, fake.clone()).await;
        assert!(n.degraded);
        assert_eq!(n.rpc_urls(), BOOTSTRAP_RPC);
        assert_eq!(n.squid_urls(), BOOTSTRAP_SQUID);
        assert!(cached(&db).await.is_none(), "une liste dégradée n'est pas mise en cache");
        // Une découverte réussie est mise en cache : le second appel ne touche plus le réseau.
        fake.answer("https://squid.g1.gyroi.de/v1/graphql|squidStatus", r#"{"data":{"squidStatus":{"height":9}}}"#);
        healthy_rpc(&fake, "wss://g1.gyroi.de", 77, 0);
        let first = nodes(&db, fake.clone()).await;
        assert!(!first.degraded && first.rpc_urls() == ["wss://g1.gyroi.de"] && first.squid_urls() == ["https://squid.g1.gyroi.de/v1/graphql"], "{first:?}");
        let calls = fake.1.lock().unwrap().len();
        assert_eq!(nodes(&db, fake.clone()).await, first);
        assert_eq!(fake.1.lock().unwrap().len(), calls, "cache valide : aucun appel réseau");
        let _ = std::fs::remove_dir_all(dir);
    }
}
