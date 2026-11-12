//! Accueil : invitations, liens d'accueil, inscription, annuaire des stations, carte de la station.
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// Accueil : invitations, liens d'accueil, inscription, annuaire des stations, carte de la station.
pub trait OnboardApi: Auth {
    /// Liens d'accueil de la station (Capitaine) : `{ links: [...] }`.
    fn welcome_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("liens d'accueil non disponibles sur cette station")) }
    }
    /// Crée un lien d'accueil `{ label, max_uses?, days? }` (Capitaine) : `{ token, … }`.
    fn welcome_create(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("liens d'accueil non disponibles sur cette station")) }
    }
    /// Retire un lien d'accueil (Capitaine).
    fn welcome_revoke(&self, _who: &Principal, _token: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("liens d'accueil non disponibles sur cette station")) }
    }
    /// Ce que voit un visiteur d'un lien d'accueil (public).
    fn welcome_preview(&self, _token: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::not_found("lien d'accueil inconnu")) }
    }
    /// Arrivée par un lien d'accueil (public) : crée le MULTIPASS, amitié réciproque avec l'hôte ; `chat_peer` = l'hôte.
    fn welcome_arrive(&self, _ip: Option<std::net::IpAddr>, _token: String, _email: Option<String>, _alias: Option<String>, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::not_found("lien d'accueil inconnu")) }
    }
    /// Offre de parrainage (public, usage unique) : `{ sealed }`, chiffrée pour le seul détenteur du jeton.
    fn join_take(&self, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::not_found("offre de parrainage introuvable")) }
    }
    /// L'inscription d'un MULTIPASS est-elle ouverte ? `{ open, constellation, accounts, max_accounts }` (public).
    fn register_info(&self) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("inscription non disponible sur cette station")) }
    }
    /// Annuaire public des stations de la même UPlanet (balises 30850 récentes) : places, position arrondie, matériel.
    fn stations(&self) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("annuaire des stations non disponible")) }
    }
    /// Carte publique de la station (`GET /12345`) : identités publiques et adresses, jamais d'adresse e-mail ni de secret.
    fn station_card(&self) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("carte de station non disponible")) }
    }
    /// Crée un MULTIPASS (public). `ip` : adresse du client, pour la limite de débit.
    fn register(&self, _ip: Option<std::net::IpAddr>, _email: String, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("inscription non disponible sur cette station")) }
    }
    /// Crée le compte du **Capitaine** d'une station qui n'en a pas (public, mais gardé par le code d'installation que
    /// seul le journal de la station affiche).
    fn register_captain(&self, _ip: Option<std::net::IpAddr>, _code: String, _email: String, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("cette station n'attend pas de Capitaine")) }
    }
    /// Crée un MULTIPASS invité (public) : pas d'email, adresse générée par la station.
    fn register_guest(&self, _ip: Option<std::net::IpAddr>, _alias: Option<String>, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("inscription non disponible sur cette station")) }
    }
    /// Crée une invitation à recevoir des photos (`{ paths | person, label?, email?, contact_path?, days? }`).
    fn invite_create(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Invitations du compte : jeton, état, dates.
    fn invites(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Retire une invitation.
    fn invite_revoke(&self, _who: &Principal, _token: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Ce que voit le porteur du lien (public) : qui invite, combien de photos.
    fn invite_preview(&self, _token: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Aperçu d'une invitation pour une station qui signe sa demande (NIP-98) : il porte en plus la clé de l'expéditeur.
    fn invite_preview_signed(&self, _headers: &HeaderMap, _method: &str, _uri: &Uri, _token: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Vignette floutée n° `index` d'une invitation (public) : PNG de quelques pixels.
    fn invite_thumb(&self, _token: String, _index: usize) -> impl Future<Output = Result<Vec<u8>, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Accepte une invitation avec le compte authentifié (même station) : double suivi, photos rangées, sans nouveau compte.
    fn invite_accept(&self, _who: &Principal, _token: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Accepte une invitation au nom d'un destinataire qui prouve son identité par NIP-98 (`Authorization: Nostr`) : compte d'ici,
    /// ou compte hébergé sur une autre station qui appelle de station à station. `body` est le corps brut (lié à la signature).
    fn invite_accept_nostr(&self, _headers: &HeaderMap, _method: &str, _uri: &Uri, _token: String, _body: Vec<u8>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Demande l'envoi des photos d'une invitation acceptée depuis une autre station (NIP-98).
    fn invite_deliver(&self, _headers: &HeaderMap, _method: &str, _uri: &Uri, _token: String, _body: Vec<u8>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Accepte, avec le compte authentifié, l'invitation d'**une autre station** (`origin`).
    fn invite_accept_elsewhere(&self, _who: &Principal, _origin: String, _token: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Réclame une invitation (public) : crée le MULTIPASS de `email` et range les photos chez lui.
    fn invite_claim(&self, _ip: Option<std::net::IpAddr>, _token: String, _who: Applicant, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("invitations non disponibles sur cette station")) }
    }
    /// Rétablit sur cette station un MULTIPASS existant depuis sa carte secrète (public : la carte est la preuve). `ip` : limite de débit.
    fn restore(&self, _ip: Option<std::net::IpAddr>, _card: String, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("restauration non disponible sur cette station")) }
    }
    /// Secours par les gardiens (public : la carte imprimée P1 est le justificatif) : `send` prévient les gardiens, sinon
    /// l'état seul. `{ status: none|searching|waiting|ready, sent?, by?, denied?, asks_left? }`.
    fn restore_ask(&self, _card: String, _send: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("secours par les gardiens non disponible sur cette station")) }
    }
}

pub(super) fn routes<B: OnboardApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        .route("/.well-known/nostr/nip96.json", get(nip96_info::<B>))
        .route("/api/v1/register", get(register_info::<B>).post(register::<B>))
        .route("/api/v1/stations", get(stations::<B>))
        .route("/12345", get(station_card::<B>))
        .route("/12345/", get(station_card::<B>))
        .route("/api/v1/restore", post(restore::<B>))
        .route("/api/v1/restore/ask", post(restore_ask::<B>))
        .route("/api/v1/invites", get(invites::<B>).post(invite_create::<B>))
        .route("/api/v1/invites/{token}", axum::routing::delete(invite_revoke::<B>))
        .route("/api/v1/invite/{token}", get(invite_preview::<B>))
        .route("/api/v1/invite/{token}/thumb/{index}", get(invite_thumb::<B>))
        .route("/api/v1/invite/{token}/claim", post(invite_claim::<B>))
        .route("/api/v1/invite/{token}/accept", post(invite_accept::<B>))
        .route("/api/v1/invite/{token}/deliver", post(invite_deliver::<B>))
        .route("/api/v1/invites/remote-accept", post(invite_accept_elsewhere::<B>))
        .route("/api/v1/station/welcome", get(welcome_list::<B>).post(welcome_create::<B>))
        .route("/api/v1/station/welcome/{token}/revoke", post(welcome_revoke::<B>))
        .route("/api/v1/welcome/{token}", get(welcome_preview::<B>).post(welcome_arrive::<B>))
        .route("/api/v1/join/{id}", get(join_take::<B>))
}

/// Adresse `/w/<jeton>` d'un lien d'accueil, et son QR si `qr` (une vingtaine de Ko : jamais pour toute une liste).
fn with_welcome_link(mut v: Value, origin: &str, qr: bool) -> Value {
    if let Some(token) = v.get("token").and_then(Value::as_str).map(str::to_owned) {
        let url = format!("{}/w/{token}", origin.trim_end_matches('/'));
        if let Some(svg) = crate::qr::svg(&url).filter(|_| qr) {
            v["qr_svg"] = Value::String(svg);
        }
        v["url"] = Value::String(url);
    }
    v
}

/// Liens d'accueil du Capitaine ; `?qr=<jeton>` joint le QR de ce seul lien.
async fn welcome_list<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Captain(p): Captain, Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
    let origin = origin_of(&st, &headers);
    let want = q.get("qr").cloned();
    reply(st.backend.welcome_list(&p).await.map(|mut v| {
        if let Some(list) = v.get_mut("links").and_then(Value::as_array_mut) {
            for l in list.iter_mut() {
                let qr = want.is_some() && l["token"].as_str() == want.as_deref();
                *l = with_welcome_link(l.take(), &origin, qr);
            }
        }
        v
    }))
}

async fn welcome_create<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Captain(p): Captain, Json(b): Json<Value>) -> Response {
    let origin = origin_of(&st, &headers);
    reply(st.backend.welcome_create(&p, b).await.map(|v| with_welcome_link(v, &origin, true)))
}

async fn welcome_revoke<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Path(token): Path<String>) -> Response {
    reply(st.backend.welcome_revoke(&p, token).await)
}

/// Lien d'accueil, vu par un visiteur (aucun compte requis : c'est la porte d'entrée).
async fn welcome_preview<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, Path(token): Path<String>) -> Response {
    reply(st.backend.welcome_preview(token).await)
}

/// Arrivée par un lien d'accueil : les secrets de la réponse ne sont remis qu'une fois (`no-store`, comme l'inscription).
async fn welcome_arrive<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, Path(token): Path<String>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<RegisterBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"lang\": \"fr\" }"))) };
    let email = (!body.guest).then_some(body.email);
    reply(st.backend.welcome_arrive(ip, token, email, body.alias, body.lang).await)
}

/// Parrainage : la nouvelle station retire l'offre chiffrée (une seule fois). Sans compte : le jeton est la preuve.
async fn join_take<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, Path(id): Path<String>) -> Response {
    reply(st.backend.join_take(id).await)
}

#[derive(Deserialize)]
struct RegisterBody {
    #[serde(default)]
    email: String,
    /// Sans email : la station génère l'adresse (`alias` : prénom ou pseudo facultatif).
    #[serde(default)]
    guest: bool,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    lang: String,
    /// Code d'installation : crée le compte du Capitaine (page `/welcome`).
    #[serde(default)]
    setup_code: Option<String>,
}

#[derive(Deserialize)]
struct RestoreBody {
    card: String,
    #[serde(default)]
    lang: String,
}

#[derive(Deserialize)]
struct RestoreAskBody {
    card: String,
    #[serde(default)]
    send: bool,
}

/// Inscription ouverte ? (aucun compte requis : c'est la porte d'entrée.)
async fn register_info<B: OnboardApi>(State(st): State<Arc<UiState<B>>>) -> Response {
    any_origin(reply(st.backend.register_info().await))
}

/// Carte de la station au format `12345.json` réduit (public, lecture seule).
async fn station_card<B: OnboardApi>(State(st): State<Arc<UiState<B>>>) -> Response {
    any_origin(reply(st.backend.station_card().await))
}

/// Annuaire des stations de l'UPlanet (public).
async fn stations<B: OnboardApi>(State(st): State<Arc<UiState<B>>>) -> Response {
    any_origin(reply(st.backend.stations().await))
}

/// Crée un MULTIPASS. Les secrets de la réponse ne sont jamais remis une seconde fois : `no-store`.
async fn register<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<RegisterBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"lang\": \"fr\" }"))) };
    if let Some(code) = body.setup_code {
        return reply(st.backend.register_captain(ip, code, body.email, body.lang).await);
    }
    if body.guest {
        return reply(st.backend.register_guest(ip, body.alias, body.lang).await);
    }
    reply(st.backend.register(ip, body.email, body.lang).await)
}

/// Rétablit un MULTIPASS depuis sa carte secrète. La réponse porte un nouveau PASS et un nouveau jeton, jamais redonnés : `no-store`.
async fn restore<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<RestoreBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"card\": \"astro-secret:v1|…\", \"lang\": \"fr\" }"))) };
    reply(st.backend.restore(ip, body.card, body.lang).await)
}

/// Secours par les gardiens depuis la carte imprimée (public). Réponse jamais mise en cache.
async fn restore_ask<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<RestoreAskBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"card\": \"astro-share:v1|…\", \"send\": true }"))) };
    reply(st.backend.restore_ask(body.card, body.send).await)
}

/// Ajoute au jeton d'une invitation son adresse `/i/<jeton>` et le QR de cette adresse.
fn with_invite_link(mut v: Value, origin: &str) -> Value {
    if let Some(token) = v.get("token").and_then(Value::as_str).map(str::to_owned) {
        let url = format!("{}/i/{token}", origin.trim_end_matches('/'));
        let svg = crate::qr::svg(&url);
        if let Some(o) = v.as_object_mut() {
            if let Some(svg) = svg {
                o.insert("qr_svg".into(), Value::String(svg));
            }
            o.insert("url".into(), Value::String(url));
        }
    }
    v
}

async fn invite_create<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<Value>) -> Response {
    let origin = origin_of(&st, &headers);
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.invite_create(&p, body).await.map(|v| with_invite_link(v, &origin))),
        Err(r) => r,
    }
}

async fn invites<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    let origin = origin_of(&st, &headers);
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.invites(&p).await.map(|mut v| {
            if let Some(list) = v.get_mut("invites").and_then(Value::as_array_mut) {
                for i in list.iter_mut() {
                    let url = i.get("token").and_then(Value::as_str).map(|t| format!("{}/i/{t}", origin.trim_end_matches('/')));
                    if let (Some(o), Some(url)) = (i.as_object_mut(), url) {
                        o.insert("url".into(), Value::String(url));
                    }
                }
            }
            v
        })),
        Err(r) => r,
    }
}

async fn invite_revoke<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(token): Path<String>) -> Response {
    reply(st.backend.invite_revoke(&p, token).await)
}

/// Public : le jeton du lien est la seule preuve. Jamais mis en cache ni indexé.
async fn invite_preview<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(token): Path<String>) -> Response {
    if nostr_scheme(&headers) {
        return reply(st.backend.invite_preview_signed(&headers, method.as_str(), &uri, token).await);
    }
    reply(st.backend.invite_preview(token).await)
}

async fn invite_thumb<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, Path((token, index)): Path<(String, usize)>) -> Response {
    match st.backend.invite_thumb(token, index).await {
        Ok(png) => private((StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], png).into_response()),
        Err(e) => reply(Err(e)),
    }
}

/// Corps maximal d'un appel de station à station (une liste de contacts signée).
const REMOTE_BODY_BYTES: usize = 256 * 1024;

/// Accepte l'invitation avec le compte connecté : l'identité vient de la session (Basic) ou d'une signature NIP-98, jamais
/// d'une adresse saisie. Une signature peut venir d'une autre station, au nom d'un compte qu'elle héberge.
async fn invite_accept<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(token): Path<String>, body: axum::body::Bytes) -> Response {
    if nostr_scheme(&headers) {
        if body.len() > REMOTE_BODY_BYTES {
            return reply(Err(ApiError::bad_request("corps trop volumineux")));
        }
        return reply(st.backend.invite_accept_nostr(&headers, method.as_str(), &uri, token, body.to_vec()).await);
    }
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.invite_accept(&p, token).await),
        Err(r) => r,
    }
}

/// Demande l'envoi des photos d'une invitation acceptée depuis une autre station (signature NIP-98 du destinataire).
async fn invite_deliver<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(token): Path<String>, body: axum::body::Bytes) -> Response {
    if !nostr_scheme(&headers) || body.len() > REMOTE_BODY_BYTES {
        return unauthorized();
    }
    reply(st.backend.invite_deliver(&headers, method.as_str(), &uri, token, body.to_vec()).await)
}

#[derive(Deserialize)]
struct RemoteAcceptBody {
    origin: String,
    token: String,
}

/// Accepte l'invitation d'une autre station avec le compte connecté ici.
async fn invite_accept_elsewhere<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<RemoteAcceptBody>) -> Response {
    reply(st.backend.invite_accept_elsewhere(&p, b.origin, b.token).await)
}

#[derive(Deserialize)]
struct ClaimBody {
    #[serde(default)]
    email: String,
    #[serde(default)]
    guest: bool,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default = "default_lang")]
    lang: String,
}

/// Crée le MULTIPASS du porteur du lien. Les secrets de la réponse ne sont jamais remis une seconde fois : `no-store`.
async fn invite_claim<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, Path(token): Path<String>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<ClaimBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"lang\": \"fr\" }"))) };
    let who = if body.guest { Applicant::Guest(body.alias) } else { Applicant::Email(body.email) };
    reply(st.backend.invite_claim(ip, token, who, body.lang).await)
}

/// `/.well-known/nostr/nip96.json` : annonce l'envoi d'images de la station (JPEG, PNG, WebP, jusqu'à 8 Mo, authentifié par
/// NIP-98) et sert de sonde aux clients. Les fichiers vont dans `/Public/uploads` du membre ; l'adresse de téléchargement est
/// celle que la réponse d'envoi donne (`url`), pas une adresse déductible du seul hachage.
async fn nip96_info<B: OnboardApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap) -> Response {
    let origin = st.backend.public_origin(&host_of(&headers));
    any_origin(private(
        Json(json!({
            "api_url": format!("{origin}/api/upload/image"), "download_url": origin, "supported_nips": [98],
            "content_types": ["image/jpeg", "image/png", "image/webp"],
            "plans": { "free": { "name": "MULTIPASS", "is_nip98_required": true, "max_byte_size": UPLOAD_MAX, "file_expiration": [0, 0], "media_transformations": { "image": ["resizing", "metadata_stripping"] } } },
            "_source": "astroportd",
        }))
        .into_response(),
    ))
}
