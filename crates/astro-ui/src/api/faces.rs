//! FaceCloud : visages, dossiers, attributions, partages de visages en attente.
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// FaceCloud : visages, dossiers, attributions, partages de visages en attente.
pub trait FacesApi: Auth {
    /// Visages reconnus et suggestions.
    fn faces(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Valide la suggestion « est-ce X ? ».
    fn face_accept(&self, who: &Principal, face_id: i64) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Nomme un visage.
    fn face_name(&self, who: &Principal, face_id: i64, name: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// La table de tri des visages : positions PCA, groupes, suggestions, personnes.
    fn faces_board(&self, _who: &Principal, _dir: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("table de tri non disponible sur cette station")) }
    }
    /// Vignettes carrées de visages (`size` pixels) : `{ thumbs: { id: data URL }, failed: [[id, raison]] }`.
    fn faces_thumbs(&self, _who: &Principal, _ids: Vec<i64>, _size: u32) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("vignettes de visages non disponibles sur cette station")) }
    }
    /// Range des visages dans une personne : la fiche choisie (`target.contact`, son UID vCard) fait foi, sinon le nom ;
    /// `target.new` crée une nouvelle personne même si une fiche porte déjà ce nom.
    fn faces_assign(&self, _who: &Principal, _ids: Vec<i64>, _target: AssignTarget) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("FaceCloud non activé")) }
    }
    /// Lots de photos en attente de confirmation d'envoi : `{ pending: [{ recipient_hex, name, count, sample_path }] }`.
    fn face_shares_pending(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("FaceCloud non activé")) }
    }
    /// Réponse au lot d'un ami : envoie (`accept`) ou oublie ; `{ sent, dropped }`.
    fn face_shares_confirm(&self, _who: &Principal, _recipient: String, _accept: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("FaceCloud non activé")) }
    }
    /// Dossiers du uCloud qui contiennent des visages : `[{ dir, faces, todo }]`, du plus récent au plus ancien par chemin.
    fn faces_folders(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("FaceCloud non activé")) }
    }
    /// Supprime **les photos** où apparaissent les visages `ids` (fichiers du uCloud, avec leurs visages) : irréversible.
    fn faces_delete_photos(&self, _who: &Principal, _ids: Vec<i64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("suppression non disponible sur cette station")) }
    }
    /// « Pas à reconnaître » : oublie ces visages (les photos restent).
    fn faces_forget(&self, _who: &Principal, _ids: Vec<i64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("FaceCloud non activé")) }
    }
}

pub(super) fn routes<B: FacesApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        .route("/api/v1/faces", get(faces::<B>))
        .route("/api/v1/faces/board", get(faces_board::<B>))
        .route("/api/v1/faces/folders", get(faces_folders::<B>))
        .route("/api/v1/faces/thumbs", post(faces_thumbs::<B>))
        .route("/api/v1/faces/assign", post(faces_assign::<B>))
        .route("/api/v1/faces/forget", post(faces_forget::<B>))
        .route("/api/v1/faces/delete-photos", post(faces_delete_photos::<B>))
        .route("/api/v1/faces/pending-shares", get(face_shares_pending::<B>))
        .route("/api/v1/faces/pending-shares/confirm", post(face_shares_confirm::<B>))
        .route("/api/v1/faces/{id}/accept", post(face_accept::<B>))
        .route("/api/v1/faces/{id}/name", post(face_name::<B>))
}

async fn faces<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.faces(&p).await)
}

async fn face_accept<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(id): Path<i64>) -> Response {
    reply(st.backend.face_accept(&p, id).await)
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn face_name<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(id): Path<i64>, Json(body): Json<NameBody>) -> Response {
    let name = body.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 80 {
        return reply(Err(ApiError::bad_request("nom de 1 à 80 caractères attendu")));
    }
    reply(st.backend.face_name(&p, id, name).await)
}

/// Identifiants de visages : 1 à `max`, strictement positifs, sans doublon.
#[allow(clippy::result_large_err)]
fn face_ids(ids: Vec<i64>, max: usize) -> Result<Vec<i64>, Response> {
    let mut seen = std::collections::HashSet::new();
    if ids.is_empty() || ids.len() > max || ids.iter().any(|i| *i <= 0) {
        return Err(reply(Err(ApiError::bad_request(format!("1 à {max} identifiants de visages positifs attendus")))));
    }
    Ok(ids.into_iter().filter(|i| seen.insert(*i)).collect())
}

async fn faces_folders<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.faces_folders(&p).await)
}

#[derive(serde::Deserialize)]
struct BoardQuery {
    /// Dossier du uCloud à regarder (`/Images/2024`) ; absent : tous les visages (les plus récents).
    dir: Option<String>,
}

async fn faces_board<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BoardQuery>) -> Response {
    reply(st.backend.faces_board(&p, q.dir.filter(|d| d.len() <= 1024)).await)
}

#[derive(serde::Deserialize)]
struct ThumbsBody {
    ids: Vec<i64>,
    #[serde(default = "default_thumb")]
    size: u32,
}

fn default_thumb() -> u32 {
    96
}

async fn faces_thumbs<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ThumbsBody>) -> Response {
    let ids = match face_ids(body.ids, 80) {
        Ok(i) => i,
        Err(r) => return r,
    };
    if !(32..=320).contains(&body.size) {
        return reply(Err(ApiError::bad_request("taille de vignette de 32 à 320 pixels attendue")));
    }
    reply(st.backend.faces_thumbs(&p, ids, body.size).await)
}

#[derive(serde::Deserialize)]
struct AssignBody {
    ids: Vec<i64>,
    name: String,
    #[serde(default)]
    pubkey: Option<String>,
    #[serde(default)]
    contact: Option<String>,
    #[serde(default)]
    new: bool,
}

async fn faces_assign<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AssignBody>) -> Response {
    let ids = match face_ids(body.ids, 2000) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let name = body.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return reply(Err(ApiError::bad_request("nom de 1 à 80 caractères attendu")));
    }
    if body.pubkey.as_deref().is_some_and(|k| k.len() != 64 || !k.bytes().all(|b| b.is_ascii_hexdigit())) {
        return reply(Err(ApiError::bad_request("clé Nostr de 64 caractères hexadécimaux attendue")));
    }
    let contact = body.contact.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty());
    if contact.as_deref().is_some_and(|c| c.len() > 200 || c.chars().any(char::is_control)) {
        return reply(Err(ApiError::bad_request("identifiant de fiche invalide")));
    }
    reply(st.backend.faces_assign(&p, ids, AssignTarget { name, pubkey: body.pubkey.map(|k| k.to_ascii_lowercase()), contact, new: body.new }).await)
}

#[derive(serde::Deserialize)]
struct ForgetBody {
    ids: Vec<i64>,
}

async fn faces_forget<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ForgetBody>) -> Response {
    let ids = match face_ids(body.ids, 2000) {
        Ok(i) => i,
        Err(r) => return r,
    };
    reply(st.backend.faces_forget(&p, ids).await)
}

async fn faces_delete_photos<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ForgetBody>) -> Response {
    let ids = match face_ids(body.ids, 20_000) {
        Ok(i) => i,
        Err(r) => return r,
    };
    reply(st.backend.faces_delete_photos(&p, ids).await)
}

async fn face_shares_pending<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.face_shares_pending(&p).await)
}

#[derive(serde::Deserialize)]
struct ConfirmShareBody {
    recipient_hex: String,
    accept: bool,
}

async fn face_shares_confirm<B: FacesApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ConfirmShareBody>) -> Response {
    let recipient = body.recipient_hex.to_ascii_lowercase();
    if recipient.len() != 64 || !recipient.bytes().all(|b| b.is_ascii_hexdigit()) {
        return reply(Err(ApiError::bad_request("clé publique de l'ami attendue (64 hexadécimaux)")));
    }
    reply(st.backend.face_shares_confirm(&p, recipient, body.accept).await)
}
