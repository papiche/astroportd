//! Les dons OpenCollective deviennent des Ẑen : l'`oc2uplanet.sh` du legacy, dans le démon.
//!
//! La station lit les contributions **du mois** du collectif (`OCSLUG`, `OCAPIKEY` du kind 30800), classe chaque don selon son palier
//! (`TIER_SLUG_SATELLITE`, `TIER_SLUG_CONSTELLATION`, `TIER_SLUG_LABO`, mêmes valeurs par défaut que le script) puis le verse avec
//! [`ZenCredit`] : don simple → crédit d'usage du MULTIPASS (1 € = 1 Ẑ), palier infrastructure → part sociale (33 % MULTIPASS · R&D ·
//! ASSETS, le reste au Capitaine), palier labo → portefeuille R&D.
//!
//! **Sûreté** : un don est versé **au plus une fois** (son identifiant est l'empreinte de `slug | montant | date`, écrit avant le
//! premier virement) ; par défaut la station **liste seulement** ce qui est à créditer et le Capitaine confirme (`OC_AUTO_CREDIT=1`
//! dans la configuration coopérative pour que la station crédite seule) ; un don dont le MULTIPASS n'est pas hébergé ici est laissé à
//! la station de ce membre. Un montant n'a jamais de flottant : la valeur lue est un texte décimal, tronquée au dixième de Ẑen.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::Chain;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::billet::BilletError;
use crate::coop_config;
use crate::fetch::{self, FetchLimits};
use crate::oc_member::ENDPOINT;
use crate::zen_credit::{Kind, MAX_ZEN_PER_CREDIT, ZenCredit};

/// Lecture de l'API : une page de 1 000 contributions, comme le script.
const LIMIT: u32 = 1000;
const PAGE: usize = 100;
const MAX_PAGES: usize = 10;
const EVERY: Duration = Duration::from_secs(30 * 60);
const FIRST: Duration = Duration::from_secs(5 * 60);
/// Réponse de l'API tolérée (1 000 contributions avec noms et e-mails).
const MAX_BODY: u64 = 8 * 1024 * 1024;

const DEFAULT_SATELLITE: &str = "*parrainage*128*,extension-128,satellite,*love-box*claude*";
const DEFAULT_CONSTELLATION: &str = "*parrainage*gpu*,module-gpu,constellation,*love-box*deluxe*,*love-box*gpu*";
const DEFAULT_LABO: &str = "infrastructure,labo,genereux-donateur,r-d,recherche";

/// Le palier `slug` correspond-il à l'une des entrées de `patterns` (séparées par des virgules) ? Une entrée avec `*` est un motif
/// libre ; sans `*`, elle doit former un segment entier délimité par des tirets (« labo » ne correspond pas à « collaboratif »).
pub fn tier_matches(slug: &str, patterns: &str) -> bool {
    patterns.split(',').map(str::trim).filter(|p| !p.is_empty()).any(|p| {
        if p.contains('*') {
            glob(p, slug)
        } else {
            slug == p || slug.starts_with(&format!("{p}-")) || slug.ends_with(&format!("-{p}")) || slug.contains(&format!("-{p}-"))
        }
    })
}

/// Motif avec `*` pour « n'importe quoi » (comme un `[[ == ]]` du shell, sans `?` ni classes).
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) || text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// Texte décimal (« 12.5 ») en centimes, tronqué au-delà du deuxième chiffre. Jamais de flottant.
pub fn cents_of_decimal(text: &str) -> Option<u64> {
    let (int, frac) = text.split_once('.').unwrap_or((text, ""));
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let frac2: String = frac.chars().chain("00".chars()).take(2).collect();
    int.parse::<u64>().ok()?.checked_mul(100)?.checked_add(frac2.parse::<u64>().ok()?)
}

/// Une contribution lue chez OpenCollective.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Donation {
    /// Identifiant stable d'un don pour la station.
    pub id: String,
    /// Compte OpenCollective du donateur.
    pub slug: String,
    /// Adresse e-mail du donateur, si OpenCollective la donne.
    pub email: Option<String>,
    /// Montant en centimes d'euro (1 € = 1 Ẑ).
    pub cents: u64,
    /// Devise ; seul l'euro est versé.
    pub currency: String,
    /// Palier (`order.tier.slug`).
    pub tier: Option<String>,
    /// Date de la contribution (texte de l'API).
    pub created_at: String,
    /// Ce que l'API a dit de l'e-mail : `ok`, `null`, `vide`, `absent` ou `invalide` (pour comprendre un don bloqué).
    pub email_state: &'static str,
}

/// Le service.
pub struct OcEmission<C: Chain> {
    db: Arc<Db>,
    uplanet_name: String,
    credit: Arc<ZenCredit<C>>,
    endpoint: String,
    limits: FetchLimits,
    activity: ActivityBus,
    last: Mutex<Option<Value>>,
    /// Dernière erreur de la recherche des adresses e-mail des membres (rendue dans le bilan, jamais avalée).
    lookup_error: Mutex<Option<String>>,
}

impl<C: Chain> OcEmission<C> {
    /// Service de production (API d'OpenCollective).
    pub fn new(db: Arc<Db>, uplanet_name: String, credit: Arc<ZenCredit<C>>, activity: ActivityBus) -> Self {
        Self::with(db, uplanet_name, credit, activity, ENDPOINT.into(), FetchLimits::new(MAX_BODY))
    }

    /// Service avec une adresse et des limites choisies (tests).
    pub fn with(db: Arc<Db>, uplanet_name: String, credit: Arc<ZenCredit<C>>, activity: ActivityBus, endpoint: String, limits: FetchLimits) -> Self {
        Self { db, uplanet_name, credit, endpoint, limits, activity, last: Mutex::new(None), lookup_error: Mutex::new(None) }
    }

    /// Opérations du Capitaine : `status` (lit et liste), `sync` (crédite ce qui est à créditer, avec `confirm`).
    pub async fn handle(&self, op: &str, body: &Value) -> Result<Value, BilletError> {
        if !body["_who"]["captain"].as_bool().unwrap_or(false) {
            return Err(BilletError::Refused("oc_not_captain", "réservé au Capitaine".into()));
        }
        match op {
            "status" => Ok(self.report(false, None).await),
            "sync" => {
                if body.get("confirm").and_then(Value::as_bool) != Some(true) {
                    return Err(BilletError::Invalid("oc_unconfirmed", "crédit non confirmé".into()));
                }
                // Avec `id`, un seul don est versé (le Capitaine le choisit dans la liste).
                Ok(self.report(true, body.get("id").and_then(Value::as_str)).await)
            }
            _ => Err(BilletError::Invalid("oc_bad_op", "opération inconnue".into())),
        }
    }

    async fn setting(&self, key: &'static str) -> Option<String> {
        coop_config::setting(&self.db, &self.uplanet_name, key).await
    }

    async fn post(&self, token: &str, query: Value) -> Result<Value, String> {
        let raw = fetch::post_json_with(&self.endpoint, &query.to_string(), &[("Personal-Token", token.to_owned()), ("Content-Type", "application/json".to_owned())], &self.limits).await.map_err(|e| format!("OpenCollective : {e}"))?;
        let v: Value = serde_json::from_slice(&raw).map_err(|_| "OpenCollective : réponse illisible".to_owned())?;
        match v["errors"].as_array().and_then(|e| e.first()) {
            Some(err) => Err(format!("OpenCollective : {}", err["message"].as_str().unwrap_or("erreur"))),
            None => Ok(v),
        }
    }

    /// Contributions du mois en cours (UTC).
    pub async fn fetch(&self) -> Result<Vec<Donation>, String> {
        let (Some(slug), Some(token)) = (self.setting("OCSLUG").await, self.setting("OCAPIKEY").await) else { return Err("OpenCollective n'est pas configuré (OCSLUG, OCAPIKEY)".into()) };
        let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs() / 86_400).unwrap_or(0));
        let (y, m, _) = astro_core::week::civil_from_days(days);
        let from = format!("{y:04}-{m:02}-01T00:00:00Z");
        // `includeChildrenTransactions` est indispensable : sans lui, les contributions aux projets enfants du collectif sont invisibles.
        let query = json!({
            "query": "query($slug: String, $from: DateTime, $limit: Int) { account(slug: $slug) { transactions(limit: $limit, type: CREDIT, includeChildrenTransactions: true, dateFrom: $from) { nodes { fromAccount { slug emails } amount { value currency } order { tier { slug } } createdAt } } } }",
            "variables": { "slug": slug, "from": from, "limit": LIMIT },
        });
        let v = self.post(&token, query).await?;
        let nodes = v["data"]["account"]["transactions"]["nodes"].as_array().ok_or("OpenCollective : collectif introuvable (OCSLUG ?)")?;
        let mut out: Vec<Donation> = nodes.iter().filter_map(parse_donation).collect();
        if out.iter().any(|d| d.email.is_none()) {
            let map = match self.backers(&slug, &token).await {
                Ok(m) => {
                    *self.lookup_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    m
                }
                Err(e) => {
                    *self.lookup_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
                    Vec::new()
                }
            };
            for d in out.iter_mut().filter(|d| d.email.is_none()) {
                d.email = map.iter().find(|(s, _)| *s == d.slug).map(|(_, e)| e.clone());
            }
        }
        Ok(out)
    }

    /// Adresse e-mail de chaque membre du collectif : les dons dont OpenCollective ne donne pas l'adresse la retrouvent ici.
    async fn backers(&self, slug: &str, token: &str) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        for page in 0..MAX_PAGES {
            let v = self.post(token, json!({ "query": "query($slug: String, $limit: Int, $offset: Int) { account(slug: $slug) { members(role: BACKER, limit: $limit, offset: $offset) { nodes { account { slug emails } } } } }", "variables": { "slug": slug, "limit": PAGE, "offset": page * PAGE } })).await?;
            let nodes = v["data"]["account"]["members"]["nodes"].as_array().ok_or("OpenCollective : membres illisibles")?;
            out.extend(nodes.iter().filter_map(|n| Some((n["account"]["slug"].as_str()?.to_owned(), n["account"]["emails"].as_array()?.first()?.as_str()?.to_owned()))));
            if nodes.len() < PAGE {
                break;
            }
        }
        Ok(out)
    }

    /// Cette adresse a-t-elle déjà un MULTIPASS sur une **autre** station de la constellation ? Créer un compte ici provoquerait
    /// une collision : le Capitaine en est alerté et rien n'est créé.
    pub async fn known_elsewhere(&self, email: &str) -> bool {
        let e = email.trim().to_ascii_lowercase();
        let n: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM identities WHERE role = 'multipass' AND hosted = 0 AND lower(email) = ?1", [&e], |r| r.get(0))?)).await.unwrap_or(1);
        if n > 0 {
            self.activity.publish_to(Audience::Captain, "alert", "oc", format!("Collision possible : {email} a déjà un MULTIPASS sur une autre station de la constellation, aucun compte n'est créé ici"), None);
        }
        n > 0
    }

    async fn kind_of(&self, tier: Option<&str>) -> Kind {
        let Some(tier) = tier else { return Kind::Usage };
        let pick = |v: Option<String>, d: &str| v.unwrap_or_else(|| d.to_owned());
        if tier_matches(tier, &pick(self.setting("TIER_SLUG_SATELLITE").await, DEFAULT_SATELLITE)) {
            Kind::Satellite
        } else if tier_matches(tier, &pick(self.setting("TIER_SLUG_CONSTELLATION").await, DEFAULT_CONSTELLATION)) {
            Kind::Constellation
        } else if tier_matches(tier, &pick(self.setting("TIER_SLUG_LABO").await, DEFAULT_LABO)) {
            Kind::Labo
        } else {
            Kind::Usage
        }
    }

    /// Lit les dons du mois et rend le bilan. `credit` : verse ceux qui sont à créditer.
    pub async fn report(&self, credit: bool, only: Option<&str>) -> Value {
        let donations = match self.fetch().await {
            Ok(d) => d,
            Err(e) => {
                let v = json!({ "configured": !e.contains("n'est pas configuré"), "error": e, "donations": [], "checked_at": now() });
                *self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(v.clone());
                return v;
            }
        };
        let auto = matches!(self.setting("OC_AUTO_CREDIT").await.as_deref(), Some("1"));
        let (mut rows, mut stop) = (Vec::new(), None::<String>);
        for d in &donations {
            let kind = self.kind_of(d.tier.as_deref()).await;
            // Au dixième de Ẑ près : 1 centime de Ğ1 vaut 10 centimes de Ẑ.
            let zc = d.cents / 10 * 10;
            let email = d.email.as_deref().unwrap_or("").trim();
            let done = self.credit.is_done("oc", &d.id).await;
            let (status, detail): (&str, Option<String>) = if done {
                ("done", None)
            } else if d.currency != "EUR" {
                ("blocked", Some(format!("devise {} non gérée", d.currency)))
            } else if email.is_empty() {
                ("blocked", Some(format!("aucune adresse e-mail pour ce don (OpenCollective : e-mail {})", d.email_state)))
            } else if zc < 10 {
                ("blocked", Some("montant trop petit".into()))
            } else if zc > MAX_ZEN_PER_CREDIT * 100 {
                ("blocked", Some(format!("plus de {MAX_ZEN_PER_CREDIT} ẑ : à verser à la main, en plusieurs fois")))
            } else if stop.is_some() {
                ("pending", stop.clone())
            } else if !(credit || auto) || only.is_some_and(|o| o != d.id) {
                ("pending", None)
            } else {
                match self.credit.credit_donation(&d.id, kind, email, zc).await {
                    Ok(_) => ("done", None),
                    // Ce MULTIPASS vit sur une autre station de la constellation : c'est à elle de le créditer.
                    // Sinon, personne ne l'a encore : le Capitaine peut lui créer un MULTIPASS, à moins qu'il existe déjà ailleurs.
                    Err(BilletError::Refused("zen_credit_unknown_recipient", m)) => {
                        if self.known_elsewhere(email).await {
                            ("elsewhere", Some(m))
                        } else {
                            ("no_multipass", Some("aucun MULTIPASS dans la constellation : à créer".into()))
                        }
                    }
                    Err(e @ (BilletError::Refused("zen_credit_master_poor" | "zen_credit_unknown_outcome", _) | BilletError::Failed(..) | BilletError::Unavailable(..))) => {
                        stop = Some(e.to_string());
                        ("failed", Some(e.to_string()))
                    }
                    Err(e) => ("blocked", Some(e.to_string())),
                }
            };
            rows.push(json!({ "id": d.id, "email": email, "zen_cents": zc, "tier": d.tier, "kind": match kind { Kind::Usage => "usage", Kind::Satellite => "satellite", Kind::Constellation => "constellation", _ => "labo" }, "created_at": d.created_at, "status": status, "detail": detail }));
        }
        let count = |s: &str| rows.iter().filter(|r| r["status"] == s).count();
        let pending_cents: u64 = rows.iter().filter(|r| r["status"] == "pending").filter_map(|r| r["zen_cents"].as_u64()).sum();
        let v = json!({ "configured": true, "auto": auto, "checked_at": now(), "donations": rows, "pending": count("pending"), "pending_zen_cents": pending_cents, "done": count("done"), "blocked": count("blocked"), "failed": count("failed"), "elsewhere": count("elsewhere"), "no_multipass": count("no_multipass"), "error": stop, "email_lookup_error": self.lookup_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() });
        let (blocked, failed) = (count("blocked"), count("failed"));
        if failed > 0 || ((credit || auto) && blocked > 0) {
            self.activity.publish_to(Audience::Captain, "alert", "oc", format!("OpenCollective : {failed} don(s) en échec, {blocked} bloqué(s) : voir « Ma UPlanet »"), None);
        }
        *self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(v.clone());
        v
    }

    /// Dernier bilan, sans interroger OpenCollective.
    pub fn last(&self) -> Option<Value> {
        self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Lecture périodique, jusqu'à l'arrêt. Sans configuration, la boucle ne fait rien.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut wait = FIRST;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => return,
            }
            wait = EVERY;
            if self.setting("OCAPIKEY").await.is_none() {
                continue;
            }
            let v = self.report(false, None).await;
            if let Some(e) = v["error"].as_str() {
                eprintln!("OpenCollective : {e}");
            } else if v["done"].as_u64().unwrap_or(0) + v["pending"].as_u64().unwrap_or(0) > 0 {
                println!("OpenCollective : {} don(s) à créditer, {} déjà versé(s){}", v["pending"], v["done"], if v["auto"] == true { " (crédit automatique)" } else { "" });
            }
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn parse_donation(n: &Value) -> Option<Donation> {
    let slug = n["fromAccount"]["slug"].as_str().unwrap_or("").to_owned();
    let value = n["amount"]["value"].as_number()?.to_string();
    let cents = cents_of_decimal(&value)?;
    let created_at = n["createdAt"].as_str()?.to_owned();
    let raw = &n["fromAccount"]["emails"];
    let email = raw.as_array().and_then(|e| e.first()).and_then(Value::as_str).filter(|e| e.contains('@')).map(str::to_owned);
    let email_state = match (raw, &email) {
        (_, Some(_)) => "ok",
        (Value::Null, _) if n["fromAccount"].get("emails").is_none() => "absent",
        (Value::Null, _) => "null",
        (Value::Array(a), _) if a.is_empty() => "vide",
        _ => "invalide",
    };
    let id = format!("oc-{}", &hex::encode(Sha256::digest(format!("{slug}|{cents}|{created_at}").as_bytes()))[..32]);
    Some(Donation { id, slug, email, cents, currency: n["amount"]["currency"].as_str().unwrap_or("").to_owned(), tier: n["order"]["tier"]["slug"].as_str().map(str::to_owned), created_at, email_state })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_match_like_the_script_does() {
        assert!(tier_matches("parrainage-infrastructure-extension-128-go", DEFAULT_SATELLITE));
        assert!(tier_matches("satellite", DEFAULT_SATELLITE) && tier_matches("mon-satellite", DEFAULT_SATELLITE));
        assert!(tier_matches("parrainage-module-gpu-1-24", DEFAULT_CONSTELLATION));
        assert!(tier_matches("labo", DEFAULT_LABO) && tier_matches("don-r-d-annuel", DEFAULT_LABO));
        assert!(!tier_matches("collaboratif", DEFAULT_LABO), "un mot nu ne correspond pas à une sous-chaîne d'un autre mot");
        assert!(!tier_matches("membre-resident", DEFAULT_SATELLITE) && !tier_matches("", DEFAULT_LABO));
        assert!(tier_matches("a-b-c", "*b*") && !tier_matches("abc", "x*c") && tier_matches("xc", "x*c") && !tier_matches("c", "c*c"));
    }

    #[test]
    fn amounts_are_read_as_decimal_text_never_as_floats() {
        for (text, cents) in [("10", Some(1_000)), ("8.5", Some(850)), ("0.05", Some(5)), ("12.999", Some(1_299)), ("1e3", None), ("-3", None), ("", None), (".5", None)] {
            assert_eq!(cents_of_decimal(text), cents, "{text}");
        }
    }

    use crate::zen_credit::tests::{Rig, rig};

    /// Faux OpenCollective : une page de contributions, et la liste des membres.
    async fn mock(nodes: Vec<Value>) -> String {
        let nodes = Arc::new(nodes);
        let app = axum::Router::new().route("/graphql", axum::routing::post(move |h: axum::http::HeaderMap, axum::Json(b): axum::Json<Value>| {
            let nodes = nodes.clone();
            async move {
                assert_eq!(h["personal-token"], "sk_test");
                if b["query"].as_str().unwrap().contains("transactions") {
                    assert_eq!(b["variables"]["slug"], "monnaie-libre");
                    assert!(b["variables"]["from"].as_str().unwrap().ends_with("-01T00:00:00Z"));
                    axum::Json(json!({ "data": { "account": { "transactions": { "nodes": *nodes } } } }))
                } else {
                    axum::Json(json!({ "data": { "account": { "members": { "nodes": [{ "account": { "slug": "anon", "emails": ["membre@exemple.org"] } }] } } } }))
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    fn tx(slug: &str, email: Option<&str>, value: Value, tier: Option<&str>, at: &str) -> Value {
        json!({ "fromAccount": { "slug": slug, "emails": email.map(|e| vec![e]) }, "amount": { "value": value, "currency": "EUR" }, "order": { "tier": tier.map(|t| json!({ "slug": t })) }, "createdAt": at })
    }

    async fn configure(r: &Rig, auto: bool) {
        let mut patch = std::collections::BTreeMap::from([("OCSLUG".to_owned(), Some("monnaie-libre".to_owned())), ("OCAPIKEY".to_owned(), Some("sk_test".to_owned()))]);
        if auto {
            patch.insert("OC_AUTO_CREDIT".to_owned(), Some("1".to_owned()));
        }
        let ev = coop_config::build_event(None, &patch, "UPlanetTestName", 1_800_000_000).unwrap();
        r.db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &ev).map(|_| ())).await.unwrap();
    }

    fn captain() -> Value {
        json!({ "_who": { "captain": true, "hex": "cc", "email": "capitaine@exemple.org" } })
    }

    fn service(r: &Rig, url: String) -> OcEmission<crate::zen_credit::tests::Ledger> {
        let mut limits = FetchLimits::new(1 << 20);
        limits.allow_private = true;
        OcEmission::with(r.db.clone(), "UPlanetTestName".into(), r.svc.clone(), ActivityBus::new(), url, limits)
    }

    #[tokio::test]
    async fn donations_are_listed_first_then_credited_once_when_the_captain_confirms() {
        let r = rig("oc", 10_000).await;
        configure(&r, false).await;
        let url = mock(vec![
            tx("membre", Some("membre@exemple.org"), json!(8.5), None, "2026-11-03T10:00:00Z"),
            tx("anon", None, json!(100), Some("parrainage-infrastructure-extension-128-go"), "2026-11-04T10:00:00Z"),
            tx("loin", Some("loin@exemple.org"), json!(10), None, "2026-11-05T10:00:00Z"),
            tx("labo", Some("don@exemple.org"), json!(30), Some("labo"), "2026-11-06T10:00:00Z"),
            tx("sansmail", None, json!(10), None, "2026-11-07T10:00:00Z"),
        ]).await;
        r.db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'loin@exemple.org', 0)", vec!["ab".repeat(32).into()]).await.unwrap();
        let oc = service(&r, url);
        // Par défaut, rien n'est versé : la station liste.
        let v = oc.handle("status", &captain()).await.unwrap();
        assert_eq!((v["pending"].as_u64(), v["blocked"].as_u64(), v["auto"].as_bool()), (Some(4), Some(1), Some(false)), "{v}");
        assert!(r.chain.sent.lock().unwrap().is_empty());
        assert!(matches!(oc.handle("sync", &captain()).await, Err(BilletError::Invalid("oc_unconfirmed", _))));
        let mut go = captain();
        go["confirm"] = json!(true);
        let v = oc.handle("sync", &go).await.unwrap();
        assert_eq!((v["done"].as_u64(), v["elsewhere"].as_u64(), v["blocked"].as_u64(), v["failed"].as_u64()), (Some(3), Some(1), Some(1), Some(0)), "{v}");
        let kinds: Vec<(String, String)> = v["donations"].as_array().unwrap().iter().map(|d| (d["email"].as_str().unwrap().to_owned(), d["kind"].as_str().unwrap().to_owned())).collect();
        assert!(kinds.contains(&("membre@exemple.org".into(), "usage".into())) && kinds.contains(&("don@exemple.org".into(), "labo".into())), "{kinds:?}");
        let n = r.chain.sent.lock().unwrap().len();
        assert_eq!(n, 2 + 5 + 1, "usage : 2 virements ; part sociale : 5 ; labo : 1");
        // Rejouer ne verse plus rien.
        let again = oc.handle("sync", &go).await.unwrap();
        assert_eq!((again["done"].as_u64(), r.chain.sent.lock().unwrap().len()), (Some(3), n));
        // Un membre n'a pas la main.
        assert!(matches!(oc.handle("status", &json!({ "_who": { "captain": false } })).await, Err(BilletError::Refused("oc_not_captain", _))));
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn the_station_credits_alone_only_when_the_captain_turned_it_on_and_stops_when_the_master_is_poor() {
        let r = rig("oc-auto", 300).await;
        configure(&r, true).await;
        let url = mock(vec![tx("membre", Some("membre@exemple.org"), json!(10), None, "2026-11-03T10:00:00Z"), tx("membre", Some("membre@exemple.org"), json!(500), None, "2026-11-04T10:00:00Z"), tx("membre", Some("membre@exemple.org"), json!(1), None, "2026-11-05T10:00:00Z")]).await;
        let v = service(&r, url).report(false, None).await;
        assert_eq!((v["auto"].as_bool(), v["done"].as_u64(), v["failed"].as_u64(), v["pending"].as_u64()), (Some(true), Some(1), Some(1), Some(1)), "le premier est versé, le deuxième dépasse le maître, le troisième attend : {v}");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_missing_configuration_or_a_down_api_is_an_error_not_an_empty_list() {
        let r = rig("oc-down", 10_000).await;
        let v = service(&r, "http://127.0.0.1:9/graphql".into()).report(false, None).await;
        assert_eq!(v["configured"], false);
        configure(&r, false).await;
        let v = service(&r, "http://127.0.0.1:9/graphql".into()).report(false, None).await;
        assert_eq!(v["configured"], true);
        assert!(v["error"].as_str().unwrap().contains("OpenCollective"), "{v}");
        let _ = std::fs::remove_dir_all(r.dir);
    }
}
