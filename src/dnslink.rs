//! Adresses de la coopérative : chaque station (Astroport) et chaque MULTIPASS hébergé reçoivent un enregistrement DNSLink
//! `_dnslink.<nom>.<zone>` → `/ipns/<espace IPNS>` dans la zone DNS OVH de la coopérative. N'importe quelle passerelle IPFS
//! résout alors `<nom>.<zone>` vers l'espace de la station ou du membre.
//!
//! Reprend `Astroport.ONE/admin/system/ovh.me.sh` (API OVH v1, signature HMAC-SHA1, mêmes noms de sous-domaines, mêmes
//! cibles, TTL 0) : les deux outils écrivent les **mêmes** enregistrements et peuvent se relayer. Les identifiants viennent de
//! la configuration coopérative (kind 30800 : `OVH_APP_KEY`, `OVH_APP_SECRET`, `OVH_CONSUMER_KEY`, `OVH_ZONE`).
//!
//! Rien n'est écrit tant qu'une cible n'a pas changé (dernière cible enregistrée dans `kv`), un échec n'est jamais avalé
//! (bilan dans `kv`, alerte au Capitaine quand il change), et la station ne supprime jamais un enregistrement.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::coop;
use astro_db::Db;
use bytes::Bytes;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use tokio::io::AsyncReadExt;

use crate::coop_config;
use crate::fetch::{self, FetchLimits};

/// Point d'accès de l'API OVH (Europe), comme `ovh.me.sh`.
pub const ENDPOINT: &str = "https://eu.api.ovh.com/1.0";
/// Zone par défaut, comme `ovh.me.sh`.
pub const DEFAULT_ZONE: &str = "astroport.one";
/// Enregistrements créés ou mis à jour au plus par passage (l'API OVH est limitée en débit).
const MAX_PER_PASS: usize = 40;
const SCOPE: &str = "dnslink";
const STATUS_SCOPE: &str = "dnslink_status";

/// Identifiants OVH de la coopérative.
#[derive(Clone)]
pub struct Credentials {
    /// `OVH_APP_KEY`.
    pub app_key: String,
    /// `OVH_APP_SECRET`.
    pub app_secret: String,
    /// `OVH_CONSUMER_KEY`.
    pub consumer_key: String,
    /// `OVH_ZONE`.
    pub zone: String,
}

/// Transport vers l'API OVH : `(méthode, chemin, corps JSON)` → corps de la réponse.
pub trait Transport: Send + Sync {
    /// Un appel signé.
    fn call<'a>(&'a self, method: &'a str, path: &'a str, body: Option<String>) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

/// Signature OVH : `"$1$" + sha1_hex(secret+consumer+méthode+url+corps+horodatage)`.
pub fn sign(secret: &str, consumer: &str, method: &str, url: &str, body: &str, timestamp: &str) -> String {
    let digest = Sha1::digest(format!("{secret}+{consumer}+{method}+{url}+{body}+{timestamp}").as_bytes());
    format!("$1${}", hex::encode(digest))
}

/// Transport réel : HTTPS vers `eu.api.ovh.com` (racines Mozilla, aucune redirection suivie).
pub struct HttpsTransport {
    creds: Credentials,
    limits: FetchLimits,
}

impl HttpsTransport {
    /// Transport pour ces identifiants.
    pub fn new(creds: Credentials) -> Self {
        let mut limits = FetchLimits::new(1 << 20);
        limits.max_redirects = 0;
        limits.timeout = Duration::from_secs(15);
        Self { creds, limits }
    }

    async fn read(&self, got: fetch::Fetched) -> Result<String, String> {
        let mut out = Vec::new();
        let mut body = got.body;
        body.read_to_end(&mut out).await.map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

impl Transport for HttpsTransport {
    fn call<'a>(&'a self, method: &'a str, path: &'a str, body: Option<String>) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let time = self.read(fetch::fetch(&format!("{ENDPOINT}/auth/time"), &self.limits).await.map_err(|e| format!("OVH /auth/time : {e}"))?).await?;
            let timestamp = time.trim().to_owned();
            let url = format!("{ENDPOINT}{path}");
            let payload = body.clone().unwrap_or_default();
            let headers = [
                ("Content-Type", "application/json".to_owned()),
                ("X-Ovh-Application", self.creds.app_key.clone()),
                ("X-Ovh-Consumer", self.creds.consumer_key.clone()),
                ("X-Ovh-Timestamp", timestamp.clone()),
                ("X-Ovh-Signature", sign(&self.creds.app_secret, &self.creds.consumer_key, method, &url, &payload, &timestamp)),
            ];
            let got = fetch::request(method, &url, &headers, body.map(Bytes::from), &self.limits).await.map_err(|e| format!("OVH {method} {path} : {e}"))?;
            self.read(got).await
        })
    }
}

/// Sous-domaine DNSLink : `alice` → `_dnslink.alice` (comme `ovh.me.sh::_norm_sub`).
pub fn norm_sub(sub: &str) -> String {
    if sub.starts_with("_dnslink") { sub.to_owned() } else { format!("_dnslink.{sub}") }
}

/// Cible : `/ipns/k51…` → `dnslink=/ipns/k51…` ; `k51…` → IPNS ; autre chose → CID IPFS (comme `ovh.me.sh::_norm_target`).
pub fn norm_target(target: &str) -> String {
    match target {
        t if t.starts_with("dnslink=") => t.to_owned(),
        t if t.starts_with("/ipns/") || t.starts_with("/ipfs/") => format!("dnslink={t}"),
        t if t.starts_with("k51") => format!("dnslink=/ipns/{t}"),
        t => format!("dnslink=/ipfs/{t}"),
    }
}

/// Nom d'adresse d'un MULTIPASS : celui de `clyuseryomail.sh` (partie locale découpée sur tout ce qui n'est pas alphanumérique,
/// remise à l'envers, points, puis domaine) : `geg-la_debrouille@super.chez-moi.com` → `debrouille.la.geg.super.chez-moi.com`.
pub fn youser(email: &str) -> String {
    let (local, domain) = email.split_once('@').unwrap_or((email, ""));
    let mut parts: Vec<&str> = local.split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()).collect();
    parts.reverse();
    let mut out = parts.join(".");
    if !out.is_empty() {
        out.push('.');
    }
    out.push_str(domain);
    out
}

/// Nom d'adresse d'une station : `<nom de machine>-<6 derniers caractères du PeerID>`. Le suffixe évite que deux stations
/// nommées « raspberrypi » s'écrasent l'une l'autre.
pub fn station_label(hostname: &str, peer_id: &str) -> String {
    let mut slug = String::new();
    for c in hostname.trim().to_ascii_lowercase().chars() {
        match c {
            'a'..='z' | '0'..='9' => slug.push(c),
            _ if !slug.ends_with('-') && !slug.is_empty() => slug.push('-'),
            _ => {}
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(24).collect();
    let slug = if slug.is_empty() { "station".to_owned() } else { slug };
    let tail: String = peer_id.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<String>().to_ascii_lowercase();
    format!("{slug}-{tail}")
}

/// Nom IPNS (`k51…`) d'une station d'après son PeerID (`12D3Koo…`).
pub fn peer_id_to_ipns(peer_id: &str) -> Result<String, String> {
    let peer: astro_p2p::PeerId = peer_id.parse().map_err(|e| format!("PeerID illisible : {e}"))?;
    let mut cid = vec![0x01, 0x72];
    cid.extend_from_slice(&peer.to_bytes());
    Ok(astro_lifecycle::multipass::cid_to_ipns(&cid))
}

/// Un nom ne contient que ce qu'un nom DNS et un chemin d'API peuvent porter : jamais d'injection dans l'URL.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 200 && !name.ends_with('.') && !name.contains("..") && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Ce que `upsert` a fait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Enregistrement créé.
    Created,
    /// Cible mise à jour.
    Updated,
    /// Déjà à jour : rien n'a été écrit.
    Unchanged,
}

/// Client de la zone : crée ou met à jour un enregistrement TXT DNSLink.
pub struct Zone<T: Transport> {
    transport: T,
    zone: String,
}

impl<T: Transport> Zone<T> {
    /// Client de `zone`.
    pub fn new(transport: T, zone: &str) -> Self {
        Self { transport, zone: zone.to_owned() }
    }

    fn reply(raw: &str) -> Result<Value, String> {
        let v: Value = serde_json::from_str(raw).map_err(|_| format!("réponse OVH illisible : {}", raw.chars().take(120).collect::<String>()))?;
        match v.get("message").and_then(Value::as_str) {
            Some(m) => Err(format!("OVH : {m}")),
            None => Ok(v),
        }
    }

    /// Crée ou met à jour `_dnslink.<sub>.<zone>` ; rafraîchit la zone seulement si quelque chose a changé.
    pub async fn upsert(&self, sub: &str, target: &str) -> Result<Change, String> {
        let (sub, target) = (norm_sub(sub), norm_target(target));
        if !valid_name(&sub) || !valid_name(&self.zone) || target.chars().any(char::is_control) || target.len() > 255 {
            return Err(format!("nom ou cible invalide : {sub}"));
        }
        let ids = Self::reply(&self.transport.call("GET", &format!("/domain/zone/{}/record?fieldType=TXT&subDomain={sub}", self.zone), None).await?)?;
        let id = ids.as_array().and_then(|a| a.first()).and_then(Value::as_u64);
        let change = match id {
            None => {
                let body = json!({ "fieldType": "TXT", "subDomain": sub, "target": target, "ttl": 0 }).to_string();
                Self::reply(&self.transport.call("POST", &format!("/domain/zone/{}/record", self.zone), Some(body)).await?)?;
                Change::Created
            }
            Some(id) => {
                let current = Self::reply(&self.transport.call("GET", &format!("/domain/zone/{}/record/{id}", self.zone), None).await?)?;
                if current["target"].as_str() == Some(target.as_str()) {
                    return Ok(Change::Unchanged);
                }
                Self::reply(&self.transport.call("PUT", &format!("/domain/zone/{}/record/{id}", self.zone), Some(json!({ "target": target }).to_string())).await?)?;
                Change::Updated
            }
        };
        self.transport.call("POST", &format!("/domain/zone/{}/refresh", self.zone), None).await?;
        Ok(change)
    }
}

/// Identifiants OVH de la coopérative (secrets déchiffrés avec `UPLANETNAME`) ; `None` s'ils ne sont pas tous renseignés.
pub async fn credentials(db: &Db, uplanet_name: &str) -> Option<Credentials> {
    let (_, hex) = coop_config::coop_keys(uplanet_name).ok()?;
    let stored = coop_config::load(db, &hex).await?;
    let get = |k: &str| stored.values.get(k).map(|v| if coop::is_encrypted(v) { coop::decrypt(uplanet_name, v).unwrap_or_default() } else { v.clone() }).filter(|v| !v.trim().is_empty());
    Some(Credentials { app_key: get("OVH_APP_KEY")?, app_secret: get("OVH_APP_SECRET")?, consumer_key: get("OVH_CONSUMER_KEY")?, zone: get("OVH_ZONE").map(|z| z.trim().to_ascii_lowercase()).unwrap_or_else(|| DEFAULT_ZONE.to_owned()) })
}

/// Une adresse voulue : nom (sans `_dnslink.` ni zone) et espace IPNS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// Sous-domaine.
    pub sub: String,
    /// `/ipns/k51…`.
    pub ipns: String,
    /// `station` ou `multipass`.
    pub kind: &'static str,
}

/// Service d'enregistrement : station et MULTIPASS hébergés.
pub struct Dnslink {
    db: Arc<Db>,
    uplanet_name: String,
    peer_id: Option<String>,
    hostname: String,
    activity: ActivityBus,
    gate: tokio::sync::Mutex<()>,
}

impl Dnslink {
    /// Service d'une station.
    pub fn new(db: Arc<Db>, uplanet_name: String, peer_id: Option<String>, hostname: String, activity: ActivityBus) -> Self {
        Self { db, uplanet_name, peer_id, hostname, activity, gate: tokio::sync::Mutex::new(()) }
    }

    /// Adresses à enregistrer : la station, puis chaque MULTIPASS hébergé qui a un espace IPNS.
    pub async fn wanted(&self) -> Vec<Wanted> {
        let mut out = Vec::new();
        if let Some(p) = &self.peer_id {
            match peer_id_to_ipns(p) {
                Ok(ipns) => out.push(Wanted { sub: station_label(&self.hostname, p), ipns: format!("/ipns/{ipns}"), kind: "station" }),
                Err(e) => eprintln!("dnslink : station ignorée : {e}"),
            }
        }
        let rows: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT email, ipns FROM identities WHERE hosted = 1 AND role IN ('multipass', 'captain') AND email IS NOT NULL AND ipns IS NOT NULL ORDER BY email")?;
                Ok(stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        for (email, ipns) in rows {
            let name = ipns.trim().trim_start_matches("/ipns/");
            let sub = youser(&email).to_ascii_lowercase();
            if name.starts_with("k51") && valid_name(&sub) {
                out.push(Wanted { sub, ipns: format!("/ipns/{name}"), kind: "multipass" });
            }
        }
        out
    }

    async fn last(&self, sub: &str) -> Option<String> {
        let key = sub.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, key], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten()
    }

    async fn remember(&self, key: &str, value: &str, scope: &str) {
        let at = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())).unwrap_or(0);
        if let Err(e) = self
            .db
            .writer()
            .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![scope.to_owned().into(), key.to_owned().into(), value.to_owned().into(), at.into()])
            .await
        {
            eprintln!("dnslink : bilan non enregistré : {e}");
        }
    }

    /// Un passage : enregistre ce qui a changé depuis la dernière fois. Rend le bilan (aussi gardé dans `kv`).
    pub async fn sync(&self) -> Value {
        let _one = self.gate.lock().await;
        let Some(creds) = credentials(&self.db, &self.uplanet_name).await else {
            let v = json!({ "configured": false });
            self.remember("last", &v.to_string(), STATUS_SCOPE).await;
            return v;
        };
        let zone = Zone::new(HttpsTransport::new(creds.clone()), &creds.zone);
        self.sync_with(&zone, &creds.zone).await
    }

    /// `sync` avec un client donné (tests).
    pub async fn sync_with<T: Transport>(&self, zone: &Zone<T>, zone_name: &str) -> Value {
        let wanted = self.wanted().await;
        let (mut written, mut unchanged, mut errors, mut station) = (0u32, 0u32, Vec::<String>::new(), Value::Null);
        let mut budget = MAX_PER_PASS;
        for w in &wanted {
            let target = norm_target(&w.ipns);
            if w.kind == "station" {
                station = json!({ "name": format!("{}.{zone_name}", w.sub), "ipns": w.ipns });
            }
            if self.last(&w.sub).await.as_deref() == Some(target.as_str()) {
                unchanged += 1;
                continue;
            }
            if budget == 0 {
                continue;
            }
            budget -= 1;
            match zone.upsert(&w.sub, &w.ipns).await {
                Ok(change) => {
                    self.remember(&w.sub, &target, SCOPE).await;
                    if change == Change::Unchanged { unchanged += 1 } else { written += 1 }
                }
                Err(e) => errors.push(format!("{} : {e}", w.sub)),
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let pending = wanted.len() as u32 - written - unchanged - u32::try_from(errors.len()).unwrap_or(0);
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let status = json!({ "configured": true, "zone": zone_name, "at": at, "station": station, "total": wanted.len(), "written": written, "unchanged": unchanged, "pending": pending, "failed": errors.len(), "errors": errors.iter().take(5).collect::<Vec<_>>() });
        let previous = self.status().await;
        self.remember("last", &status.to_string(), STATUS_SCOPE).await;
        if !errors.is_empty() && previous.as_ref().and_then(|p| p["errors"].get(0)) != status["errors"].get(0) {
            self.activity.publish_to(Audience::Captain, "alert", "dnslink", format!("DNSLink : {} adresse(s) de la coopérative non enregistrée(s) — {}", errors.len(), errors[0]), None);
        }
        status
    }

    /// Dernier bilan enregistré.
    pub async fn status(&self) -> Option<Value> {
        let raw: Option<String> = self.db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = 'last'", [STATUS_SCOPE], |r| r.get(0)).optional()?)).await.ok().flatten();
        raw.and_then(|r| serde_json::from_str(&r).ok())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const PEER: &str = "12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG";

    /// Faux OVH : une zone en mémoire (id → (sous-domaine, cible)) et le journal des appels.
    #[derive(Default)]
    struct Fake {
        records: Mutex<Vec<(u64, String, String)>>,
        calls: Mutex<Vec<String>>,
        fail_on: Option<&'static str>,
    }

    impl Transport for Arc<Fake> {
        fn call<'a>(&'a self, method: &'a str, path: &'a str, body: Option<String>) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("{method} {path}"));
                if self.fail_on.is_some_and(|f| path.contains(f)) {
                    return Ok(r#"{"message":"This call has not been granted"}"#.to_owned());
                }
                let mut recs = self.records.lock().unwrap();
                let sub_of = |p: &str| p.split("subDomain=").nth(1).map(str::to_owned);
                Ok(match method {
                    "GET" if path.contains("subDomain=") => {
                        let sub = sub_of(path).unwrap();
                        json!(recs.iter().filter(|r| r.1 == sub).map(|r| r.0).collect::<Vec<_>>()).to_string()
                    }
                    "GET" => {
                        let id: u64 = path.rsplit('/').next().unwrap().parse().unwrap();
                        let r = recs.iter().find(|r| r.0 == id).unwrap();
                        json!({ "id": r.0, "subDomain": r.1, "target": r.2 }).to_string()
                    }
                    "POST" if path.ends_with("/record") => {
                        let v: Value = serde_json::from_str(&body.unwrap()).unwrap();
                        assert_eq!((v["fieldType"].as_str(), v["ttl"].as_u64()), (Some("TXT"), Some(0)));
                        let id = recs.len() as u64 + 1;
                        recs.push((id, v["subDomain"].as_str().unwrap().to_owned(), v["target"].as_str().unwrap().to_owned()));
                        json!({ "id": id }).to_string()
                    }
                    "PUT" => {
                        let id: u64 = path.rsplit('/').next().unwrap().parse().unwrap();
                        let v: Value = serde_json::from_str(&body.unwrap()).unwrap();
                        recs.iter_mut().find(|r| r.0 == id).unwrap().2 = v["target"].as_str().unwrap().to_owned();
                        "null".to_owned()
                    }
                    _ => "null".to_owned(),
                })
            })
        }
    }

    #[test]
    fn the_signature_is_the_ovh_v1_one() {
        // Vecteur calculé avec `sha1sum` sur la chaîne que `ovh.me.sh::_ovh_sign` construit.
        assert_eq!(sign("sec", "cons", "GET", "https://eu.api.ovh.com/1.0/domain/zone/z.fr/record", "", "1700000000"), "$1$2e55ad552ce4505686e0e4f7187e666cb239471f");
    }

    #[test]
    fn names_and_targets_are_the_ones_of_the_shell_tools() {
        // Sorties de `clyuseryomail.sh` et de `ovh.me.sh`.
        assert_eq!(youser("geg-la_debrouille@super.chez-moi.com"), "debrouille.la.geg.super.chez-moi.com");
        assert_eq!(youser("Alice.Martin+x@Exemple.ORG"), "x.Martin.Alice.Exemple.ORG");
        assert_eq!((norm_sub("alice").as_str(), norm_sub("_dnslink.origin").as_str(), norm_sub("_dnslink").as_str()), ("_dnslink.alice", "_dnslink.origin", "_dnslink"));
        assert_eq!(norm_target("/ipns/k51abc"), "dnslink=/ipns/k51abc");
        assert_eq!(norm_target("k51abc"), "dnslink=/ipns/k51abc");
        assert_eq!(norm_target("QmXyz"), "dnslink=/ipfs/QmXyz");
        assert_eq!(norm_target("dnslink=/ipfs/Qm"), "dnslink=/ipfs/Qm");
    }

    #[test]
    fn a_station_gets_a_readable_unique_name_and_the_ipns_of_its_peer_id() {
        assert_eq!(station_label("Raspberry Pi 4", PEER), "raspberry-pi-4-b9ag".replace("b9ag", &PEER[PEER.len() - 6..].to_ascii_lowercase()));
        assert_eq!(station_label("", PEER), format!("station-{}", PEER[PEER.len() - 6..].to_ascii_lowercase()));
        assert_ne!(station_label("raspberrypi", PEER), station_label("raspberrypi", "12D3KooWAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"), "deux Raspberry ne s'écrasent pas");
        // Même clé que le vecteur legacy de ylevel.rs : PeerID 12D3KooWRxhg… ⇔ k51qzi5uqu5d….
        let ipns = peer_id_to_ipns(PEER).unwrap();
        assert!(ipns.starts_with("k51qzi5uqu5d") && ipns.len() == 62, "{ipns}");
        assert!(peer_id_to_ipns("pas-un-peer").is_err());
    }

    #[tokio::test]
    async fn upsert_creates_then_updates_and_leaves_an_up_to_date_record_alone() {
        let fake = Arc::new(Fake::default());
        let zone = Zone::new(fake.clone(), "astroport.one");
        assert_eq!(zone.upsert("alice", "/ipns/k51aaa").await.unwrap(), Change::Created);
        assert_eq!(fake.records.lock().unwrap()[0].1, "_dnslink.alice");
        let before = fake.calls.lock().unwrap().len();
        assert_eq!(zone.upsert("alice", "/ipns/k51aaa").await.unwrap(), Change::Unchanged);
        assert!(!fake.calls.lock().unwrap()[before..].iter().any(|c| c.contains("refresh") || c.starts_with("PUT")), "rien d'écrit, zone non rafraîchie");
        assert_eq!(zone.upsert("alice", "/ipns/k51bbb").await.unwrap(), Change::Updated);
        assert_eq!(fake.records.lock().unwrap()[0].2, "dnslink=/ipns/k51bbb");
        assert_eq!(fake.records.lock().unwrap().len(), 1, "jamais de doublon");
    }

    #[tokio::test]
    async fn an_ovh_error_is_reported_and_hostile_names_never_reach_the_api() {
        let fake = Arc::new(Fake { fail_on: Some("/record"), ..Fake::default() });
        let zone = Zone::new(fake.clone(), "astroport.one");
        let e = zone.upsert("alice", "/ipns/k51aaa").await.unwrap_err();
        assert!(e.contains("not been granted"), "{e}");
        let fake = Arc::new(Fake::default());
        let zone = Zone::new(fake.clone(), "astroport.one");
        for bad in ["a/../../x", "a b", "a?x=1", "a\"b", ""] {
            assert!(zone.upsert(bad, "/ipns/k51aaa").await.is_err(), "{bad:?}");
        }
        assert!(zone.upsert("alice", "/ipns/k51\naaa").await.is_err());
        assert!(fake.calls.lock().unwrap().is_empty(), "aucun appel pour un nom refusé");
    }

    async fn db(tag: &str) -> (std::path::PathBuf, Arc<Db>) {
        let dir = std::env::temp_dir().join(format!("astroportd-dnslink-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), Arc::new(Db::open(&dir.join("u.db"), 2).unwrap()))
    }

    async fn host(db: &Db, hex: &str, role: &str, email: &str, ipns: &str) {
        db.writer()
            .execute("INSERT INTO identities(hex, role, email, ipns, hosted) VALUES (?1, ?2, ?3, ?4, 1)", vec![hex.to_owned().into(), role.to_owned().into(), email.to_owned().into(), ipns.to_owned().into()])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_station_and_every_hosted_multipass_are_registered_once() {
        let (dir, db) = db("sync").await;
        host(&db, &"a".repeat(64), "captain", "fred@qo-op.com", "/ipns/k51qzi5uqu5dcaptain").await;
        host(&db, &"b".repeat(64), "multipass", "geg-la@exemple.org", "k51qzi5uqu5dmember").await;
        host(&db, &"c".repeat(64), "multipass", "sans-ipns@exemple.org", "").await;
        let service = Dnslink::new(db.clone(), "x".repeat(64), Some(PEER.to_owned()), "ma-box".into(), ActivityBus::new());
        let wanted = service.wanted().await;
        assert_eq!(wanted.iter().map(|w| (w.kind, w.sub.as_str())).collect::<Vec<_>>(), [("station", format!("ma-box-{}", PEER[PEER.len() - 6..].to_ascii_lowercase()).as_str()), ("multipass", "fred.qo-op.com"), ("multipass", "la.geg.exemple.org")].map(|(k, s)| (k, s)), "{wanted:?}");

        let fake = Arc::new(Fake::default());
        let zone = Zone::new(fake.clone(), "astroport.one");
        let first = service.sync_with(&zone, "astroport.one").await;
        assert_eq!((first["total"].as_u64(), first["written"].as_u64(), first["failed"].as_u64()), (Some(3), Some(3), Some(0)), "{first}");
        assert_eq!(fake.records.lock().unwrap().len(), 3);
        // Deuxième passage : tout est déjà enregistré, l'API n'est même pas appelée.
        let calls = fake.calls.lock().unwrap().len();
        let second = service.sync_with(&zone, "astroport.one").await;
        assert_eq!((second["written"].as_u64(), second["unchanged"].as_u64()), (Some(0), Some(3)), "{second}");
        assert_eq!(fake.calls.lock().unwrap().len(), calls);
        assert_eq!(service.status().await.unwrap()["unchanged"], 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failure_is_in_the_report_and_the_captain_is_told_once() {
        let (dir, db) = db("fail").await;
        host(&db, &"a".repeat(64), "captain", "fred@qo-op.com", "/ipns/k51qzi5uqu5dcaptain").await;
        let bus = ActivityBus::new();
        let service = Dnslink::new(db.clone(), "x".repeat(64), None, "box".into(), bus.clone());
        let fake = Arc::new(Fake { fail_on: Some("/record"), ..Fake::default() });
        let zone = Zone::new(fake, "astroport.one");
        let r = service.sync_with(&zone, "astroport.one").await;
        assert_eq!((r["failed"].as_u64(), r["written"].as_u64()), (Some(1), Some(0)), "{r}");
        assert!(r["errors"][0].as_str().unwrap().contains("not been granted"));
        service.sync_with(&zone, "astroport.one").await;
        let alerts = bus.recent(50).iter().filter(|a| a.source == "dnslink").count();
        assert_eq!(alerts, 1, "même erreur : une seule alerte");
        let _ = std::fs::remove_dir_all(dir);
    }
}
