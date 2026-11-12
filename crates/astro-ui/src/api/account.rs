//! Compte : profil, site, clés, jetons, appareils, restauration, retour d'usage.
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// Compte : profil, site, clés, jetons, appareils, restauration, retour d'usage.
pub trait AccountApi: Auth {
    /// Empreinte du compte sur les communs : disque utilisé et quota de sa formule, Opex et Capex.
    fn footprint(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Comptes hébergés : disque, cycle de vie (Capitaine).
    fn accounts(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Jeton de secours d'un compte hébergé (Capitaine).
    fn account_token(&self, who: &Principal, hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Fixe (ou retire, avec `None`) le quota d'un compte hébergé (Capitaine) : il remplace celui de la formule du compte.
    fn account_quota(&self, _who: &Principal, _hex: String, _quota_bytes: Option<u64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("quota non réglable sur cette station")) }
    }
    /// Appareils du compte : un jeton WebDAV chacun, confiné à un dossier du uCloud.
    fn devices_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("appareils non disponibles sur cette station")) }
    }
    /// Ajoute un appareil (corps JSON : `label`, `root`, `mode`) ; rend son jeton, montré une seule fois.
    fn devices_add(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("appareils non disponibles sur cette station")) }
    }
    /// Révoque un appareil.
    fn devices_revoke(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("appareils non disponibles sur cette station")) }
    }
    /// « Mon compte » : identité, profil, accès WebDAV, relais annoncés.
    fn account(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Site public du compte (dossier `/Public`) : nom, adresses, état.
    fn account_site(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("site public non disponible sur cette station")) }
    }
    /// Dépose le site d'exemple dans `/Public` (`force` : écrase la page d'accueil existante).
    fn account_site_example(&self, _who: &Principal, _force: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("site public non disponible sur cette station")) }
    }
    /// Reçoit l'image de profil du compte : `{ kind: "picture" | "banner", data: base64, origin? }`.
    fn account_image(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("images de profil non disponibles sur cette station")) }
    }
    /// Image de profil publique d'un compte (JPEG), si la station en garde une.
    fn profile_image(&self, _hex: &str, _kind: &str) -> impl Future<Output = Option<Vec<u8>>> + Send {
        async { None }
    }
    /// Position du membre (arrondie par la station à la maille UMAP de 0,01°) : la station la plus proche tient ses
    /// informations de quartier. `{lat, lon}`, ou `{}` pour l'effacer.
    fn account_position(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("position non disponible sur cette station")) }
    }
    /// Modifie le profil (`name`, `about`, `picture`, `banner`, `lang`).
    fn account_profile(&self, _who: &Principal, _patch: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Connexion par code PASS (public) : rend le jeton de session. `ip` : limite de débit par adresse.
    fn account_unlock(&self, _ip: Option<std::net::IpAddr>, _email: String, _pass: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("connexion par PASS non disponible sur cette station")) }
    }
    /// Signe un événement avec la clé du compte (signataire distant : la clé ne quitte jamais la station).
    fn account_sign(&self, _who: &Principal, _event: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("signature non disponible sur cette station")) }
    }
    /// Le Capitaine lève le verrouillage du PASS d'un compte.
    fn pass_unlock(&self, _who: &Principal, _hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("connexion par PASS non disponible sur cette station")) }
    }
    /// Le Capitaine éjecte un MULTIPASS créé ici qui ne s'est jamais connecté en 24 h (aucune Ğ1 reçue).
    fn account_eject(&self, _who: &Principal, _hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("éjection non disponible sur cette station")) }
    }
    /// Change le jeton WebDAV : l'ancien cesse de fonctionner.
    fn account_token_rotate(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Exporte les clés secrètes du compte (confirmation explicite).
    fn account_export_keys(&self, _who: &Principal, _confirm: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Retour d'un utilisateur (`POST /api/v1/feedback`) : issue Git de la coopérative, ou gardé sur la station.
    fn feedback(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("retours non activés sur cette station")) }
    }
    /// Le MULTIPASS a-t-il déclaré une IA (Ollama ou clé d'API) ? Sans cela, l'onglet BRO n'apparaît pas.
    fn ai_declared(&self, _who: &Principal) -> impl Future<Output = bool> + Send {
        async { false }
    }
    /// Le compte peut-il créer des Ğ1Billets ? (le Capitaine, ou un MULTIPASS qu'il a autorisé)
    fn billet_issuer(&self, _who: &Principal) -> impl Future<Output = bool> + Send {
        async { false }
    }
}

pub(super) fn routes<B: AccountApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        .route("/api/v1/me", get(me::<B>))
        .route("/api/v1/devices", get(devices_list::<B>).post(devices_add::<B>))
        .route("/api/v1/devices/revoke", post(devices_revoke::<B>))
        .route("/api/v1/me/footprint", get(footprint::<B>))
        .route("/api/v1/feedback", post(feedback::<B>).options(cors_preflight))
        .route("/api/v1/account", get(account::<B>))
        .route("/api/v1/account/profile", post(account_profile::<B>))
        .route("/api/v1/account/position", post(account_position::<B>))
        .route("/api/v1/account/image", post(account_image::<B>))
        .route("/api/v1/account/site", get(account_site::<B>))
        .route("/api/v1/account/site/example", post(account_site_example::<B>))
        .route("/api/v1/profile/{hex}/{file}", get(profile_image::<B>))
        .route("/api/v1/account/token", post(account_token_rotate::<B>))
        .route("/api/v1/account/unlock", post(account_unlock::<B>).options(cors_preflight))
        .route("/api/v1/account/sign", post(account_sign::<B>))
        .route("/api/v1/station/accounts/{hex}/unlock-pass", post(pass_unlock::<B>))
        .route("/api/v1/station/accounts/{hex}/eject", post(account_eject::<B>))
        .route("/api/v1/account/keys", post(account_keys::<B>).options(cors_preflight))
        .route("/api/v1/station/accounts", get(accounts::<B>))
        .route("/api/v1/station/accounts/{hex}/token", post(account_token::<B>))
        .route("/api/v1/station/accounts/{hex}/quota", post(account_quota::<B>))
}

async fn footprint<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.footprint(&p).await)
}

async fn accounts<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.accounts(&p).await)
}

async fn account_token<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
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
struct QuotaBody {
    /// Octets ; absent ou `null` : retire le quota propre au compte (retour à celui de sa formule).
    quota_bytes: Option<u64>,
}

async fn account_quota<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>, Json(body): Json<QuotaBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.account_quota(&p, hex.to_ascii_lowercase(), body.quota_bytes).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn devices_list<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.devices_list(&p).await)
}

async fn devices_add<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.devices_add(&p, body).await)
}

async fn devices_revoke<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.devices_revoke(&p, body.id).await)
}

async fn account<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account(&p).await)
}

async fn account_site<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account_site(&p).await)
}

#[derive(Deserialize, Default)]
struct SiteExampleBody {
    #[serde(default)]
    force: bool,
}

async fn account_site_example<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, body: Option<Json<SiteExampleBody>>) -> Response {
    reply(st.backend.account_site_example(&p, body.map(|b| b.0.force).unwrap_or(false)).await)
}

async fn account_image<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_image(&p, body).await)
}

/// Image de profil : publique (un profil Nostr l'est), servie telle qu'elle a été réencodée, mise en cache une heure.
async fn profile_image<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Path((hex, file)): Path<(String, String)>) -> Response {
    let kind = file.strip_suffix(".jpg").unwrap_or("");
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return reply(Err(ApiError::not_found("image inconnue")));
    }
    match st.backend.profile_image(&hex.to_ascii_lowercase(), kind).await {
        Some(bytes) => (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "image/jpeg"), (axum::http::header::CACHE_CONTROL, "public, max-age=3600"), (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")], bytes).into_response(),
        None => reply(Err(ApiError::not_found("image inconnue"))),
    }
}

async fn account_profile<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_profile(&p, body).await)
}

async fn account_position<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_position(&p, body).await)
}

async fn account_token_rotate<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account_token_rotate(&p).await)
}

#[derive(Deserialize)]
struct UnlockBody {
    email: String,
    pass: String,
}

/// Connexion par code PASS : publique (c'est la porte d'entrée). Le jeton rendu n'est jamais mis en cache.
async fn account_unlock<B: AccountApi>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 1024).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<UnlockBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"pass\": \"123456\" }"))) };
    any_origin(reply(st.backend.account_unlock(ip, body.email, body.pass).await))
}

async fn account_sign<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_sign(&p, body).await)
}

async fn pass_unlock<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.pass_unlock(&p, hex.to_ascii_lowercase()).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn account_eject<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.account_eject(&p, hex.to_ascii_lowercase()).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

#[derive(serde::Deserialize)]
struct KeysBody {
    #[serde(default)]
    confirm: bool,
}

async fn account_keys<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<KeysBody>) -> Response {
    any_origin(reply(st.backend.account_export_keys(&p, body.confirm).await))
}

/// Retour d'un utilisateur : authentifié (le jeton Git de la coopérative n'est jamais offert à un anonyme).
async fn feedback<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    if body.len() > 32 * 1024 {
        return any_origin(reply(Err(ApiError::bad_request("retour trop long (32 Ko au plus)"))));
    }
    let _ = headers;
    let Ok(v) = serde_json::from_slice::<Value>(&body) else { return any_origin(reply(Err(ApiError::bad_request("JSON attendu : { \"title\", \"description\", \"source\", \"category\"? }")))) };
    any_origin(reply(st.backend.feedback(&p, v).await))
}

async fn me<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    private(Json(json!({ "hex": p.hex, "email": p.email, "captain": p.captain, "ai": st.backend.ai_declared(&p).await, "billet_issuer": st.backend.billet_issuer(&p).await })).into_response())
}
