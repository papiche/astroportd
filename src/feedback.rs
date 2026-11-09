//! Retours des utilisateurs (bogues, idées) **en issues Git**, pour toute application cliente (coracle, zelkova, une autre).
//!
//! Le Capitaine règle la forge dans la configuration coopérative (kind 30800) : `GIT_HOST` (GitHub par défaut, ou un GitLab),
//! `GIT_OWNER` (organisation ou compte) et `GIT_TOKEN` (secret). Une application s'identifie par `source` (son dépôt, un nom
//! simple) : l'issue est créée dans `<GIT_OWNER>/<source>`. Sans forge réglée, ou si elle répond mal, le retour est **gardé
//! sur la station** (journal) et le Capitaine en est prévenu : rien n'est perdu et l'utilisateur n'a pas d'erreur à lire.
//!
//! Garde-fous : compte authentifié obligatoire (pas d'envoi anonyme avec le jeton de la coopérative), 5 retours par heure et par
//! compte, textes bornés et nettoyés, nom de dépôt réduit à un identifiant simple, jamais de clé publique dans l'issue sans que
//! l'utilisateur l'ait demandé (`share_identity`), appel sortant par le chargeur borné (pas d'adresse privée).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::Arc;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use serde_json::{Value, json};

use crate::coop_config;
use crate::fetch::{self, FetchLimits};

/// Retours acceptés par heure et par compte.
const PER_HOUR: usize = 5;
const TITLE_MAX: usize = 120;
const TEXT_MAX: usize = 8000;

/// Pourquoi un retour est refusé.
#[derive(Debug, PartialEq, Eq)]
pub enum FeedbackError {
    /// Saisie refusée.
    Invalid(String),
    /// Trop de retours récents.
    Limited(String),
    /// Panne de la station.
    Internal(String),
}

/// Forge où créer l'issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forge {
    /// GitHub (`api.github.com`).
    GitHub,
    /// GitLab auto-hébergé ou non, à l'adresse `https://…`.
    GitLab(String),
}

/// Retour nettoyé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Titre.
    pub title: String,
    /// Description, métadonnées comprises.
    pub body: String,
    /// `bug` ou `feedback`.
    pub label: &'static str,
    /// Dépôt (nom simple) dans lequel créer l'issue.
    pub repo: String,
}

/// Texte borné, sans caractère de contrôle (sauf retours à la ligne).
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n').take(max).collect::<String>().trim().to_owned()
}

/// Nom de dépôt : minuscules, chiffres, `.`, `_`, `-`, 1 à 64 caractères, jamais `..` ni point initial.
pub fn valid_repo(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.starts_with(['.', '-']) && !s.contains("..") && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// Construit le retour depuis le corps JSON de l'application.
pub fn report_of(body: &Value, hex: &str) -> Result<Report, FeedbackError> {
    let text = |k: &str, max: usize| body[k].as_str().map(|s| clean(s, max)).unwrap_or_default();
    let (title, description) = (text("title", TITLE_MAX), text("description", TEXT_MAX));
    if title.chars().count() < 3 || description.chars().count() < 3 {
        return Err(FeedbackError::Invalid("un titre et une description d'au moins 3 caractères sont attendus".into()));
    }
    let source = text("source", 64).to_ascii_lowercase();
    if source.is_empty() || !valid_repo(&source) {
        return Err(FeedbackError::Invalid("source : nom du dépôt de l'application (minuscules, chiffres, - _ .)".into()));
    }
    let label = if body["category"].as_str().unwrap_or("bug").eq_ignore_ascii_case("bug") { "bug" } else { "feedback" };
    let mut meta = vec![format!("**Source :** {source}")];
    for (k, name) in [("app_version", "Version"), ("platform", "Plateforme")] {
        let v = text(k, 40);
        if !v.is_empty() {
            meta.push(format!("**{name} :** {v}"));
        }
    }
    if body["share_identity"].as_bool() == Some(true) {
        meta.push(format!("**Clé publique :** `{hex}`"));
    }
    Ok(Report { title, body: format!("{description}\n\n---\n{}", meta.join("  \n")), label, repo: source })
}

/// Adresse, en-têtes et corps de la création d'une issue.
pub fn issue_request(forge: &Forge, token: &str, owner: &str, r: &Report) -> (String, Vec<(&'static str, String)>, String) {
    match forge {
        Forge::GitHub => (
            format!("https://api.github.com/repos/{owner}/{}/issues", r.repo),
            vec![("Authorization", format!("Bearer {token}")), ("Accept", "application/vnd.github+json".into()), ("X-GitHub-Api-Version", "2022-11-28".into()), ("User-Agent", "astroportd".into())],
            json!({ "title": r.title, "body": r.body, "labels": [r.label] }).to_string(),
        ),
        Forge::GitLab(host) => (
            format!("{}/api/v4/projects/{owner}%2F{}/issues", host.trim_end_matches('/'), r.repo),
            vec![("PRIVATE-TOKEN", token.to_owned()), ("User-Agent", "astroportd".into())],
            json!({ "title": r.title, "description": r.body, "labels": r.label }).to_string(),
        ),
    }
}

/// Crée l'issue ; rend `(adresse, numéro)`.
pub async fn post_issue(forge: &Forge, token: &str, owner: &str, r: &Report, limits: &FetchLimits) -> Result<(String, u64), String> {
    let (url, headers, body) = issue_request(forge, token, owner, r);
    let raw = fetch::post_json_with(&url, &body, &headers, limits).await.map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&raw).map_err(|_| "réponse de la forge illisible".to_owned())?;
    let link = v["html_url"].as_str().or_else(|| v["web_url"].as_str()).ok_or("réponse de la forge sans adresse d'issue")?;
    Ok((link.to_owned(), v["number"].as_u64().or_else(|| v["iid"].as_u64()).unwrap_or(0)))
}

/// Le service des retours.
pub struct Feedback {
    db: Arc<Db>,
    uplanet_name: String,
    activity: ActivityBus,
    hits: Mutex<HashMap<String, Vec<i64>>>,
    limits: FetchLimits,
}

impl Feedback {
    /// `limits` : réglages de l'appel sortant (production : [`FetchLimits::new`]).
    pub fn new(db: Arc<Db>, uplanet_name: String, activity: ActivityBus) -> Self {
        Self { db, uplanet_name, activity, hits: Mutex::new(HashMap::new()), limits: FetchLimits::new(256 * 1024) }
    }

    fn now() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
    }

    /// Reçoit un retour du compte `hex`.
    pub async fn submit(&self, hex: &str, body: &Value) -> Result<Value, FeedbackError> {
        let report = report_of(body, hex)?;
        {
            let mut hits = self.hits.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let list = hits.entry(hex.to_owned()).or_default();
            let at = Self::now();
            list.retain(|t| at - *t < 3600);
            if list.len() >= PER_HOUR {
                return Err(FeedbackError::Limited(format!("{PER_HOUR} retours par heure au plus : merci, réessayez plus tard")));
            }
            list.push(at);
        }
        let setting = |k: &'static str| coop_config::setting(&self.db, &self.uplanet_name, k);
        let (token, owner) = (setting("GIT_TOKEN").await, setting("GIT_OWNER").await);
        if let (Some(token), Some(owner)) = (token, owner) {
            let forge = match setting("GIT_HOST").await {
                Some(h) if !h.contains("github.com") => Forge::GitLab(h),
                _ => Forge::GitHub,
            };
            if !owner.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) {
                self.activity.publish_to(Audience::Captain, "alert", "feedback", "GIT_OWNER illisible : les retours sont gardés sur la station".to_owned(), None);
            } else {
                match post_issue(&forge, &token, &owner, &report, &self.limits).await {
                    Ok((url, number)) => return Ok(json!({ "ok": true, "stored": "git", "issue_url": url, "issue_number": number, "repo": format!("{owner}/{}", report.repo), "provider": if forge == Forge::GitHub { "github" } else { "gitlab" } })),
                    Err(e) => {
                        self.activity.publish_to(Audience::Captain, "alert", "feedback", format!("issue non créée ({e}) : le retour est gardé sur la station"), None);
                    }
                }
            }
        }
        // Forge absente ou en panne : rien n'est perdu, le Capitaine le lit dans le journal et est prévenu.
        let data = json!({ "title": report.title, "body": report.body, "repo": report.repo, "label": report.label, "from": hex }).to_string();
        self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('feedback', ?1, ?2)", vec![Self::now().into(), data.into()]).await.map_err(|e| FeedbackError::Internal(e.to_string()))?;
        self.activity.publish_to(Audience::Captain, "system", "feedback", format!("retour reçu pour {} : {}", report.repo, report.title), None);
        Ok(json!({ "ok": true, "stored": "local", "message": "Retour reçu" }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "ab";

    fn body(extra: Value) -> Value {
        let mut v = json!({ "title": "Le bouton ne répond pas", "description": "Je clique et rien ne se passe.", "source": "Coracle", "category": "bug", "app_version": "1.2", "platform": "web" });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    }

    #[test]
    fn a_report_is_cleaned_bounded_and_anonymous_unless_asked() {
        let r = report_of(&body(json!({})), HEX).unwrap();
        assert_eq!((r.repo.as_str(), r.label), ("coracle", "bug"));
        assert!(r.body.contains("**Version :** 1.2") && !r.body.contains(HEX), "pas de clé publique sans demande : {}", r.body);
        assert!(report_of(&body(json!({ "share_identity": true })), HEX).unwrap().body.contains("`ab`"));
        let long = report_of(&body(json!({ "title": "t".repeat(500), "description": format!("a\u{0}b\u{7}{}", "d".repeat(20_000)) })), HEX).unwrap();
        assert_eq!(long.title.chars().count(), TITLE_MAX);
        assert!(!long.body.contains('\u{0}') && !long.body.contains('\u{7}') && long.body.len() < TEXT_MAX + 400);
        assert_eq!(report_of(&body(json!({ "category": "idee" })), HEX).unwrap().label, "feedback");
    }

    #[test]
    fn titles_and_repo_names_are_refused_when_they_could_reach_another_repository() {
        for (k, v) in [("title", "ab"), ("description", ""), ("source", ""), ("source", "../autre"), ("source", "a/b"), ("source", ".git"), ("source", "-x"), ("source", "A B"), ("source", "x%2Fy")] {
            assert!(matches!(report_of(&body(json!({ k: v })), HEX), Err(FeedbackError::Invalid(_))), "{k}={v}");
        }
        assert!(valid_repo("coracle") && valid_repo("astroport.one") && valid_repo("zelkova_2"));
    }

    #[test]
    fn the_request_matches_each_forge_api() {
        let r = report_of(&body(json!({})), HEX).unwrap();
        let (url, h, b) = issue_request(&Forge::GitHub, "tok", "papiche", &r);
        assert_eq!(url, "https://api.github.com/repos/papiche/coracle/issues");
        assert!(h.iter().any(|(k, v)| *k == "Authorization" && v == "Bearer tok"));
        assert_eq!(serde_json::from_str::<Value>(&b).unwrap()["labels"], json!(["bug"]));
        let (url, h, b) = issue_request(&Forge::GitLab("https://git.p2p.legal/".into()), "tok", "grp", &r);
        assert_eq!(url, "https://git.p2p.legal/api/v4/projects/grp%2Fcoracle/issues");
        assert!(h.iter().any(|(k, v)| *k == "PRIVATE-TOKEN" && v == "tok"));
        assert_eq!(serde_json::from_str::<Value>(&b).unwrap()["labels"], "bug");
    }

    #[tokio::test]
    async fn the_issue_is_created_on_the_forge_and_its_address_comes_back() {
        // Faux GitLab local : vérifie ce qu'il reçoit (jeton, corps) et répond comme l'API.
        let app = axum::Router::new().route("/api/v4/projects/{p}/issues", axum::routing::post(|h: axum::http::HeaderMap, axum::Json(b): axum::Json<Value>| async move {
            assert_eq!(h["private-token"], "tok");
            axum::Json(json!({ "web_url": format!("http://forge.test/i/{}", b["title"].as_str().unwrap().len()), "iid": 7 }))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut limits = FetchLimits::new(64 * 1024);
        limits.allow_private = true;
        let r = report_of(&body(json!({})), HEX).unwrap();
        let (url, n) = post_issue(&Forge::GitLab(host.clone()), "tok", "grp", &r, &limits).await.unwrap();
        assert_eq!((url.as_str(), n), ("http://forge.test/i/24", 7));
        // Adresse privée refusée par le chargeur borné (production : allow_private = false).
        assert!(post_issue(&Forge::GitLab(host), "tok", "grp", &r, &FetchLimits::new(64 * 1024)).await.is_err());
    }
}
