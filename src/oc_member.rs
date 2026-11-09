//! « Mon adresse est-elle inscrite sur OpenCollective ? » : la vérification que zelkova affiche à un membre, avec le rappel de
//! le faire si ce n'est pas le cas.
//!
//! La station interroge l'API GraphQL d'OpenCollective avec la clé de la coopérative (`OCAPIKEY`, secret du kind 30800) pour le
//! collectif `OCSLUG`, et ne répond **que pour l'adresse du compte qui demande** : jamais d'adresse passée en paramètre, donc
//! jamais un moyen de savoir si quelqu'un d'autre est membre. Le résultat est gardé 10 minutes par compte.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use astro_db::Db;
use serde_json::{Value, json};

use crate::coop_config;
use crate::fetch::{self, FetchLimits};

/// API publique d'OpenCollective.
pub const ENDPOINT: &str = "https://api.opencollective.com/graphql/v2";
const TTL: Duration = Duration::from_secs(600);
const MAX_CACHED: usize = 256;
const PAGE: usize = 100;
/// Pages lues au plus (1 000 membres) : au-delà, la réponse dit qu'elle est partielle plutôt que de boucler.
const MAX_PAGES: usize = 10;

/// `email` figure-t-il parmi les adresses des membres d'une page de réponse ? (comparaison sans casse)
pub fn email_in_nodes(nodes: &[Value], email: &str) -> bool {
    nodes.iter().any(|n| n["account"]["emails"].as_array().into_iter().flatten().filter_map(Value::as_str).any(|e| e.trim().eq_ignore_ascii_case(email)))
}

/// Le service de vérification.
pub struct OcMember {
    db: Arc<Db>,
    uplanet_name: String,
    endpoint: String,
    limits: FetchLimits,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
}

impl OcMember {
    /// Service de production (API d'OpenCollective).
    pub fn new(db: Arc<Db>, uplanet_name: String) -> Self {
        Self::with(db, uplanet_name, ENDPOINT.into(), FetchLimits::new(2 * 1024 * 1024))
    }

    /// Service avec une adresse et des limites choisies (tests).
    pub fn with(db: Arc<Db>, uplanet_name: String, endpoint: String, limits: FetchLimits) -> Self {
        Self { db, uplanet_name, endpoint, limits, cache: Mutex::new(HashMap::new()) }
    }

    /// État du membre `email` (celui du compte authentifié) : `{ configured, is_member, url, partial?, error? }`.
    pub async fn check(&self, email: &str) -> Value {
        let key = email.trim().to_ascii_lowercase();
        if let Some((at, v)) = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key) {
            if at.elapsed() < TTL {
                return v.clone();
            }
        }
        let setting = |k: &'static str| coop_config::setting(&self.db, &self.uplanet_name, k);
        let url = setting("OC_URL_MEMBRE").await;
        let (Some(slug), Some(token)) = (setting("OCSLUG").await, setting("OCAPIKEY").await) else {
            return json!({ "configured": false, "is_member": null, "url": url });
        };
        let result = match self.is_member(&slug, &token, &key).await {
            Ok((is_member, partial)) => json!({ "configured": true, "is_member": is_member, "partial": partial, "url": url }),
            // Une panne n'est pas un « non » : l'écran ne doit pas rappeler à quelqu'un déjà membre de s'inscrire.
            Err(e) => return json!({ "configured": true, "is_member": null, "error": e, "url": url }),
        };
        let mut cache = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.len() >= MAX_CACHED {
            cache.retain(|_, (at, _)| at.elapsed() < TTL);
            if cache.len() >= MAX_CACHED {
                cache.clear();
            }
        }
        cache.insert(key, (Instant::now(), result.clone()));
        result
    }

    /// Lit les membres page par page ; rend `(trouvé, lecture partielle)`.
    async fn is_member(&self, slug: &str, token: &str, email: &str) -> Result<(bool, bool), String> {
        for page in 0..MAX_PAGES {
            let query = json!({
                "query": "query($slug: String, $limit: Int, $offset: Int) { account(slug: $slug) { members(role: BACKER, limit: $limit, offset: $offset) { nodes { account { emails } } } } }",
                "variables": { "slug": slug, "limit": PAGE, "offset": page * PAGE },
            });
            let raw = fetch::post_json_with(&self.endpoint, &query.to_string(), &[("Personal-Token", token.to_owned()), ("Content-Type", "application/json".to_owned())], &self.limits).await.map_err(|e| format!("OpenCollective : {e}"))?;
            let v: Value = serde_json::from_slice(&raw).map_err(|_| "OpenCollective : réponse illisible".to_owned())?;
            if let Some(err) = v["errors"].as_array().and_then(|e| e.first()) {
                return Err(format!("OpenCollective : {}", err["message"].as_str().unwrap_or("erreur")));
            }
            let nodes = v["data"]["account"]["members"]["nodes"].as_array().ok_or("OpenCollective : collectif introuvable (OCSLUG ?)")?;
            if email_in_nodes(nodes, email) {
                return Ok((true, false));
            }
            if nodes.len() < PAGE {
                return Ok((false, false));
            }
        }
        Ok((false, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_email_is_matched_without_case_among_the_members_of_a_page() {
        let nodes = vec![json!({ "account": { "emails": ["Ana@Example.org", "autre@x.fr"] } }), json!({ "account": { "emails": null } }), json!({ "account": {} })];
        assert!(email_in_nodes(&nodes, "ana@example.org"));
        assert!(!email_in_nodes(&nodes, "bob@example.org"));
        assert!(!email_in_nodes(&[], "ana@example.org"));
    }

    async fn mock(members: Vec<String>) -> String {
        let members = Arc::new(members);
        let app = axum::Router::new().route("/graphql", axum::routing::post(move |h: axum::http::HeaderMap, axum::Json(b): axum::Json<Value>| {
            let members = members.clone();
            async move {
                assert_eq!(h["personal-token"], "tok");
                assert_eq!(b["variables"]["slug"], "monnaie-libre");
                let offset = usize::try_from(b["variables"]["offset"].as_u64().unwrap()).unwrap();
                let nodes: Vec<Value> = members.iter().skip(offset).take(PAGE).map(|e| json!({ "account": { "emails": [e] } })).collect();
                axum::Json(json!({ "data": { "account": { "members": { "nodes": nodes } } } }))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn membership_is_looked_up_page_by_page_and_a_failure_is_never_a_no() {
        let dir = std::env::temp_dir().join(format!("astroportd-oc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 1).unwrap());
        let mut limits = FetchLimits::new(1 << 20);
        limits.allow_private = true;
        let members: Vec<String> = (0..250).map(|i| format!("m{i}@example.org")).collect();
        let oc = OcMember::with(db.clone(), "UP".into(), mock(members).await, limits);
        // Sans OCSLUG ni OCAPIKEY : « non configurée », jamais « pas membre ».
        let v = oc.check("m3@example.org").await;
        assert_eq!((v["configured"].as_bool(), v["is_member"].is_null()), (Some(false), true));
        // Réglée : on teste le parcours des pages directement (la configuration passe par le kind 30800, testé ailleurs).
        assert_eq!(oc.is_member("monnaie-libre", "tok", "m3@example.org").await, Ok((true, false)));
        assert_eq!(oc.is_member("monnaie-libre", "tok", "M249@EXAMPLE.org").await, Ok((true, false)), "dernière page, casse ignorée");
        assert_eq!(oc.is_member("monnaie-libre", "tok", "inconnu@example.org").await, Ok((false, false)));
        // Une panne (adresse injoignable) est une erreur, pas un « non ».
        let down = OcMember::with(db, "UP".into(), "http://127.0.0.1:9/graphql".into(), FetchLimits { allow_private: true, ..FetchLimits::new(1 << 20) });
        assert!(down.is_member("monnaie-libre", "tok", "m3@example.org").await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
