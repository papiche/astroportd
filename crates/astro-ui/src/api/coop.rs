//! Économie coopérative : vision de la constellation, portefeuilles, charte, Ğ1Billet, hébergement.
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// Économie coopérative : vision de la constellation, portefeuilles, charte, Ğ1Billet, hébergement.
pub trait CoopApi: Auth {
    /// Constellation : nœuds de l'essaim, Power-Score, distance, statut, services (Capitaine).
    fn constellation(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Régie coopérative : parts 3×1/3 de la dernière semaine, provision fiscale, Love Ledger (Capitaine).
    fn economy(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Carburant de la station : portefeuille maître à créditer pour ouvrir les portefeuilles coopératifs (Capitaine).
    /// `{ enabled: false }` sans transaction primale ; sinon `{ master, needed_g1_cents, spendable_g1_cents, waiting, ready, total, at }`.
    fn fuel(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Ok(json!({ "enabled": false })) }
    }
    /// Fixe le quota ET le prix hebdomadaire (ẑ) d'un compte hébergé (Capitaine) ; `None` retire le réglage propre au
    /// compte (retour au palier par défaut du Capitaine).
    fn account_hosting(&self, _who: &Principal, _hex: String, _quota_bytes: Option<u64>, _weekly_zen: Option<u64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("hébergement non réglable sur cette station")) }
    }
    /// Palier par défaut du Capitaine (quota et prix hebdomadaire des MULTIPASS sans réglage propre) : lecture.
    fn hosting_defaults(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("hébergement non réglable sur cette station")) }
    }
    /// Palier par défaut du Capitaine : écriture (`None` retire le réglage).
    fn set_hosting_defaults(&self, _who: &Principal, _quota_bytes: Option<u64>, _weekly_zen: Option<u64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("hébergement non réglable sur cette station")) }
    }
    /// Configuration coopérative de la constellation (Capitaine) : sections, valeurs en clair, secrets masqués.
    fn coop_config(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("configuration coopérative non disponible sur cette station")) }
    }
    /// Modifie la configuration coopérative : `{ clé: valeur | null }` (Capitaine).
    fn coop_config_set(&self, _who: &Principal, _patch: std::collections::BTreeMap<String, Option<String>>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("configuration coopérative non disponible sur cette station")) }
    }
    /// Vision de la constellation, pour tout membre : `None` = portefeuilles, soldes, nom, charte et état de l'historique ;
    /// `Some((depuis, au plus))` = historique des virements de la coopérative pour la carte animée.
    fn cooperative_view(&self, _who: &Principal, _ledger: Option<(u64, usize)>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("vision de la constellation non disponible sur cette station")) }
    }
    /// Le Capitaine signe la charte d'empreinte `sha256` (la version courante) : `{ version, sha256, at }`.
    fn charter_accept(&self, _who: &Principal, _sha256: &str) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("acceptation de la charte non disponible sur cette station")) }
    }
    /// Versement de Ẑen par le Capitaine : `op` = `recipients`, `preview` (devis) ou `send` (confirmé) ; réservé au Capitaine.
    fn zen_credit(&self, _who: &Principal, _op: &'static str, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("versement de Ẑen non disponible sur cette station")) }
    }
    /// Autorise (ou retire) un MULTIPASS hébergé à créer des Ğ1Billets (Capitaine).
    fn account_billet_issuer(&self, _who: &Principal, _hex: String, _allowed: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Ğ1Billets non disponibles sur cette station")) }
    }
    /// Crée des Ğ1Billets (émetteurs autorisés) : `{ count, amount_zen, valid_days?, return_to_issuer?, origin? }`.
    fn billet_issue(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Ğ1Billets non disponibles sur cette station")) }
    }
    /// Finance un billet créé par l'appelant : `{ g1pub }`.
    fn billet_fund(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Ğ1Billets non disponibles sur cette station")) }
    }
    /// Supprime un billet créé par l'appelant qui n'est pas encore financé (retiré de Nostr) : `{ g1pub }`.
    fn billet_delete(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Ğ1Billets non disponibles sur cette station")) }
    }
    /// Les billets créés par l'appelant, avec leur état.
    fn billet_issued(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Ğ1Billets non disponibles sur cette station")) }
    }
    /// Ğ1Billet (public : le billet est un titre au porteur) : `op` = `check` (adresse), `identify` (deux parts), `destination` ou
    /// `redeem` ; `ip` : limite de débit. L'erreur est rangée par nature : refus, trop de demandes, service incomplet…
    fn billet(&self, _ip: Option<std::net::IpAddr>, _op: &'static str, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Ğ1Billets non disponibles sur cette station")) }
    }
    /// Le compte est-il inscrit sur OpenCollective ? (`GET /api/v1/account/oc`) : seulement pour l'adresse du compte appelant.
    fn account_oc(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("vérification OpenCollective non activée sur cette station")) }
    }
    /// Solde d'une adresse Ğ1 (`GET /check_balance?g1pub=`, public) : `{ g1pub, balance, g1_cents, zen, zen_cents }`.
    fn check_balance(&self, _g1pub: &str) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("solde indisponible sur cette station")) }
    }
    /// Clé Nostr publique de l'UPlanet (`GET /UPLANET/HEX`, ou `/UPLANET/G1HEX` si `g1`), en hexadécimal.
    fn uplanet_hex(&self, _g1: bool) -> impl Future<Output = Result<String, ApiError>> + Send {
        async { Err(ApiError::unavailable("clé de l'UPlanet non disponible")) }
    }
}

pub(super) fn routes<B: CoopApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        // Compatibilité des clients historiques (zelkova, coracle) : mêmes noms qu'Astroport.ONE, sur le port unique.
        .route("/check_balance", get(check_balance::<B>))
        .route("/api/v1/account/oc", get(account_oc::<B>).options(cors_preflight))
        .route("/UPLANET/HEX", get(uplanet_hex::<B>))
        .route("/UPLANET/G1HEX", get(uplanet_g1hex::<B>))
        .route("/api/v1/station/coop", get(coop_get::<B>).post(coop_set::<B>))
        .route("/api/v1/constellation", get(constellation_get::<B>))
        .route("/api/v1/constellation/ledger", get(constellation_ledger::<B>))
        .route("/api/v1/constellation/charter/accept", post(charter_accept::<B>))
        .route("/api/v1/billet/check", get(billet_check::<B>))
        .route("/api/v1/billet/destination", get(billet_destination::<B>))
        .route("/api/v1/billet/identify", post(billet_identify::<B>))
        .route("/api/v1/billet/redeem", post(billet_redeem::<B>))
        .route("/api/v1/billet/issue", post(billet_issue::<B>))
        .route("/api/v1/billet/fund", post(billet_fund::<B>))
        .route("/api/v1/billet/delete", post(billet_delete::<B>))
        .route("/api/v1/billet/issued", get(billet_issued::<B>))
        .route("/api/v1/station/zen-credit/recipients", post(zen_credit_recipients::<B>))
        .route("/api/v1/station/zen-credit/preview", post(zen_credit_preview::<B>))
        .route("/api/v1/station/zen-credit/send", post(zen_credit_send::<B>))
        .route("/api/v1/me/zen/recipients", post(member_zen_recipients::<B>))
        .route("/api/v1/me/zen/preview", post(member_zen_preview::<B>))
        .route("/api/v1/me/zen/send", post(member_zen_send::<B>))
        .route("/api/v1/station/oc/donations", get(oc_donations::<B>))
        .route("/api/v1/station/oc/sync", post(oc_sync::<B>))
        .route("/api/v1/station/constellation", get(constellation::<B>))
        .route("/api/v1/station/economy", get(economy::<B>))
        .route("/api/v1/station/fuel", get(fuel::<B>))
        .route("/api/v1/station/accounts/{hex}/hosting", post(account_hosting::<B>))
        .route("/api/v1/station/hosting", get(hosting_defaults::<B>).post(set_hosting_defaults::<B>))
}

async fn constellation<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.constellation(&p).await)
}

async fn economy<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.economy(&p).await)
}

/// Carburant de la station, avec le QR de l'adresse à créditer (lu par Cesium, Ğecko…).
async fn fuel<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.fuel(&p).await.map(|mut v| {
        if let Some(svg) = v["master"].as_str().and_then(crate::qr::svg) {
            v["qr_svg"] = Value::String(svg);
        }
        v
    }))
}

#[derive(Deserialize)]
struct HostingBody {
    /// Octets ; absent ou `null` : pas de quota propre (palier par défaut).
    quota_bytes: Option<u64>,
    /// ẑ par semaine ; absent ou `null` : pas de prix propre (prix par défaut).
    weekly_zen: Option<u64>,
    /// Autorise (`true`) ou retire (`false`) la création de Ğ1Billets ; absent : inchangé.
    #[serde(default)]
    billet_issuer: Option<bool>,
}

async fn account_hosting<B: CoopApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>, Json(body): Json<HostingBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
                let hex = hex.to_ascii_lowercase();
                // Une seule action du Capitaine règle l'hébergement et le droit de créer des Ğ1Billets.
                if let Some(allowed) = body.billet_issuer {
                    if let Err(e) = st.backend.account_billet_issuer(&p, hex.clone(), allowed).await {
                        return reply(Err(e));
                    }
                }
                reply(st.backend.account_hosting(&p, hex, body.quota_bytes, body.weekly_zen).await)
            }
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn hosting_defaults<B: CoopApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) => reply(st.backend.hosting_defaults(&p).await),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn set_hosting_defaults<B: CoopApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<HostingBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) => reply(st.backend.set_hosting_defaults(&p, body.quota_bytes, body.weekly_zen).await),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn coop_get<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.coop_config(&p).await)
}

async fn constellation_get<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.cooperative_view(&p, None).await)
}

#[derive(serde::Deserialize)]
struct LedgerQuery {
    since: Option<u64>,
    limit: Option<usize>,
}

async fn constellation_ledger<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<LedgerQuery>) -> Response {
    reply(st.backend.cooperative_view(&p, Some((q.since.unwrap_or(0), q.limit.unwrap_or(5000).clamp(1, 20_000)))).await)
}

#[derive(serde::Deserialize)]
struct BilletQuery {
    g1pub: String,
}

/// Lectures publiques d'un billet (valeur, origine, validité) et d'un destinataire. Jamais mises en cache.
async fn billet_check<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Query(q): Query<BilletQuery>, req: axum::extract::Request) -> Response {
    no_store(reply(st.backend.billet(client_ip(&req), "check", serde_json::json!({ "g1pub": q.g1pub })).await))
}

async fn billet_destination<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Query(q): Query<BilletQuery>, req: axum::extract::Request) -> Response {
    no_store(reply(st.backend.billet(client_ip(&req), "destination", serde_json::json!({ "g1pub": q.g1pub })).await))
}

/// Deux parts : reconnaître le billet sans y toucher.
async fn billet_identify<B: CoopApi>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    billet_post(st, "identify", req).await
}

/// Encaisse : deux parts et un destinataire. Les parts ne transitent qu'ici, en HTTPS, et ne sont jamais journalisées.
async fn billet_redeem<B: CoopApi>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    billet_post(st, "redeem", req).await
}

/// Création de billets : réservée aux émetteurs que le Capitaine a choisis. Les parts ne sont rendues qu'ici, une fois : `no-store`.
async fn billet_issue<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.billet_issue(&p, body).await))
}

async fn billet_delete<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.billet_delete(&p, body).await))
}

async fn billet_fund<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.billet_fund(&p, body).await))
}

async fn zen_credit_recipients<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    no_store(reply(st.backend.zen_credit(&p, "recipients", Value::Null).await))
}

async fn zen_credit_preview<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.zen_credit(&p, "preview", body).await))
}

async fn zen_credit_send<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.zen_credit(&p, "send", body).await))
}

async fn member_zen_recipients<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    no_store(reply(st.backend.zen_credit(&p, "member_recipients", Value::Null).await))
}

async fn member_zen_preview<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.zen_credit(&p, "member_preview", body).await))
}

async fn member_zen_send<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.zen_credit(&p, "member_send", body).await))
}

async fn oc_donations<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    no_store(reply(st.backend.zen_credit(&p, "oc_status", Value::Null).await))
}

async fn oc_sync<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.zen_credit(&p, "oc_sync", body).await))
}

async fn billet_issued<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    no_store(reply(st.backend.billet_issued(&p).await))
}

async fn billet_post<B: CoopApi>(st: Arc<UiState<B>>, op: &'static str, req: axum::extract::Request) -> Response {
    let ip = client_ip(&req);
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 1024).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<Value>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu"))) };
    no_store(reply(st.backend.billet(ip, op, body).await))
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut().insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    r
}

#[derive(serde::Deserialize)]
struct CharterBody {
    sha256: String,
}

async fn charter_accept<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(body): Json<CharterBody>) -> Response {
    reply(st.backend.charter_accept(&p, &body.sha256).await)
}

#[derive(serde::Deserialize)]
struct CoopBody {
    values: std::collections::BTreeMap<String, Option<String>>,
}

async fn coop_set<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(body): Json<CoopBody>) -> Response {
    if body.values.is_empty() || body.values.len() > 64 || body.values.keys().any(|k| k.len() > 64 || !k.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')) {
        return reply(Err(ApiError::bad_request("1 à 64 clés en majuscules attendues")));
    }
    reply(st.backend.coop_config_set(&p, body.values).await)
}

async fn account_oc<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    any_origin(reply(st.backend.account_oc(&p).await))
}

#[derive(Deserialize)]
struct BalanceQuery {
    g1pub: String,
}

/// Solde d'une adresse Ğ1, au format de l'ancien UPassport. Une adresse seulement : jamais d'e-mail (ce serait un moyen de
/// retrouver le portefeuille d'une personne à partir de son adresse).
async fn check_balance<B: CoopApi>(State(st): State<Arc<UiState<B>>>, Query(q): Query<BalanceQuery>) -> Response {
    if q.g1pub.contains('@') || q.g1pub.len() > 64 {
        return any_origin(reply(Err(ApiError::bad_request("une adresse Ğ1 est attendue (pas d'adresse e-mail)"))));
    }
    any_origin(reply(st.backend.check_balance(&q.g1pub).await))
}

async fn uplanet_hex_text<B: CoopApi>(st: &UiState<B>, g1: bool) -> Response {
    any_origin(match st.backend.uplanet_hex(g1).await {
        Ok(hex) => private(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], hex).into_response()),
        Err(e) => reply(Err(e)),
    })
}

async fn uplanet_hex<B: CoopApi>(State(st): State<Arc<UiState<B>>>) -> Response {
    uplanet_hex_text(&st, false).await
}

async fn uplanet_g1hex<B: CoopApi>(State(st): State<Arc<UiState<B>>>) -> Response {
    uplanet_hex_text(&st, true).await
}
