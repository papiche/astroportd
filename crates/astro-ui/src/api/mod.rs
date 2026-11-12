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
//! | `GET /api/v1/faces/pending-shares` · `POST …/confirm` | envois de photos en attente d'un clic |
//! | `GET /api/v1/faces/board` · `POST …/thumbs` · `…/assign` · `…/forget` | table de tri : positions, groupes, vignettes, rangement |
//! | `GET` · `POST /api/v1/station/coop` | configuration coopérative (Capitaine) : kind 30800, secrets en écriture seule |
//! | `GET` · `POST /api/v1/station/admin/{section}` | réglages du Capitaine par section (`face` : moteur FaceID ; `kinds` : matrice du relai) |
//! | `GET /api/v1/account` · `POST …/profile` · `…/token` · `…/keys` | Mon compte : identité, profil, nouveau jeton WebDAV, export des clés |
//! | `GET /api/v1/contacts` · `POST …/sync` · `…/from-person` · `…/link` | annuaire CardDAV : fiches, personnes sans fiche, rapprochement visage ↔ fiche ↔ MULTIPASS |
//! | `GET /api/v1/friends` · `POST /api/v1/ucloud/send` | proches réciproques ; envoi de fichiers à des proches |
//! | `GET`/`POST /api/v1/backup` · `POST /api/v1/backup/respond` | sauvegarde privée chez un ami : réglage, demandes reçues, réponse de l'hébergeur |
//! | `POST /api/v1/backup/rescue` · `POST /api/v1/restore/ask` | secours par les gardiens : réponse du gardien ; demande depuis la carte imprimée (public) |
//! | `GET /api/v1/station/energy` | énergie de la station préparée par le plugin système `energy` (consommation, production solaire), Capitaine |
//! | `GET /api/v1/stations` | annuaire public des stations de la même UPlanet (balises 30850 : places, position UMAP, matériel), lisible depuis toute origine |
//! | `POST /api/v1/ucloud/have` | parmi des SHA-256, ceux que le compte possède déjà (envoi sans doublon) |
//! | `GET /api/v1/ucloud/memories` · `…/timeline[?ym=]` · `…/search` · `…/duplicates` | « ce jour-là », chronologie par mois, recherche à filtres, doublons exacts et visuels |
//! | `GET /api/v1/replication` | où et par qui les données du compte sont répliquées (stations, amis, fichiers) |
//! | `POST /api/v1/bro/say` | dialogue avec BRO |
//! | `GET /api/v1/station/console?ticket=` | console (WebSocket) ouverte par un ticket à usage unique que le Capitaine a obtenu par `admin/assist` |
//!
//! L'authentification est portée par l'en-tête `Authorization` (Basic ou NIP-98), jamais par un cookie : l'API
//! n'est donc pas exposée aux requêtes forgées depuis un autre site.

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astro_core::ActivityBus;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
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

/// Question posée à la bibliothèque du compte.
#[derive(Debug, Clone)]
pub enum LibraryQuery {
    /// Parmi ces SHA-256, lesquels le compte possède-t-il ?
    Have(Vec<String>),
    /// « Ce jour-là » : les années précédentes, pour le jour `(MM-JJ, année)` du navigateur (son fuseau, pas celui de la station).
    Memories(Option<(String, i64)>),
    /// Compteurs par mois, ou les éléments d'un mois (`2026-10`).
    Timeline(Option<String>),
    /// Recherche à filtres combinés.
    Search(LibrarySearch),
    /// Carte : regroupements géolocalisés d'une période et d'une zone, ou éléments d'une zone.
    Map(MapQuery),
    /// Doublons exacts et visuels ; `true` : exacts de tous les types de fichiers (sinon photos et vidéos).
    Duplicates(bool),
}

/// Opération du carnet d'adresses.
#[derive(Debug, Clone)]
pub enum BookOp {
    /// Fiches filtrées par texte et par groupe.
    List(String, Option<String>),
    /// Une fiche en détail.
    Get(String),
    /// Crée (`None`) ou modifie une fiche : champs en JSON.
    Save(Option<String>, Value),
    /// Supprime une fiche.
    Delete(String),
    /// Supprime plusieurs fiches.
    DeleteMany(Vec<String>),
    /// Photo incorporée d'une fiche.
    Photo(String),
    /// Visages reconnus proposés comme photo d'une fiche.
    PhotoChoices(String),
    /// Change (`Some(data:…)`) ou retire (`None`) la photo d'une fiche.
    SetPhoto(String, Option<String>),
    /// Prend des valeurs du profil Nostr dans la fiche.
    Take(String, Vec<String>),
    /// Renomme ou retire un groupe.
    Group(String, Option<String>),
    /// Importe un fichier vCard.
    Import(String),
    /// Doublons probables.
    Duplicates,
    /// Fusionne des fiches dans une fiche gardée.
    Merge(String, Vec<String>),
    /// Doublons exacts : aperçu (`false`) ou fusion d'un lot de `usize` groupes au plus (`true`).
    MergeExact(bool, usize),
    /// « Ma fiche » : lecture.
    Me,
    /// « Ma fiche » : enregistrement (publie le profil Nostr et écrit la fiche).
    SaveMe(Value),
}

/// Opération du chat privé 1 à 1.
#[derive(Debug, Clone)]
pub enum ChatOp {
    /// Conversations et nombre de messages non lus.
    List,
    /// Messages avec un correspondant, avant un horodatage éventuel.
    Messages(String, Option<i64>),
    /// Envoie un texte à un correspondant.
    Send(String, String),
    /// Marque la conversation comme lue.
    Read(String),
    /// Signal d'appel visio `{ peer, type, call, sdp?, video?, missed? }`.
    Signal(Value),
}

/// Demande de carte telle que l'URL la porte.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MapQuery {
    /// Début de période (secondes Unix).
    pub from: Option<i64>,
    /// Fin de période (exclue).
    pub to: Option<i64>,
    /// `sud,ouest,nord,est` en degrés.
    pub bbox: Option<String>,
    /// Côté d'une case de regroupement, en degrés.
    pub cell: Option<f64>,
    /// `1` : les éléments de la zone au lieu des regroupements.
    pub items: Option<u8>,
}

/// Filtres de recherche tels que l'URL les porte.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LibrarySearch {
    /// Mots cherchés dans le nom et la description.
    pub q: Option<String>,
    /// Personne reconnue sur la photo.
    pub person: Option<String>,
    /// Début de période (secondes Unix, inclus).
    pub from: Option<i64>,
    /// Fin de période (exclue).
    pub to: Option<i64>,
    /// Zone `sud,ouest,nord,est`.
    pub bbox: Option<String>,
    /// `image`, `video` ou `other`.
    pub kind: Option<String>,
    /// Curseur : date du dernier résultat reçu.
    pub after_t: Option<i64>,
    /// Curseur : chemin du dernier résultat reçu.
    pub after_p: Option<String>,
    /// Résultats par page.
    pub limit: Option<usize>,
}

/// Ce que le navigateur envoie à une console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleIn {
    /// Frappes à transmettre telles quelles.
    Data(Vec<u8>),
    /// Nouvelle taille du terminal `(colonnes, lignes)`.
    Resize(u16, u16),
}

/// Les deux sens d'une console ouverte par la station : la fermer côté navigateur (abandon de `input`) la termine.
pub struct ConsoleLink {
    /// Vers le processus.
    pub input: tokio::sync::mpsc::Sender<ConsoleIn>,
    /// Depuis le processus ; fermé quand il se termine.
    pub output: tokio::sync::mpsc::Receiver<Vec<u8>>,
}

/// Qui réclame une invitation : une adresse email, ou un invité (adresse générée par la station, `alias` facultatif).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applicant {
    /// Adresse fournie.
    Email(String),
    /// Sans email : prénom ou pseudo facultatif.
    Guest(Option<String>),
}

/// Erreur d'API : statut HTTP et message pour l'humain.
#[derive(Debug)]
pub struct ApiError {
    /// Statut HTTP.
    pub status: StatusCode,
    /// Message.
    pub message: String,
    /// Code stable (`invite_expired`…) : l'interface traduit d'après lui, sans analyser le message.
    pub code: Option<&'static str>,
}

impl ApiError {
    /// Ajoute le code stable de l'erreur.
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    /// 400.
    pub fn bad_request(m: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: m.into(), code: None }
    }

    /// 404.
    pub fn not_found(m: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: m.into(), code: None }
    }

    /// 401 : identifiants refusés (code PASS faux).
    pub fn denied(m: impl Into<String>) -> Self {
        Self { status: StatusCode::UNAUTHORIZED, message: m.into(), code: None }
    }

    /// 403 : réservé à un autre rôle (le Capitaine).
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self { status: StatusCode::FORBIDDEN, message: m.into(), code: None }
    }

    /// 409 : la ressource existe déjà (email déjà inscrit).
    pub fn conflict(m: impl Into<String>) -> Self {
        Self { status: StatusCode::CONFLICT, message: m.into(), code: None }
    }

    /// 410 : la ressource a existé (lien déjà utilisé, retiré ou expiré).
    pub fn gone(m: impl Into<String>) -> Self {
        Self { status: StatusCode::GONE, message: m.into(), code: None }
    }

    /// 429 : trop de demandes.
    pub fn too_many(m: impl Into<String>) -> Self {
        Self { status: StatusCode::TOO_MANY_REQUESTS, message: m.into(), code: None }
    }

    /// 501 : fonction non activée sur cette station.
    pub fn unavailable(m: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_IMPLEMENTED, message: m.into(), code: None }
    }

    /// 500 (le détail technique reste dans les journaux, pas dans la réponse).
    pub fn internal(m: impl Into<String>) -> Self {
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: m.into(), code: None }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.code {
            Some(code) => json!({ "error": self.message, "code": code }),
            None => json!({ "error": self.message }),
        };
        (self.status, Json(body)).into_response()
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

/// Socle de tous les domaines : identifie l'appelant et dit comment la station est jointe (origine publique).
pub trait Auth: Send + Sync + 'static {
    /// Identifie l'appelant (Basic ou NIP-98) ; `None` : non authentifié.
    fn authenticate(&self, headers: &HeaderMap, method: &str, uri: &Uri) -> impl Future<Output = Option<Principal>> + Send;
    /// Adresse publique de la station telle que les clients la voient : `https://<domaine>` si elle en a un, sinon
    /// `http://<hôte demandé>` (essai local).
    fn public_origin(&self, host: &str) -> String {
        format!("http://{host}")
    }
}

/// Ce que l'interface demande à la station. Implémenté par le binaire, qui câble base, WebDAV, FaceCloud, BRO,
/// Duniter et P2P ; l'interface elle-même ne connaît aucun de ces sous-systèmes.
///
/// Il se compose d'un trait par domaine (UcloudApi, FacesApi, CoopApi, SocialApi, AccountApi, OnboardApi, StationApi) sur le socle [`Auth`] ; chaque module de
/// `api/` ne demande que le sien. Tout type qui les implémente tous est un [`Backend`] (implémentation générale).
pub trait Backend: Auth + UcloudApi + FacesApi + CoopApi + SocialApi + AccountApi + OnboardApi + StationApi {}

impl<T: Auth + UcloudApi + FacesApi + CoopApi + SocialApi + AccountApi + OnboardApi + StationApi> Backend for T {}


/// État partagé des routes.
pub struct UiState<B> {
    backend: Arc<B>,
    activity: ActivityBus,
    started: Instant,
    /// Origine publique (`https://domaine`) ; sinon déduite de l'en-tête `Host`.
    public_origin: Option<String>,
}


pub use ucloud::UcloudApi;
pub use faces::FacesApi;
pub use coop::CoopApi;
pub use social::SocialApi;
pub use account::AccountApi;
pub use onboarding::OnboardApi;
pub use station::StationApi;

mod ucloud;
mod faces;
mod coop;
mod social;
mod account;
mod onboarding;
mod station;

/// Ce que partagent tous les domaines : le flux d'activité, l'heure de démarrage et l'origine publique.
#[derive(Clone)]
struct Shared {
    activity: ActivityBus,
    started: Instant,
    public_origin: Option<String>,
}

impl Shared {
    fn state<U>(&self, backend: Arc<U>) -> Arc<UiState<U>> {
        Arc::new(UiState { backend, activity: self.activity.clone(), started: self.started, public_origin: self.public_origin.clone() })
    }
}

/// Le routeur de l'API, monté domaine par domaine : chaque domaine reçoit **son** implémentation et ne connaît que son trait.
/// Un domaine non monté n'a pas de routes (404) ; la station les monte tous (voir `StationUi::into_router`).
pub struct Routes {
    router: Router,
    shared: Shared,
}

impl Routes {
    /// Un routeur sans domaine.
    pub fn new(activity: ActivityBus, public_origin: Option<String>) -> Self {
        Self { router: Router::new(), shared: Shared { activity, started: Instant::now(), public_origin } }
    }

    /// uCloud.
    #[must_use]
    pub fn ucloud<U: UcloudApi>(mut self, backend: Arc<U>) -> Self {
        self.router = self.router.merge(ucloud::routes::<U>().with_state(self.shared.state(backend)));
        self
    }

    /// FaceCloud.
    #[must_use]
    pub fn faces<U: FacesApi>(mut self, backend: Arc<U>) -> Self {
        self.router = self.router.merge(faces::routes::<U>().with_state(self.shared.state(backend)));
        self
    }

    /// Économie coopérative.
    #[must_use]
    pub fn coop<U: CoopApi>(mut self, backend: Arc<U>) -> Self {
        self.router = self.router.merge(coop::routes::<U>().with_state(self.shared.state(backend)));
        self
    }

    /// Social.
    #[must_use]
    pub fn social<U: SocialApi>(mut self, backend: Arc<U>) -> Self {
        self.router = self.router.merge(social::routes::<U>().with_state(self.shared.state(backend)));
        self
    }

    /// Compte.
    #[must_use]
    pub fn account<U: AccountApi>(mut self, backend: Arc<U>) -> Self {
        self.router = self.router.merge(account::routes::<U>().with_state(self.shared.state(backend)));
        self
    }

    /// Accueil et inscription.
    #[must_use]
    pub fn onboarding<U: OnboardApi>(mut self, backend: Arc<U>) -> Self {
        self.router = self.router.merge(onboarding::routes::<U>().with_state(self.shared.state(backend)));
        self
    }

    /// Station (Capitaine).
    #[must_use]
    pub fn station<U: StationApi>(mut self, backend: Arc<U>) -> Self {
        self.router = self.router.merge(station::routes::<U>().with_state(self.shared.state(backend)));
        self
    }

    /// Le routeur obtenu.
    pub fn build(self) -> Router {
        self.router
    }
}

/// Routeur de l'API et de `/health` pour **un seul** objet qui implémente tous les domaines (les tests, un `Fake`).
pub fn router<B: Backend>(backend: Arc<B>, activity: ActivityBus, public_origin: Option<String>) -> Router {
    Routes::new(activity, public_origin)
        .ucloud(backend.clone())
        .faces(backend.clone())
        .coop(backend.clone())
        .social(backend.clone())
        .account(backend.clone())
        .onboarding(backend.clone())
        .station(backend)
        .build()
}

/// 401 : seul le schéma `Nostr` est annoncé. Annoncer `Basic` ferait ouvrir au navigateur sa propre fenêtre de
/// connexion à chaque appel `fetch` ; l'application gère elle-même la saisie (Basic reste accepté si fourni).
fn unauthorized() -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, Json(json!({ "error": "authentification requise" }))).into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Nostr realm=\"astroportd\""));
    r
}

async fn who<B: Auth>(st: &UiState<B>, headers: &HeaderMap, method: &Method, uri: &Uri) -> Result<Principal, Response> {
    st.backend.authenticate(headers, method.as_str(), uri).await.ok_or_else(unauthorized)
}

/// Le compte qui appelle : l'authentification (Basic ou NIP-98) est faite avant que la route ne lise sa requête ou son corps.
struct Authed(Principal);

impl<B: Auth> FromRequestParts<Arc<UiState<B>>> for Authed {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &Arc<UiState<B>>) -> Result<Self, Response> {
        st.backend.authenticate(&parts.headers, parts.method.as_str(), &parts.uri).await.map(Self).ok_or_else(unauthorized)
    }
}

/// Le Capitaine de la station : 403 pour tout autre compte, même authentifié.
struct Captain(Principal);

impl<B: Auth> FromRequestParts<Arc<UiState<B>>> for Captain {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &Arc<UiState<B>>) -> Result<Self, Response> {
        let Authed(p) = Authed::from_request_parts(parts, st).await?;
        captain_only(&p).map(|()| Self(p))
    }
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

/// Où ranger des visages : une fiche choisie, ou un nom (et la clé Nostr de la personne, si elle est connue).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignTarget {
    /// Nom tapé (ou celui de la fiche choisie).
    pub name: String,
    /// Clé Nostr (64 hex, minuscules).
    pub pubkey: Option<String>,
    /// UID vCard de la fiche choisie : il fait foi.
    pub contact: Option<String>,
    /// Nouvelle personne, même si une fiche porte déjà ce nom.
    pub new: bool,
}

/// Les écrans d'administration sont réservés au Capitaine : 403 pour tout autre compte, même authentifié.
#[allow(clippy::result_large_err)]
fn captain_only(p: &Principal) -> Result<(), Response> {
    if p.captain { Ok(()) } else { Err(reply(Err(ApiError::forbidden("réservé au Capitaine de la station")))) }
}

#[derive(Deserialize)]
struct AgentIdBody {
    id: String,
}

fn client_ip(req: &axum::extract::Request) -> Option<std::net::IpAddr> {
    req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip())
}

/// Octets au plus d'une image envoyée (avant réduction).
const UPLOAD_MAX: usize = 8 * 1024 * 1024;

/// Hôte demandé, tel que le client l'a écrit (sans schéma).
fn host_of(headers: &HeaderMap) -> String {
    headers.get(header::HOST).and_then(|v| v.to_str().ok()).filter(|h| !h.is_empty() && h.len() <= 255 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.:[]".contains(&b))).unwrap_or("localhost").to_owned()
}

/// Réponse à une requête de pré-vérification CORS d'une route publique **sans cookie** (l'authentification est portée par
/// l'en-tête `Authorization`) : `*` ne donne alors accès à rien qu'un client sans le jeton ne pourrait déjà obtenir.
async fn cors_preflight() -> Response {
    let mut r = StatusCode::NO_CONTENT.into_response();
    let h = r.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, OPTIONS"));
    h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Authorization, Content-Type"));
    h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    r
}

/// Octets chiffrés d'une pièce jointe au plus (aligné sur `astro_dav::share::ATTACHMENT_MAX`, avec une marge pour l'en-tête).
const ATTACHMENT_BODY_MAX: usize = 20 * 1024 * 1024 + 1024;

/// Données publiques lisibles depuis une autre origine (vue LIVE d'astroport.one, autre station) : places libres et
/// annuaire. Lecture seule, sans cookie ni secret : `Access-Control-Allow-Origin: *` ne donne accès à rien de privé.
fn any_origin(mut r: Response) -> Response {
    r.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    r
}

/// Origine publique du lien : celle déclarée par la station, sinon l'en-tête `Host`.
fn origin_of<B>(st: &UiState<B>, headers: &HeaderMap) -> String {
    st.public_origin.clone().or_else(|| headers.get(header::HOST).and_then(|h| h.to_str().ok()).map(|h| format!("http://{h}"))).unwrap_or_default()
}

fn nostr_scheme(headers: &HeaderMap) -> bool {
    headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.split_whitespace().next()).is_some_and(|s| s.eq_ignore_ascii_case("nostr"))
}

fn default_lang() -> String {
    "fr".into()
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

    impl Auth for Fake {
        async fn authenticate(&self, headers: &HeaderMap, _: &str, _: &Uri) -> Option<Principal> {
            let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
            matches!(auth, "Basic ok" | "Basic cap").then(|| Principal { hex: ALICE.into(), email: Some("alice@example.org".into()), captain: auth == "Basic cap" })
        }
    }

    impl UcloudApi for Fake {
        async fn ucloud_list(&self, _: &Principal, path: String) -> Result<Value, ApiError> {
            Ok(json!({ "path": path, "entries": [] }))
        }
        async fn ucloud_photos(&self, _: &Principal, before: Option<i64>, limit: usize) -> Result<Value, ApiError> {
            Ok(json!({ "before": before, "limit": limit }))
        }
        async fn ucloud_share(&self, _: &Principal, paths: Vec<String>, ttl: i64, origin: String) -> Result<Value, ApiError> {
            Ok(json!({ "url": format!("{origin}/s/abc#{}", "cd".repeat(32)), "items": paths.len(), "expires_at": ttl }))
        }
    }

    impl FacesApi for Fake {
        async fn faces(&self, _: &Principal) -> Result<Value, ApiError> {
            Err(ApiError::unavailable("FaceCloud non activé"))
        }
        async fn face_accept(&self, _: &Principal, id: i64) -> Result<Value, ApiError> {
            Ok(json!({ "accepted": id }))
        }
        async fn face_name(&self, _: &Principal, id: i64, name: String) -> Result<Value, ApiError> {
            Ok(json!({ "id": id, "name": name }))
        }
    }

    impl CoopApi for Fake {
        async fn constellation(&self, _: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "nodes": [] }))
        }
        async fn economy(&self, _: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "week": "2026-W43" }))
        }
    }

    impl SocialApi for Fake {
        async fn bro_say(&self, _: &Principal, text: String) -> Result<Value, ApiError> {
            Ok(json!({ "reply": format!("écho : {text}") }))
        }
    }

    impl AccountApi for Fake {
        async fn footprint(&self, p: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "who": p.hex }))
        }
        async fn accounts(&self, _: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "accounts": [] }))
        }
        async fn account_token(&self, _: &Principal, hex: String) -> Result<Value, ApiError> {
            Ok(json!({ "for": hex }))
        }
    }

    impl OnboardApi for Fake {}

    impl StationApi for Fake {
        async fn dashboard(&self, p: &Principal) -> Result<Value, ApiError> {
            Ok(json!({ "who": p.hex }))
        }
        async fn console_open(&self, ticket: &str) -> Option<ConsoleLink> {
            // Console de test : renvoie en majuscules ce qu'elle reçoit ; « bye » la termine ; une taille est rapportée.
            (ticket == "good").then(|| {
                let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<ConsoleIn>(8);
                let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
                tokio::spawn(async move {
                    while let Some(m) = in_rx.recv().await {
                        match m {
                            ConsoleIn::Data(d) if d == b"bye" => break,
                            ConsoleIn::Data(d) => drop(out_tx.send(d.to_ascii_uppercase()).await),
                            ConsoleIn::Resize(c, r) => drop(out_tx.send(format!("{c}x{r}").into_bytes()).await),
                        }
                    }
                });
                ConsoleLink { input: in_tx, output: out_rx }
            })
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
        for (method, uri) in [(Method::GET, "/api/v1/friends"), (Method::GET, "/api/v1/backup"), (Method::GET, "/api/v1/replication"), (Method::GET, "/api/v1/me"), (Method::GET, "/api/v1/dashboard"), (Method::GET, "/api/v1/ucloud/list"), (Method::GET, "/api/v1/ucloud/photos"), (Method::GET, "/api/v1/faces"), (Method::GET, "/api/v1/activity")] {
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
    async fn the_console_socket_needs_a_valid_ticket_and_the_same_origin() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as M};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = app(&ActivityBus::new());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let connect = |ticket: &str, origin: Option<String>| {
            let mut req = format!("ws://{addr}/api/v1/station/console?ticket={ticket}").into_client_request().unwrap();
            if let Some(o) = origin {
                req.headers_mut().insert("origin", o.parse().unwrap());
            }
            tokio_tungstenite::connect_async(req)
        };
        assert!(connect("good", None).await.is_err(), "sans Origin : refusé");
        assert!(connect("good", Some("http://evil.example".into())).await.is_err(), "autre origine : refusé");
        assert!(connect("bad", Some(format!("http://{addr}"))).await.is_err(), "ticket inconnu : refusé");
        let (mut ws, _) = connect("good", Some(format!("http://{addr}"))).await.unwrap();
        ws.send(M::Binary(b"salut".to_vec().into())).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap().into_data().as_ref(), b"SALUT");
        ws.send(M::Text(r#"{"resize":[120,40]}"#.into())).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap().into_data().as_ref(), b"120x40");
        ws.send(M::Text(r#"{"resize":[0,9999]}"#.into())).await.unwrap();
        ws.send(M::Binary(b"bye".to_vec().into())).await.unwrap();
        assert!(matches!(ws.next().await, Some(Ok(M::Close(_))) | None), "la fin du processus ferme la socket");
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
        impl Auth for Broken {
            async fn authenticate(&self, _: &HeaderMap, _: &str, _: &Uri) -> Option<Principal> {
                None
            }
        }

        impl UcloudApi for Broken {
            async fn ucloud_list(&self, _: &Principal, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn ucloud_photos(&self, _: &Principal, _: Option<i64>, _: usize) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn ucloud_share(&self, _: &Principal, _: Vec<String>, _: i64, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
        }

        impl FacesApi for Broken {
            async fn faces(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn face_accept(&self, _: &Principal, _: i64) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn face_name(&self, _: &Principal, _: i64, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
        }

        impl CoopApi for Broken {
            async fn constellation(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn economy(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
        }

        impl SocialApi for Broken {
            async fn bro_say(&self, _: &Principal, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
        }

        impl AccountApi for Broken {
            async fn footprint(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn accounts(&self, _: &Principal) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
            async fn account_token(&self, _: &Principal, _: String) -> Result<Value, ApiError> {
                Ok(Value::Null)
            }
        }

        impl OnboardApi for Broken {}

        impl StationApi for Broken {
            async fn dashboard(&self, _: &Principal) -> Result<Value, ApiError> {
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
    async fn personal_events_reach_only_their_audience_even_for_the_captain() {
        use astro_core::Audience;
        let bus = ActivityBus::new();
        bus.publish_to(Audience::Owner("bb".into()), "economy", "t", "virement de bob", None);
        bus.publish_to(Audience::Captain, "system", "t", "email de nouveau membre", None);
        bus.publish_to(Audience::Owner(ALICE.into()), "economy", "t", "virement d'alice", None);
        bus.publish("system", "t", "pour tous", None);
        for (auth, expected_in, expected_out) in [("Basic ok", ["virement d'alice", "pour tous"], ["virement de bob", "email de nouveau membre"]), ("Basic cap", ["virement d'alice", "email de nouveau membre"], ["virement de bob", ""])] {
            let req = Request::builder().uri("/api/v1/activity").header(header::AUTHORIZATION, auth).body(Body::empty()).unwrap();
            let mut body = app(&bus).oneshot(req).await.unwrap().into_body();
            let mut seen = String::new();
            while !(seen.contains(expected_in[0]) && seen.contains(expected_in[1])) {
                seen.push_str(std::str::from_utf8(&body.frame().await.unwrap().unwrap().into_data().unwrap()).unwrap());
            }
            for out in expected_out.iter().filter(|s| !s.is_empty()) {
                assert!(!seen.contains(out), "{auth} ne doit pas voir « {out} » : {seen}");
            }
        }
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
