//! API JSON de l'interface web, supervision et flux d'activité en direct.
//!
//! Routes (toutes authentifiées, sauf `/health`) :
//!
//! | Route | Rôle |
//! |---|---|
//! | `GET /health` | état des sous-systèmes (public, sans donnée personnelle) |
//! | `GET /api/v1/me` | le compte connecté |
//! | `GET /api/v1/dashboard` | tableau de bord et widget de souveraineté |
//! | `GET /api/v1/activity` | flux SSE des événements de la station |
//! | `GET /api/v1/ucloud/list?path=` | dossier uCloud |
//! | `GET /api/v1/ucloud/photos?before=&limit=` | album photo (du plus récent au plus ancien) |
//! | `POST /api/v1/ucloud/share` | lien magique `#clé` + QR |
//! | `GET /api/v1/faces` | visages reconnus et suggestions « est-ce X ? » |
//! | `POST /api/v1/faces/{id}/accept` · `/name` | validation d'un visage |
//! | `POST /api/v1/bro/say` | dialogue avec BRO |
//!
//! L'authentification est portée par l'en-tête `Authorization` (Basic ou NIP-98), jamais par un cookie : l'API
//! n'est donc pas exposée aux requêtes forgées depuis un autre site.

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astro_core::ActivityBus;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

/// Compte connecté.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Clé Nostr du compte (64 hex).
    pub hex: String,
    /// Adresse email, si connue.
    pub email: Option<String>,
    /// Le compte est celui du Capitaine.
    pub captain: bool,
}

/// Erreur d'API : statut HTTP et message pour l'humain.
#[derive(Debug)]
pub struct ApiError {
    /// Statut HTTP.
    pub status: StatusCode,
    /// Message.
    pub message: String,
}

impl ApiError {
    /// 400.
    pub fn bad_request(m: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: m.into() }
    }

    /// 404.
    pub fn not_found(m: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: m.into() }
    }

    /// 403 : réservé à un autre rôle (le Capitaine).
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self { status: StatusCode::FORBIDDEN, message: m.into() }
    }

    /// 409 : la ressource existe déjà (email déjà inscrit).
    pub fn conflict(m: impl Into<String>) -> Self {
        Self { status: StatusCode::CONFLICT, message: m.into() }
    }

    /// 429 : trop de demandes.
    pub fn too_many(m: impl Into<String>) -> Self {
        Self { status: StatusCode::TOO_MANY_REQUESTS, message: m.into() }
    }

    /// 501 : fonction non activée sur cette station.
    pub fn unavailable(m: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_IMPLEMENTED, message: m.into() }
    }

    /// 500 (le détail technique reste dans les journaux, pas dans la réponse).
    pub fn internal(m: impl Into<String>) -> Self {
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: m.into() }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

/// État d'un sous-système.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Fonctionne.
    Ok,
    /// Fonctionne avec une limite (retard, nœud de secours…).
    Degraded,
    /// En panne.
    Down,
    /// Non activé sur cette station.
    Disabled,
}

impl Status {
    /// Nom publié.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Down => "down",
            Self::Disabled => "disabled",
        }
    }
}

/// Un sous-système et son état.
#[derive(Debug, Clone)]
pub struct Subsystem {
    /// Nom (`db`, `relay`, `p2p`, `duniter`, `bro`…).
    pub name: &'static str,
    /// État.
    pub status: Status,
    /// Détail lisible, sans donnée personnelle.
    pub detail: String,
}

impl Subsystem {
    /// Construit un état.
    pub fn new(name: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Self { name, status, detail: detail.into() }
    }
}

/// Ce que l'interface demande à la station. Implémenté par le binaire, qui câble base, WebDAV, FaceCloud, BRO,
/// Duniter et P2P ; l'interface elle-même ne connaît aucun de ces sous-systèmes.
pub trait Backend: Send + Sync + 'static {
    /// Identifie l'appelant (Basic ou NIP-98) ; `None` : non authentifié.
    fn authenticate(&self, headers: &HeaderMap, method: &str, uri: &Uri) -> impl Future<Output = Option<Principal>> + Send;
    /// Tableau de bord et widget de souveraineté.
    fn dashboard(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Contenu d'un dossier uCloud.
    fn ucloud_list(&self, who: &Principal, path: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Album : photos et vidéos, `before` = horodatage exclusif (pagination).
    fn ucloud_photos(&self, who: &Principal, before: Option<i64>, limit: usize) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Crée un lien magique ; doit renvoyer `{ "url", "expires_at", "items" }`.
    fn ucloud_share(&self, who: &Principal, paths: Vec<String>, ttl_secs: i64, origin: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Visages reconnus et suggestions.
    fn faces(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Valide la suggestion « est-ce X ? ».
    fn face_accept(&self, who: &Principal, face_id: i64) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Nomme un visage.
    fn face_name(&self, who: &Principal, face_id: i64, name: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Empreinte du compte sur les communs : disque utilisé et quota, baux 1/24e, tarifs du nœud.
    fn footprint(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Constellation : nœuds de l'essaim, Power-Score, distance, statut, services (Capitaine).
    fn constellation(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Régie coopérative : parts 3×1/3 de la dernière semaine, provision fiscale, Love Ledger (Capitaine).
    fn economy(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Comptes hébergés : disque, cycle de vie (Capitaine).
    fn accounts(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Jeton de secours d'un compte hébergé (Capitaine).
    fn account_token(&self, who: &Principal, hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Dialogue avec BRO.
    fn bro_say(&self, who: &Principal, text: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Attestation de copie privée du compte : `{ accepted, version, accepted_at, text }`.
    fn consent(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("dépôt de médias non disponible sur cette station")) }
    }
    /// Enregistre l'attestation de copie privée (signée de la clé du titulaire) ; `version` = texte affiché.
    fn consent_accept(&self, _who: &Principal, _version: u32) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("dépôt de médias non disponible sur cette station")) }
    }
    /// L'inscription d'un MULTIPASS est-elle ouverte ? `{ open, constellation, accounts, max_accounts }` (public).
    fn register_info(&self) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("inscription non disponible sur cette station")) }
    }
    /// Crée un MULTIPASS (public). `ip` : adresse du client, pour la limite de débit.
    fn register(&self, _ip: Option<std::net::IpAddr>, _email: String, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("inscription non disponible sur cette station")) }
    }
    /// État des sous-systèmes.
    fn health(&self) -> impl Future<Output = Vec<Subsystem>> + Send;
}

/// État partagé des routes.
pub struct UiState<B> {
    backend: Arc<B>,
    activity: ActivityBus,
    started: Instant,
    /// Origine publique (`https://domaine`) ; sinon déduite de l'en-tête `Host`.
    public_origin: Option<String>,
}

/// Routeur de l'API et de `/health`.
pub fn router<B: Backend>(backend: Arc<B>, activity: ActivityBus, public_origin: Option<String>) -> Router {
    let state = Arc::new(UiState { backend, activity, started: Instant::now(), public_origin });
    Router::new()
        .route("/health", get(health::<B>))
        .route("/api/v1/me", get(me::<B>))
        .route("/api/v1/dashboard", get(dashboard::<B>))
        .route("/api/v1/activity", get(activity_stream::<B>))
        .route("/api/v1/ucloud/list", get(ucloud_list::<B>))
        .route("/api/v1/ucloud/photos", get(ucloud_photos::<B>))
        .route("/api/v1/ucloud/share", post(ucloud_share::<B>))
        .route("/api/v1/faces", get(faces::<B>))
        .route("/api/v1/faces/{id}/accept", post(face_accept::<B>))
        .route("/api/v1/faces/{id}/name", post(face_name::<B>))
        .route("/api/v1/bro/say", post(bro_say::<B>))
        .route("/api/v1/consent/private-copy", get(consent::<B>).post(consent_accept::<B>))
        .route("/api/v1/me/footprint", get(footprint::<B>))
        .route("/api/v1/register", get(register_info::<B>).post(register::<B>))
        .route("/api/v1/station/constellation", get(constellation::<B>))
        .route("/api/v1/station/economy", get(economy::<B>))
        .route("/api/v1/station/accounts", get(accounts::<B>))
        .route("/api/v1/station/accounts/{hex}/token", post(account_token::<B>))
        .with_state(state)
}

/// 401 : seul le schéma `Nostr` est annoncé. Annoncer `Basic` ferait ouvrir au navigateur sa propre fenêtre de
/// connexion à chaque appel `fetch` ; l'application gère elle-même la saisie (Basic reste accepté si fourni).
fn unauthorized() -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, Json(json!({ "error": "authentification requise" }))).into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Nostr realm=\"astroportd\""));
    r
}

async fn who<B: Backend>(st: &UiState<B>, headers: &HeaderMap, method: &Method, uri: &Uri) -> Result<Principal, Response> {
    st.backend.authenticate(headers, method.as_str(), uri).await.ok_or_else(unauthorized)
}

fn private(mut r: Response) -> Response {
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    r
}

fn reply(result: Result<Value, ApiError>) -> Response {
    private(match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    })
}

async fn health<B: Backend>(State(st): State<Arc<UiState<B>>>) -> Response {
    let subsystems = st.backend.health().await;
    let overall = if subsystems.iter().any(|s| s.status == Status::Down) {
        Status::Down
    } else if subsystems.iter().any(|s| s.status == Status::Degraded) {
        Status::Degraded
    } else {
        Status::Ok
    };
    let map: serde_json::Map<String, Value> = subsystems.iter().map(|s| (s.name.to_owned(), json!({ "status": s.status.as_str(), "detail": s.detail }))).collect();
    let body = json!({ "status": overall.as_str(), "version": env!("CARGO_PKG_VERSION"), "uptime_secs": st.started.elapsed().as_secs(), "subsystems": map });
    let code = if overall == Status::Down { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::OK };
    private((code, Json(body)).into_response())
}

async fn me<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => private(Json(json!({ "hex": p.hex, "email": p.email, "captain": p.captain })).into_response()),
        Err(r) => r,
    }
}

async fn dashboard<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.dashboard(&p).await),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ListQuery {
    path: Option<String>,
}

async fn ucloud_list<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Query(q): Query<ListQuery>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.ucloud_list(&p, q.path.unwrap_or_else(|| "/".into())).await),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct PhotosQuery {
    before: Option<i64>,
    limit: Option<usize>,
}

async fn ucloud_photos<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Query(q): Query<PhotosQuery>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.ucloud_photos(&p, q.before, q.limit.unwrap_or(60).clamp(1, 200)).await),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ShareBody {
    paths: Vec<String>,
    ttl_hours: Option<u32>,
}

async fn ucloud_share<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<ShareBody>) -> Response {
    let principal = match who(&st, &headers, &method, &uri).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if body.paths.is_empty() || body.paths.len() > 200 {
        return reply(Err(ApiError::bad_request("1 à 200 chemins attendus")));
    }
    let origin = st
        .public_origin
        .clone()
        .or_else(|| headers.get(header::HOST).and_then(|h| h.to_str().ok()).map(|h| format!("http://{h}")))
        .unwrap_or_default();
    let ttl_secs = i64::from(body.ttl_hours.unwrap_or(72).clamp(1, 720)) * 3600;
    let mut out = match st.backend.ucloud_share(&principal, body.paths, ttl_secs, origin).await {
        Ok(v) => v,
        Err(e) => return reply(Err(e)),
    };
    // Le QR est rendu ici à partir de l'URL complète (`#clé` comprise) : l'application n'a rien à calculer.
    if let Some(url) = out.get("url").and_then(Value::as_str) {
        let svg = crate::qr::svg(url);
        if let (Some(obj), Some(svg)) = (out.as_object_mut(), svg) {
            obj.insert("qr_svg".into(), Value::String(svg));
        }
    }
    reply(Ok(out))
}

async fn faces<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.faces(&p).await),
        Err(r) => r,
    }
}

async fn face_accept<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(id): Path<i64>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.face_accept(&p, id).await),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn face_name<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(id): Path<i64>, Json(body): Json<NameBody>) -> Response {
    let name = body.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 80 {
        return reply(Err(ApiError::bad_request("nom de 1 à 80 caractères attendu")));
    }
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.face_name(&p, id, name).await),
        Err(r) => r,
    }
}

async fn footprint<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.footprint(&p).await),
        Err(r) => r,
    }
}

/// Les écrans d'administration sont réservés au Capitaine : 403 pour tout autre compte, même authentifié.
#[allow(clippy::result_large_err)]
fn captain_only(p: &Principal) -> Result<(), Response> {
    if p.captain { Ok(()) } else { Err(reply(Err(ApiError::forbidden("réservé au Capitaine de la station")))) }
}

async fn constellation<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) => reply(st.backend.constellation(&p).await),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn economy<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) => reply(st.backend.economy(&p).await),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn accounts<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) => reply(st.backend.accounts(&p).await),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn account_token<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.account_token(&p, hex.to_ascii_lowercase()).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct SayBody {
    text: String,
}

async fn consent<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.consent(&p).await),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ConsentBody {
    version: u32,
}

async fn consent_accept<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<ConsentBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.consent_accept(&p, body.version).await),
        Err(r) => r,
    }
}

async fn bro_say<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<SayBody>) -> Response {
    let text = body.text.trim().to_owned();
    if text.is_empty() || text.chars().count() > 2000 {
        return reply(Err(ApiError::bad_request("message de 1 à 2000 caractères attendu")));
    }
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.bro_say(&p, text).await),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct RegisterBody {
    email: String,
    #[serde(default)]
    lang: String,
}

/// Inscription ouverte ? (aucun compte requis : c'est la porte d'entrée.)
async fn register_info<B: Backend>(State(st): State<Arc<UiState<B>>>) -> Response {
    reply(st.backend.register_info().await)
}

/// Crée un MULTIPASS. Les secrets de la réponse ne sont jamais remis une seconde fois : `no-store`.
async fn register<B: Backend>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<RegisterBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"lang\": \"fr\" }"))) };
    reply(st.backend.register(ip, body.email, body.lang).await)
}

/// Flux SSE : d'abord ce que le client a manqué (`Last-Event-ID`), puis le direct.
async fn activity_stream<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    if let Err(r) = who(&st, &headers, &method, &uri).await {
        return r;
    }
    let after = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let (backlog, rx) = st.activity.subscribe(after);
    let sse = Sse::new(event_stream(backlog, rx)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"));
    private(sse.into_response())
}

type Live = tokio::sync::broadcast::Receiver<Arc<astro_core::Activity>>;

fn to_event(a: &astro_core::Activity) -> Event {
    Event::default().id(a.id.to_string()).event(a.kind.clone()).data(serde_json::to_string(a).unwrap_or_default())
}

fn event_stream(backlog: Vec<Arc<astro_core::Activity>>, rx: Live) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold((backlog.into_iter(), rx), |(mut backlog, mut rx)| async move {
        if let Some(a) = backlog.next() {
            return Some((Ok(to_event(&a)), (backlog, rx)));
        }
        match rx.recv().await {
            Ok(a) => Some((Ok(to_event(&a)), (backlog, rx))),
            // Le client lisait trop lentement : on le dit plutôt que de laisser croire à un flux complet.
            Err(RecvError::Lagged(n)) => Some((Ok(Event::default().event("lag").data(n.to_string())), (backlog, rx))),
            Err(RecvError::Closed) => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    const ALICE: &str = "aa";

    struct Fake;

    impl Backend for Fake {
        async fn authenticate(&self, headers: &HeaderMap, _: &str, _: &Uri) -> Option<Principal> {
            let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
            matches!(auth, "Basic ok" | "Basic cap").then(|| Principal { hex: ALICE.into(), email: Some("alice@example.org".into()), captain: auth == "Basic cap" })
        }
        async fn dashboard(&self, p: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "who": p.hex }))
        }
        async fn ucloud_list(&self, _: &Principal, path: String) -> Result<Value, ApiError> {
            Ok(json!({ "path": path, "entries": [] }))
        }
        async fn ucloud_photos(&self, _: &Principal, before: Option<i64>, limit: usize) -> Result<Value, ApiError> {
            Ok(json!({ "before": before, "limit": limit }))
        }
        async fn ucloud_share(&self, _: &Principal, paths: Vec<String>, ttl: i64, origin: String) -> Result<Value, ApiError> {
            Ok(json!({ "url": format!("{origin}/s/abc#{}", "cd".repeat(32)), "items": paths.len(), "expires_at": ttl }))
        }
        async fn faces(&self, _: &Principal) -> Result<Value, ApiError> {
            Err(ApiError::unavailable("FaceCloud non activé"))
        }
        async fn face_accept(&self, _: &Principal, id: i64) -> Result<Value, ApiError> {
            Ok(json!({ "accepted": id }))
        }
        async fn face_name(&self, _: &Principal, id: i64, name: String) -> Result<Value, ApiError> {
            Ok(json!({ "id": id, "name": name }))
        }
        async fn bro_say(&self, _: &Principal, text: String) -> Result<Value, ApiError> {
            Ok(json!({ "reply": format!("écho : {text}") }))
        }
        async fn footprint(&self, p: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "who": p.hex }))
        }
        async fn constellation(&self, _: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "nodes": [] }))
        }
        async fn economy(&self, _: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "week": "2026-W43" }))
        }
        async fn accounts(&self, _: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "accounts": [] }))
        }
        async fn account_token(&self, _: &Principal, hex: String) -> Result<Value, ApiError> {
            Ok(json!({ "for": hex }))
        }
        async fn health(&self) -> Vec<Subsystem> {
            vec![Subsystem::new("db", Status::Ok, "WAL"), Subsystem::new("p2p", Status::Disabled, "non activé")]
        }
    }

    fn app(bus: &ActivityBus) -> Router {
        router(Arc::new(Fake), bus.clone(), None)
    }

    async fn call(app: Router, method: Method, uri: &str, auth: bool, body: Option<Value>) -> (StatusCode, Value, HeaderMap) {
        call_as(app, method, uri, if auth { "Basic ok" } else { "" }, body).await
    }

    async fn call_as(app: Router, method: Method, uri: &str, auth: &str, body: Option<Value>) -> (StatusCode, Value, HeaderMap) {
        let mut req = Request::builder().method(method).uri(uri).header(header::HOST, "station.test");
        if !auth.is_empty() {
            req = req.header(header::AUTHORIZATION, auth);
        }
        let body = match body {
            Some(v) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = app.oneshot(req.body(body).unwrap()).await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), headers)
    }

    #[tokio::test]
    async fn every_api_route_but_health_requires_authentication() {
        let bus = ActivityBus::new();
        for (method, uri) in [(Method::GET, "/api/v1/me"), (Method::GET, "/api/v1/dashboard"), (Method::GET, "/api/v1/ucloud/list"), (Method::GET, "/api/v1/ucloud/photos"), (Method::GET, "/api/v1/faces"), (Method::GET, "/api/v1/activity")] {
            let (s, _, h) = call(app(&bus), method, uri, false, None).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}");
            assert_eq!(h[header::WWW_AUTHENTICATE], "Nostr realm=\"astroportd\"", "pas de Basic annoncé : pas de fenêtre native du navigateur");
        }
        let (s, ..) = call(app(&bus), Method::POST, "/api/v1/bro/say", false, Some(json!({ "text": "salut" }))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert_eq!(call(app(&bus), Method::GET, "/health", false, None).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn authenticated_calls_reach_the_backend_and_are_never_cached() {
        let bus = ActivityBus::new();
        let (s, v, h) = call(app(&bus), Method::GET, "/api/v1/ucloud/list?path=/Photos", true, None).await;
        assert_eq!((s, v["path"].as_str()), (StatusCode::OK, Some("/Photos")));
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        assert_eq!(call(app(&bus), Method::GET, "/api/v1/ucloud/list", true, None).await.1["path"], "/");
        let photos = call(app(&bus), Method::GET, "/api/v1/ucloud/photos?limit=100000&before=42", true, None).await.1;
        assert_eq!((photos["limit"].as_u64(), photos["before"].as_i64()), (Some(200), Some(42)), "limite bornée");
        assert_eq!(call(app(&bus), Method::POST, "/api/v1/faces/7/accept", true, None).await.1["accepted"], 7);
        let named = call(app(&bus), Method::POST, "/api/v1/faces/7/name", true, Some(json!({ "name": "  Léa " }))).await.1;
        assert_eq!(named["name"], "Léa");
        let (s, v, _) = call(app(&bus), Method::GET, "/api/v1/faces", true, None).await;
        assert_eq!((s, v["error"].as_str()), (StatusCode::NOT_IMPLEMENTED, Some("FaceCloud non activé")));
    }

    #[tokio::test]
    async fn station_administration_is_for_the_captain_only() {
        let bus = ActivityBus::new();
        let hex64 = "ab".repeat(32);
        for uri in ["/api/v1/station/constellation", "/api/v1/station/economy", "/api/v1/station/accounts"] {
            assert_eq!(call(app(&bus), Method::GET, uri, false, None).await.0, StatusCode::UNAUTHORIZED, "{uri} sans compte");
            let (s, v, _) = call_as(app(&bus), Method::GET, uri, "Basic ok", None).await;
            assert_eq!((s, v["error"].as_str()), (StatusCode::FORBIDDEN, Some("réservé au Capitaine de la station")), "{uri} : un MULTIPASS n'entre pas");
            assert_eq!(call_as(app(&bus), Method::GET, uri, "Basic cap", None).await.0, StatusCode::OK, "{uri} pour le Capitaine");
        }
        let token_uri = format!("/api/v1/station/accounts/{hex64}/token");
        assert_eq!(call_as(app(&bus), Method::POST, &token_uri, "Basic ok", None).await.0, StatusCode::FORBIDDEN);
        let (s, v, _) = call_as(app(&bus), Method::POST, &token_uri, "Basic cap", None).await;
        assert_eq!((s, v["for"].as_str()), (StatusCode::OK, Some(hex64.as_str())));
        assert_eq!(call_as(app(&bus), Method::POST, "/api/v1/station/accounts/zz/token", "Basic cap", None).await.0, StatusCode::BAD_REQUEST);
        // L'empreinte sur les communs est personnelle : tout compte la voit.
        assert_eq!(call_as(app(&bus), Method::GET, "/api/v1/me/footprint", "Basic ok", None).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn inputs_are_validated_before_reaching_the_backend() {
        let bus = ActivityBus::new();
        assert_eq!(call(app(&bus), Method::POST, "/api/v1/faces/1/name", true, Some(json!({ "name": "   " }))).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(app(&bus), Method::POST, "/api/v1/bro/say", true, Some(json!({ "text": "x".repeat(2001) }))).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(app(&bus), Method::POST, "/api/v1/ucloud/share", true, Some(json!({ "paths": [] }))).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(app(&bus), Method::POST, "/api/v1/faces/abc/accept", true, None).await.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn sharing_returns_the_magic_link_and_its_qr_code() {
        let bus = ActivityBus::new();
        let (s, v, _) = call(app(&bus), Method::POST, "/api/v1/ucloud/share", true, Some(json!({ "paths": ["/Photos/vacances"], "ttl_hours": 99999 }))).await;
        assert_eq!(s, StatusCode::OK);
        let url = v["url"].as_str().unwrap();
        assert!(url.starts_with("http://station.test/s/abc#") && url.len() > 64 + 20, "{url}");
        assert_eq!(v["expires_at"], 720 * 3600, "durée bornée à 30 jours");
        assert!(v["qr_svg"].as_str().unwrap().contains("<svg"));
    }

    #[tokio::test]
    async fn health_reports_each_subsystem_and_turns_503_when_one_is_down() {
        let bus = ActivityBus::new();
        let (s, v, _) = call(app(&bus), Method::GET, "/health", false, None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((v["status"].as_str(), v["subsystems"]["db"]["status"].as_str(), v["subsystems"]["p2p"]["status"].as_str()), (Some("ok"), Some("ok"), Some("disabled")));

        struct Broken;
        impl Backend for Broken {
            async fn authenticate(&self, _: &HeaderMap, _: &str, _: &Uri) -> Option<Principal> {
                None
            }
            async fn dashboard(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn ucloud_list(&self, _: &Principal, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn ucloud_photos(&self, _: &Principal, _: Option<i64>, _: usize) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn ucloud_share(&self, _: &Principal, _: Vec<String>, _: i64, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn faces(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn face_accept(&self, _: &Principal, _: i64) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn face_name(&self, _: &Principal, _: i64, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn bro_say(&self, _: &Principal, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn footprint(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn constellation(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn economy(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn accounts(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn account_token(&self, _: &Principal, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn health(&self) -> Vec<Subsystem> {
                vec![Subsystem::new("db", Status::Down, "verrouillée"), Subsystem::new("relay", Status::Degraded, "lent")]
            }
        }
        let (s, v, _) = call(router(Arc::new(Broken), bus, None), Method::GET, "/health", false, None).await;
        assert_eq!((s, v["status"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("down")));
    }

    #[tokio::test]
    async fn the_activity_stream_replays_what_was_missed_then_goes_live() {
        let bus = ActivityBus::new();
        bus.publish("bro", "t", "premier", None);
        bus.publish("relay", "t", "deuxième", None);
        let req = Request::builder().uri("/api/v1/activity").header(header::AUTHORIZATION, "Basic ok").header("last-event-id", "1").body(Body::empty()).unwrap();
        let resp = app(&bus).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
        let mut body = resp.into_body();
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        let text = String::from_utf8(first.to_vec()).unwrap();
        assert!(text.contains("id: 2") && text.contains("event: relay") && text.contains("deuxième"), "{text}");
        assert!(!text.contains("premier"), "déjà vu : Last-Event-ID = 1");
        bus.publish("economy", "t", "en direct", None);
        let live = String::from_utf8(body.frame().await.unwrap().unwrap().into_data().unwrap().to_vec()).unwrap();
        assert!(live.contains("id: 3") && live.contains("en direct"), "{live}");
    }
}
