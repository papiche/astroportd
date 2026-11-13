//! Station (Capitaine) : cockpit, domaine, énergie, réglages, constellations (plans).
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// Station (Capitaine) : cockpit, domaine, énergie, réglages, constellations (plans).
pub trait StationApi: Auth {
    /// Tableau de bord et widget de souveraineté.
    fn dashboard(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Énergie de la station préparée par le plugin `energy` (Capitaine) : `{ state: absent|invalid|stale|fresh, … }`.
    fn energy(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Ok(json!({ "state": "absent" })) }
    }
    /// PAF de cette machine, sa valeur enregistrée, celle qui s'applique et une estimation (Capitaine).
    fn paf(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("PAF non disponible sur cette station")) }
    }
    /// Enregistre la PAF de cette machine, en centimes de Ẑ par semaine (`0` la retire) ; effective au redémarrage (Capitaine).
    fn paf_set(&self, _who: &Principal, _cents: u64) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("PAF non disponible sur cette station")) }
    }
    /// Nom de domaine de la station : actuel, enregistré, ports, redémarrage possible (Capitaine).
    fn domain_info(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("réglage du domaine non disponible sur cette station")) }
    }
    /// Vérifie (forme, DNS, ports, qui répond au nom) et enregistre le nom de domaine, servi par la station ou par un
    /// reverse proxy (`proxy` : `None` = détecter) ; effectif au redémarrage (Capitaine).
    fn domain_set(&self, _who: &Principal, _domain: String, _proxy: Option<bool>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("réglage du domaine non disponible sur cette station")) }
    }
    /// Redémarre la station (systemd la relance) pour appliquer la configuration enregistrée (Capitaine).
    fn restart(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("redémarrage non disponible sur cette station")) }
    }
    /// Plans (ORIGIN, UPlanet privées) configurés sur cette station et ceux que ce processus sert (Capitaine).
    fn planes(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("configuration des plans non disponible sur cette station")) }
    }
    /// Ajoute un plan : fonde une UPlanet (`swarm_key` absent) ou rejoint celle de la `swarm.key` donnée (Capitaine).
    fn plane_add(&self, _who: &Principal, _name: String, _swarm_key: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("configuration des plans non disponible sur cette station")) }
    }
    /// Retire un plan de la configuration, sans toucher à ses données (Capitaine).
    fn plane_remove(&self, _who: &Principal, _name: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("configuration des plans non disponible sur cette station")) }
    }
    /// Met l'unité systemd d'accord avec `planes.json` : elle lance `planes` au lieu de `serve` (Capitaine).
    fn plane_service(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("configuration des plans non disponible sur cette station")) }
    }
    /// Réglages d'une section d'administration (Capitaine) : `face`, `kinds`…
    fn admin_get(&self, _who: &Principal, _section: &str) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::not_found("section d'administration inconnue")) }
    }
    /// Modifie une section d'administration (Capitaine) ; rend la vue mise à jour.
    fn admin_set(&self, _who: &Principal, _section: &str, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::not_found("section d'administration inconnue")) }
    }
    /// État des sous-systèmes.
    fn health(&self) -> impl Future<Output = Vec<Subsystem>> + Send;
}

pub(super) fn routes<B: StationApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        .route("/health", get(health::<B>))
        .route("/api/v1/dashboard", get(dashboard::<B>))
        .route("/api/v1/station/admin/{section}", get(admin_get::<B>).post(admin_set::<B>))
        .route("/api/v1/station/energy", get(energy::<B>))
        .route("/api/v1/station/paf", get(paf::<B>).post(paf_set::<B>))
        .route("/api/v1/station/domain", get(domain_info::<B>).post(domain_set::<B>))
        .route("/api/v1/station/restart", post(restart::<B>))
        .route("/api/v1/station/planes", get(planes::<B>).post(plane_add::<B>))
        .route("/api/v1/station/planes/remove", post(plane_remove::<B>))
        .route("/api/v1/station/planes/service", post(plane_service::<B>))
}

async fn dashboard<B: StationApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.dashboard(&p).await)
}

/// Énergie de la station (plugin `energy`).
async fn energy<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.energy(&p).await)
}

/// PAF de la machine.
async fn paf<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.paf(&p).await)
}

async fn paf_set<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(b): Json<Value>) -> Response {
    let Some(cents) = b["paf_cents"].as_u64() else { return reply(Err(ApiError::bad_request("JSON attendu : { \"paf_cents\": 1450 } (centimes de Ẑ par semaine, 0 pour retirer)"))) };
    reply(st.backend.paf_set(&p, cents).await)
}

async fn domain_info<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.domain_info(&p).await)
}

async fn domain_set<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(b): Json<Value>) -> Response {
    let Some(domain) = b["domain"].as_str().map(str::to_owned) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"domain\": \"station.exemple.org\", \"proxy\": true|false|null }"))) };
    reply(st.backend.domain_set(&p, domain, b["proxy"].as_bool()).await)
}

async fn restart<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.restart(&p).await)
}

#[derive(Deserialize)]
struct PlaneBody {
    name: String,
    /// Contenu d'une `swarm.key` reçue ; absent : on fonde une UPlanet neuve.
    swarm_key: Option<String>,
}

async fn planes<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.planes(&p).await)
}

async fn plane_add<B: StationApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<PlaneBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if body.name.len() <= 64 && body.swarm_key.as_ref().is_none_or(|k| k.len() <= 1024) => reply(st.backend.plane_add(&p, body.name, body.swarm_key.filter(|k| !k.trim().is_empty())).await),
            Ok(()) => reply(Err(ApiError::bad_request("nom ou clé trop long"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn plane_service<B: StationApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) => reply(st.backend.plane_service(&p).await),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn plane_remove<B: StationApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<PlaneBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if body.name.len() <= 64 => reply(st.backend.plane_remove(&p, body.name).await),
            Ok(()) => reply(Err(ApiError::bad_request("nom trop long"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

fn valid_section(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

async fn admin_get<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Path(section): Path<String>) -> Response {
    if !valid_section(&section) {
        return reply(Err(ApiError::not_found("section d'administration inconnue")));
    }
    reply(st.backend.admin_get(&p, &section).await)
}

async fn admin_set<B: StationApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Path(section): Path<String>, Json(body): Json<Value>) -> Response {
    if !valid_section(&section) {
        return reply(Err(ApiError::not_found("section d'administration inconnue")));
    }
    reply(st.backend.admin_set(&p, &section, body).await)
}

async fn health<B: StationApi>(State(st): State<Arc<UiState<B>>>) -> Response {
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
