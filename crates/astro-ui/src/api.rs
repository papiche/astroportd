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
    /// Empreinte du compte sur les communs : disque utilisé et quota de sa formule, Opex et Capex.
    fn footprint(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Constellation : nœuds de l'essaim, Power-Score, distance, statut, services (Capitaine).
    fn constellation(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Régie coopérative : parts 3×1/3 de la dernière semaine, provision fiscale, Love Ledger (Capitaine).
    fn economy(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Carburant de la station : portefeuille maître à créditer pour ouvrir les portefeuilles coopératifs (Capitaine).
    /// `{ enabled: false }` sans transaction primale ; sinon `{ master, needed_g1_cents, spendable_g1_cents, waiting, ready, total, at }`.
    fn fuel(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Ok(json!({ "enabled": false })) }
    }
    /// Nom de domaine de la station : actuel, enregistré, ports, redémarrage possible (Capitaine).
    fn domain_info(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("réglage du domaine non disponible sur cette station")) }
    }
    /// Vérifie (forme, DNS, ports) et enregistre le nom de domaine ; effectif au redémarrage (Capitaine).
    fn domain_set(&self, _who: &Principal, _domain: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("réglage du domaine non disponible sur cette station")) }
    }
    /// Redémarre la station (systemd la relance) pour appliquer la configuration enregistrée (Capitaine).
    fn restart(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("redémarrage non disponible sur cette station")) }
    }
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
    /// Comptes hébergés : disque, cycle de vie (Capitaine).
    fn accounts(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Jeton de secours d'un compte hébergé (Capitaine).
    fn account_token(&self, who: &Principal, hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Fixe (ou retire, avec `None`) le quota d'un compte hébergé (Capitaine) : il remplace celui de la formule du compte.
    fn account_quota(&self, _who: &Principal, _hex: String, _quota_bytes: Option<u64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("quota non réglable sur cette station")) }
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
    /// Le MULTIPASS a-t-il déclaré une IA (Ollama ou clé d'API) ? Sans cela, l'onglet BRO n'apparaît pas.
    fn ai_declared(&self, _who: &Principal) -> impl Future<Output = bool> + Send {
        async { false }
    }
    /// Paramètres IA du MULTIPASS, sans la clé d'API : `{ ollama_url, provider, has_key, model, declared }`.
    fn ai_params(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("paramètres IA non disponibles sur cette station")) }
    }
    /// Change les paramètres IA (champ absent : inchangé ; `null` ou vide : effacé).
    fn set_ai_params(&self, _who: &Principal, _patch: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("paramètres IA non disponibles sur cette station")) }
    }
    /// Carnet d'adresses : une opération ([`BookOp`]) sur les fiches du titulaire.
    fn book(&self, _who: &Principal, _op: BookOp) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("le carnet d'adresses n'est pas disponible sur cette station")) }
    }
    /// Présence visio : `{ reachable, friends: { hex: statut } }` ; `body` (POST) change le choix `reachable` ou l'état `in_call`.
    fn presence(&self, _who: &Principal, _body: Option<Value>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("la présence visio n'est pas disponible sur cette station")) }
    }
    /// Visio : serveurs de traversée à donner au navigateur (`{ stun_port, servers }`).
    fn call_ice(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("la visio n'est pas disponible sur cette station")) }
    }
    /// Chat privé : une opération ([`ChatOp`]) pour le titulaire (dont les signaux d'appel visio).
    fn chat(&self, _who: &Principal, _op: ChatOp) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("le chat privé n'est pas disponible sur cette station")) }
    }
    /// Mes clés dans les UPlanet de cette station et les liens prouvés : `{ plane, keys: [{plane, hex, via}] }`.
    fn my_links(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("annuaire des UPlanet non disponible sur cette station")) }
    }
    /// Annonce, sur demande, que mes comptes des autres UPlanet de la station sont les miens (lien signé des deux clés, public).
    fn announce_links(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("annuaire des UPlanet non disponible sur cette station")) }
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
    /// Agents déclarés par le membre (sans jeton ni empreinte).
    fn agents_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Déclare un agent ; rend son jeton, montré une seule fois.
    fn agents_enroll(&self, _who: &Principal, _name: String, _group: String, _max_secs: u64, _totp: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Retire un agent (son jeton cesse de fonctionner).
    fn agents_revoke(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Demandes d'accès en cours : `{ requests: [{ id, agent, site, reason, state, until }] }`.
    fn agents_requests(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Le membre accorde (`secs` facultatif) ou refuse une demande.
    fn agents_decide(&self, _who: &Principal, _id: String, _approve: bool, _secs: Option<u64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Sources WebDAV externes du compte (sans mot de passe), avec la reprise en cours ou le dernier bilan.
    fn pull_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Enregistre une source (corps JSON : `url`, `username`, `password`, `label`, `remote_path`, `dest`) après l'avoir vérifiée.
    fn pull_add(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Retire une source.
    fn pull_remove(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Lance une reprise en arrière-plan (`dry_run` : compte sans rien écrire).
    fn pull_run(&self, _who: &Principal, _id: String, _dry_run: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Demande l'arrêt de la reprise en cours.
    fn pull_stop(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Rangement du uCloud par type : passage en cours ou dernier bilan, dossiers de type, passages annulables.
    fn organize_status(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Aperçu d'un rangement (corps JSON : `scope`, `by_date`, `kinds`), sans rien écrire.
    fn organize_preview(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Lance un rangement en arrière-plan.
    fn organize_run(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Demande l'arrêt du rangement en cours.
    fn organize_stop(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Annule un passage de rangement.
    fn organize_undo(&self, _who: &Principal, _run: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
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
    /// Serveur MCP (JSON-RPC 2.0) : `bearer` = jeton d'agent. `Ok(None)` : notification, rien à répondre ;
    /// jeton absent ou faux : `Err` en 401.
    fn mcp(&self, _bearer: Option<String>, _body: Value) -> impl Future<Output = Result<Option<Value>, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
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
    /// Annuaire : fiches de contact, état de leur liaison à un MULTIPASS, personnes reconnues sans fiche.
    fn contacts(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Relit le carnet d'adresses du compte et rapproche les visages.
    fn contacts_sync(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Crée la fiche d'une personne reconnue.
    fn contacts_create(&self, _who: &Principal, _name: String, _email: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Relie (ou délie) une fiche à un MULTIPASS.
    fn contacts_link(&self, _who: &Principal, _path: String, _target: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// « Mon compte » : identité, profil, accès WebDAV, relais annoncés.
    fn account(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Site public du compte (dossier `/Public`) : nom, adresses, état.
    fn account_site(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("site public non disponible sur cette station")) }
    }
    /// Publie (`publish`) ou retire du public des fichiers de l'uCloud : un déplacement vers `/Public` ou hors de `/Public`.
    fn public_move(&self, _who: &Principal, _paths: Vec<String>, _publish: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("site public non disponible sur cette station")) }
    }
    /// Images du dossier public du compte (pour choisir une image de profil).
    fn public_images(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
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
    /// Réglages d'une section d'administration (Capitaine) : `face`, `kinds`…
    fn admin_get(&self, _who: &Principal, _section: &str) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::not_found("section d'administration inconnue")) }
    }
    /// Tourne des photos d'un quart de tour (`turn` : 1 vers la droite, -1 vers la gauche) ; rend `{ rotated, failed: [{path, error}] }`.
    fn rotate_photos(&self, _who: &Principal, _paths: Vec<String>, _turn: i8) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rotation non disponible sur cette station")) }
    }
    /// Interroge la bibliothèque du compte (photos, souvenirs, chronologie, recherche, doublons).
    fn library(&self, _who: &Principal, _q: LibraryQuery) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("bibliothèque non disponible sur cette station")) }
    }
    /// Ouvre la console que `ticket` autorise (à usage unique, délivré au Capitaine) ; `None` si le ticket est inconnu, périmé ou déjà utilisé.
    fn console_open(&self, _ticket: &str) -> impl Future<Output = Option<ConsoleLink>> + Send {
        async { None }
    }
    /// Modifie une section d'administration (Capitaine) ; rend la vue mise à jour.
    fn admin_set(&self, _who: &Principal, _section: &str, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::not_found("section d'administration inconnue")) }
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
    /// Le compte peut-il créer des Ğ1Billets ? (le Capitaine, ou un MULTIPASS qu'il a autorisé)
    fn billet_issuer(&self, _who: &Principal) -> impl Future<Output = bool> + Send {
        async { false }
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
    /// Les billets créés par l'appelant, avec leur état.
    fn billet_issued(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Ğ1Billets non disponibles sur cette station")) }
    }
    /// Ğ1Billet (public : le billet est un titre au porteur) : `op` = `check` (adresse), `identify` (deux parts), `destination` ou
    /// `redeem` ; `ip` : limite de débit. L'erreur est rangée par nature : refus, trop de demandes, service incomplet…
    fn billet(&self, _ip: Option<std::net::IpAddr>, _op: &'static str, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Ğ1Billets non disponibles sur cette station")) }
    }
    /// Proches réciproques du compte (double suivi) : `{ friends: [{ hex, label, hosted }] }`.
    fn friends(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("envoi à des proches non disponible sur cette station")) }
    }
    /// Envoie des fichiers (ou dossiers) du uCloud à des proches réciproques.
    fn ucloud_send(&self, _who: &Principal, _paths: Vec<String>, _friends: Vec<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("envoi à des proches non disponible sur cette station")) }
    }
    /// Sauvegarde privée chez un ami : mon réglage, les demandes reçues comme hébergeur, mes proches.
    fn backup_status(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Active la sauvegarde chez `host` (la demande lui est envoyée) ou la suspend.
    fn backup_set(&self, _who: &Principal, _enabled: bool, _host: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Réponse de l'hébergeur : accepter avec un volume (en octets), refuser ou révoquer.
    fn backup_respond(&self, _who: &Principal, _owner: String, _accept: bool, _quota_bytes: u64) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Secours, côté gardien : `accept` rend sa part de la clé de secours à la station où `owner` se rétablit, sinon refuse.
    fn backup_rescue(&self, _who: &Principal, _owner: String, _accept: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Réplication des données du compte : où et par qui. `{ crawl, routes, mine, peers }`.
    fn replication(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("réplication non mesurée sur cette station")) }
    }
    /// L'inscription d'un MULTIPASS est-elle ouverte ? `{ open, constellation, accounts, max_accounts }` (public).
    fn register_info(&self) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("inscription non disponible sur cette station")) }
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
    /// Rétablit sur cette station un MULTIPASS existant depuis sa carte secrète (public : la carte est la preuve). `ip` : limite de débit.
    fn restore(&self, _ip: Option<std::net::IpAddr>, _card: String, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("restauration non disponible sur cette station")) }
    }
    /// Secours par les gardiens (public : la carte imprimée P1 est le justificatif) : `send` prévient les gardiens, sinon
    /// l'état seul. `{ status: none|searching|waiting|ready, sent?, by?, denied?, asks_left? }`.
    fn restore_ask(&self, _card: String, _send: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("secours par les gardiens non disponible sur cette station")) }
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
        .route("/api/v1/me/ai", get(ai_params::<B>).post(set_ai_params::<B>))
        .route("/api/v1/book", get(book_list::<B>))
        .route("/api/v1/book/card", get(book_get::<B>).post(book_save::<B>))
        .route("/api/v1/book/delete", post(book_delete::<B>))
.route("/api/v1/ucloud/rotate", post(library_rotate::<B>))
.route("/api/v1/book/delete-many", post(book_delete_many::<B>))
.route("/api/v1/book/photo", get(book_photo::<B>).post(book_set_photo::<B>))
.route("/api/v1/book/photo-choices", get(book_photo_choices::<B>))
        .route("/api/v1/book/take", post(book_take::<B>))
        .route("/api/v1/book/group", post(book_group::<B>))
        .route("/api/v1/book/import", post(book_import::<B>))
        .route("/api/v1/book/duplicates", get(book_duplicates::<B>))
        .route("/api/v1/book/merge", post(book_merge::<B>))
        .route("/api/v1/book/merge-exact", post(book_merge_exact::<B>))
        .route("/api/v1/book/me", get(book_me::<B>).post(book_save_me::<B>))
        .route("/api/v1/me/links", get(my_links::<B>).post(announce_links::<B>))
        .route("/api/v1/chat", get(chat_list::<B>))
        .route("/api/v1/chat/messages", get(chat_messages::<B>))
        .route("/api/v1/chat/send", post(chat_send::<B>))
        .route("/api/v1/chat/read", post(chat_read::<B>))
        .route("/api/v1/call/signal", post(call_signal::<B>))
        .route("/api/v1/call/ice", get(call_ice::<B>))
        .route("/api/v1/presence", get(presence_get::<B>).post(presence_set::<B>))
        .route("/api/v1/dashboard", get(dashboard::<B>))
        .route("/api/v1/activity", get(activity_stream::<B>))
        .route("/api/v1/ucloud/list", get(ucloud_list::<B>))
        .route("/api/v1/ucloud/photos", get(ucloud_photos::<B>))
        .route("/api/v1/ucloud/have", post(library_have::<B>))
        .route("/api/v1/ucloud/memories", get(library_memories::<B>))
        .route("/api/v1/ucloud/timeline", get(library_timeline::<B>))
        .route("/api/v1/ucloud/search", get(library_search::<B>))
        .route("/api/v1/ucloud/duplicates", get(library_duplicates::<B>))
        .route("/api/v1/ucloud/map", get(library_map::<B>))
        .route("/api/v1/ucloud/share", post(ucloud_share::<B>))
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
        .route("/api/v1/bro/say", post(bro_say::<B>))
        .route("/api/v1/consent/private-copy", get(consent::<B>).post(consent_accept::<B>))
        .route("/api/v1/agents", get(agents_list::<B>).post(agents_enroll::<B>))
        .route("/api/v1/agents/revoke", post(agents_revoke::<B>))
        .route("/api/v1/agents/requests", get(agents_requests::<B>))
        .route("/api/v1/agents/decide", post(agents_decide::<B>))
        .route("/api/v1/pull", get(pull_list::<B>).post(pull_add::<B>))
        .route("/api/v1/pull/remove", post(pull_remove::<B>))
        .route("/api/v1/pull/run", post(pull_run::<B>))
        .route("/api/v1/pull/stop", post(pull_stop::<B>))
        .route("/api/v1/organize", get(organize_status::<B>))
        .route("/api/v1/organize/preview", post(organize_preview::<B>))
        .route("/api/v1/organize/run", post(organize_run::<B>))
        .route("/api/v1/organize/stop", post(organize_stop::<B>))
        .route("/api/v1/organize/undo", post(organize_undo::<B>))
        .route("/api/v1/devices", get(devices_list::<B>).post(devices_add::<B>))
        .route("/api/v1/devices/revoke", post(devices_revoke::<B>))
        .route("/mcp", post(mcp::<B>))
        .route("/api/v1/me/footprint", get(footprint::<B>))
        .route("/api/v1/register", get(register_info::<B>).post(register::<B>))
        .route("/api/v1/restore", post(restore::<B>))
        .route("/api/v1/restore/ask", post(restore_ask::<B>))
        .route("/api/v1/replication", get(replication::<B>))
        .route("/api/v1/invites", get(invites::<B>).post(invite_create::<B>))
        .route("/api/v1/invites/{token}", axum::routing::delete(invite_revoke::<B>))
        .route("/api/v1/invite/{token}", get(invite_preview::<B>))
        .route("/api/v1/invite/{token}/thumb/{index}", get(invite_thumb::<B>))
        .route("/api/v1/invite/{token}/claim", post(invite_claim::<B>))
        .route("/api/v1/invite/{token}/accept", post(invite_accept::<B>))
        .route("/api/v1/invite/{token}/deliver", post(invite_deliver::<B>))
        .route("/api/v1/invites/remote-accept", post(invite_accept_elsewhere::<B>))
        .route("/api/v1/contacts", get(contacts::<B>))
        .route("/api/v1/contacts/sync", post(contacts_sync::<B>))
        .route("/api/v1/contacts/from-person", post(contacts_create::<B>))
        .route("/api/v1/contacts/link", post(contacts_link::<B>))
        .route("/api/v1/account", get(account::<B>))
        .route("/api/v1/account/profile", post(account_profile::<B>))
        .route("/api/v1/account/position", post(account_position::<B>))
        .route("/api/v1/account/image", post(account_image::<B>))
        .route("/api/v1/account/site", get(account_site::<B>))
        .route("/api/v1/account/site/example", post(account_site_example::<B>))
        .route("/api/v1/public/move", post(public_move::<B>))
        .route("/api/v1/public/images", get(public_images::<B>))
        .route("/api/v1/profile/{hex}/{file}", get(profile_image::<B>))
        .route("/api/v1/account/token", post(account_token_rotate::<B>))
        .route("/api/v1/account/unlock", post(account_unlock::<B>))
        .route("/api/v1/account/sign", post(account_sign::<B>))
        .route("/api/v1/station/accounts/{hex}/unlock-pass", post(pass_unlock::<B>))
        .route("/api/v1/station/accounts/{hex}/eject", post(account_eject::<B>))
        .route("/api/v1/account/keys", post(account_keys::<B>))
        .route("/api/v1/station/admin/{section}", get(admin_get::<B>).post(admin_set::<B>))
        .route("/api/v1/station/console", get(console_ws::<B>))
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
        .route("/api/v1/billet/issued", get(billet_issued::<B>))
        .route("/api/v1/friends", get(friends::<B>))
        .route("/api/v1/ucloud/send", post(ucloud_send::<B>))
        .route("/api/v1/backup", get(backup_status::<B>).post(backup_set::<B>))
        .route("/api/v1/backup/respond", post(backup_respond::<B>))
        .route("/api/v1/backup/rescue", post(backup_rescue::<B>))
        .route("/api/v1/station/constellation", get(constellation::<B>))
        .route("/api/v1/station/economy", get(economy::<B>))
        .route("/api/v1/station/fuel", get(fuel::<B>))
        .route("/api/v1/station/domain", get(domain_info::<B>).post(domain_set::<B>))
        .route("/api/v1/station/restart", post(restart::<B>))
        .route("/api/v1/station/welcome", get(welcome_list::<B>).post(welcome_create::<B>))
        .route("/api/v1/station/welcome/{token}/revoke", post(welcome_revoke::<B>))
        .route("/api/v1/welcome/{token}", get(welcome_preview::<B>).post(welcome_arrive::<B>))
        .route("/api/v1/join/{id}", get(join_take::<B>))
        .route("/api/v1/station/accounts", get(accounts::<B>))
        .route("/api/v1/station/accounts/{hex}/token", post(account_token::<B>))
        .route("/api/v1/station/accounts/{hex}/quota", post(account_quota::<B>))
        .route("/api/v1/station/accounts/{hex}/hosting", post(account_hosting::<B>))
        .route("/api/v1/station/hosting", get(hosting_defaults::<B>).post(set_hosting_defaults::<B>))
        .route("/api/v1/station/planes", get(planes::<B>).post(plane_add::<B>))
        .route("/api/v1/station/planes/remove", post(plane_remove::<B>))
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

/// Le compte qui appelle : l'authentification (Basic ou NIP-98) est faite avant que la route ne lise sa requête ou son corps.
struct Authed(Principal);

impl<B: Backend> FromRequestParts<Arc<UiState<B>>> for Authed {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &Arc<UiState<B>>) -> Result<Self, Response> {
        st.backend.authenticate(&parts.headers, parts.method.as_str(), &parts.uri).await.map(Self).ok_or_else(unauthorized)
    }
}

/// Le Capitaine de la station : 403 pour tout autre compte, même authentifié.
struct Captain(Principal);

impl<B: Backend> FromRequestParts<Arc<UiState<B>>> for Captain {
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

async fn me<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    private(Json(json!({ "hex": p.hex, "email": p.email, "captain": p.captain, "ai": st.backend.ai_declared(&p).await, "billet_issuer": st.backend.billet_issuer(&p).await })).into_response())
}

#[derive(Deserialize)]
struct BookListQuery {
    q: Option<String>,
    group: Option<String>,
}

#[derive(Deserialize)]
struct BookPathQuery {
    path: String,
}

#[derive(Deserialize)]
struct BookSaveBody {
    path: Option<String>,
    fields: Value,
}

#[derive(Deserialize)]
struct BookPathBody {
    path: String,
}

#[derive(Deserialize)]
struct BookTakeBody {
    path: String,
    keys: Vec<String>,
}

#[derive(Deserialize)]
struct BookGroupBody {
    from: String,
    to: Option<String>,
}

#[derive(Deserialize)]
struct BookImportBody {
    vcf: String,
}

#[derive(Deserialize)]
struct BookMergeBody {
    keep: String,
    drop: Vec<String>,
}

#[derive(Deserialize)]
struct BookMergeExactBody {
    #[serde(default)]
    apply: bool,
    #[serde(default)]
    limit: Option<usize>,
}

async fn book_list<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookListQuery>) -> Response {
    reply(st.backend.book(&p, BookOp::List(q.q.unwrap_or_default().chars().take(200).collect(), q.group.filter(|g| g.len() <= 80))).await)
}

async fn book_get<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
    reply(st.backend.book(&p, BookOp::Get(q.path)).await)
}

async fn book_save<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookSaveBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Save(b.path, b.fields)).await)
}

#[derive(Deserialize)]
struct BookPathsBody {
paths: Vec<String>,
}
async fn book_delete_many<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPathsBody>) -> Response {
reply(st.backend.book(&p, BookOp::DeleteMany(b.paths)).await)
}
async fn book_photo<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
reply(st.backend.book(&p, BookOp::Photo(q.path)).await)
}
#[derive(Deserialize)]
struct BookPhotoBody {
path: String,
uri: Option<String>,
}
async fn book_set_photo<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPhotoBody>) -> Response {
reply(st.backend.book(&p, BookOp::SetPhoto(b.path, b.uri)).await)
}
async fn book_photo_choices<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
reply(st.backend.book(&p, BookOp::PhotoChoices(q.path)).await)
}
async fn book_delete<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPathBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Delete(b.path)).await)
}

async fn book_take<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookTakeBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Take(b.path, b.keys.into_iter().take(10).collect())).await)
}

async fn book_group<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookGroupBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Group(b.from, b.to)).await)
}

async fn book_import<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookImportBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Import(b.vcf)).await)
}

async fn book_duplicates<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.book(&p, BookOp::Duplicates).await)
}

async fn book_merge<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookMergeBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Merge(b.keep, b.drop)).await)
}

async fn book_merge_exact<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookMergeExactBody>) -> Response {
    reply(st.backend.book(&p, BookOp::MergeExact(b.apply, b.limit.unwrap_or(50).clamp(1, 200))).await)
}

async fn book_me<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.book(&p, BookOp::Me).await)
}

async fn book_save_me<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(fields): Json<Value>) -> Response {
    reply(st.backend.book(&p, BookOp::SaveMe(fields)).await)
}

#[derive(Deserialize)]
struct ChatPeerQuery {
    peer: String,
    before: Option<i64>,
}

#[derive(Deserialize)]
struct ChatSendBody {
    peer: String,
    text: String,
}

#[derive(Deserialize)]
struct ChatPeerBody {
    peer: String,
}

async fn my_links<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.my_links(&p).await)
}

async fn announce_links<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.announce_links(&p).await)
}

async fn chat_list<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.chat(&p, ChatOp::List).await)
}

async fn chat_messages<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<ChatPeerQuery>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Messages(q.peer.chars().take(80).collect(), q.before)).await)
}

async fn call_signal<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Signal(b)).await)
}

async fn presence_get<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.presence(&p, None).await)
}

async fn presence_set<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.presence(&p, Some(b)).await)
}

async fn call_ice<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.call_ice(&p).await)
}

async fn chat_send<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatSendBody>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Send(b.peer.chars().take(80).collect(), b.text.chars().take(20_000).collect())).await)
}

async fn chat_read<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatPeerBody>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Read(b.peer.chars().take(80).collect())).await)
}

async fn ai_params<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.ai_params(&p).await)
}

async fn set_ai_params<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(patch): Json<Value>) -> Response {
    if !patch.is_object() {
        return reply(Err(ApiError::bad_request("objet JSON attendu")));
    }
    reply(st.backend.set_ai_params(&p, patch).await)
}

async fn dashboard<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.dashboard(&p).await)
}

#[derive(Deserialize)]
struct ListQuery {
    path: Option<String>,
}

async fn ucloud_list<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<ListQuery>) -> Response {
    reply(st.backend.ucloud_list(&p, q.path.unwrap_or_else(|| "/".into())).await)
}

#[derive(Deserialize)]
struct HaveBody {
    shas: Vec<String>,
}

#[derive(Deserialize)]
struct RotateBody {
paths: Vec<String>,
turn: i8,
}
async fn library_rotate<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<RotateBody>) -> Response {
if b.paths.is_empty() || b.paths.len() > 200 || !matches!(b.turn, 1 | -1) {
return reply(Err(ApiError::bad_request("1 à 200 photos, quart de tour 1 ou -1")));
}
reply(st.backend.rotate_photos(&p, b.paths, b.turn).await)
}
async fn library_have<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<HaveBody>) -> Response {
    if body.shas.len() > 500 {
        return reply(Err(ApiError::bad_request("500 empreintes au plus par appel")));
    }
    reply(st.backend.library(&p, LibraryQuery::Have(body.shas)).await)
}

#[derive(Deserialize)]
struct MemoriesQuery {
    md: Option<String>,
    y: Option<i64>,
}

async fn library_memories<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<MemoriesQuery>) -> Response {
    reply(st.backend.library(&p, LibraryQuery::Memories(q.md.zip(q.y))).await)
}

#[derive(Deserialize)]
struct TimelineQuery {
    ym: Option<String>,
}

async fn library_timeline<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<TimelineQuery>) -> Response {
    reply(st.backend.library(&p, LibraryQuery::Timeline(q.ym)).await)
}

async fn library_search<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<LibrarySearch>) -> Response {
    if q.q.as_deref().is_some_and(|s| s.len() > 256) || q.person.as_deref().is_some_and(|s| s.len() > 128) {
        return reply(Err(ApiError::bad_request("recherche trop longue")));
    }
    reply(st.backend.library(&p, LibraryQuery::Search(q)).await)
}

async fn library_map<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<MapQuery>) -> Response {
    if let Some(b) = &q.bbox {
        let ok = b.split(',').count() == 4 && b.split(',').all(|v| v.parse::<f64>().is_ok_and(f64::is_finite));
        if !ok {
            return reply(Err(ApiError::bad_request("zone invalide : sud,ouest,nord,est en degrés")));
        }
    }
    reply(st.backend.library(&p, LibraryQuery::Map(q)).await)
}

#[derive(Deserialize)]
struct DuplicatesQuery {
    all: Option<u8>,
}

async fn library_duplicates<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<DuplicatesQuery>) -> Response {
    reply(st.backend.library(&p, LibraryQuery::Duplicates(q.all == Some(1))).await)
}

#[derive(Deserialize)]
struct PhotosQuery {
    before: Option<i64>,
    limit: Option<usize>,
}

async fn ucloud_photos<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<PhotosQuery>) -> Response {
    reply(st.backend.ucloud_photos(&p, q.before, q.limit.unwrap_or(60).clamp(1, 200)).await)
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

async fn faces<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.faces(&p).await)
}

async fn face_accept<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(id): Path<i64>) -> Response {
    reply(st.backend.face_accept(&p, id).await)
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn face_name<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(id): Path<i64>, Json(body): Json<NameBody>) -> Response {
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

async fn faces_folders<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.faces_folders(&p).await)
}

#[derive(serde::Deserialize)]
struct BoardQuery {
    /// Dossier du uCloud à regarder (`/Images/2024`) ; absent : tous les visages (les plus récents).
    dir: Option<String>,
}

async fn faces_board<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BoardQuery>) -> Response {
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

async fn faces_thumbs<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ThumbsBody>) -> Response {
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

async fn faces_assign<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AssignBody>) -> Response {
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

async fn faces_forget<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ForgetBody>) -> Response {
    let ids = match face_ids(body.ids, 2000) {
        Ok(i) => i,
        Err(r) => return r,
    };
    reply(st.backend.faces_forget(&p, ids).await)
}

async fn faces_delete_photos<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ForgetBody>) -> Response {
    let ids = match face_ids(body.ids, 20_000) {
        Ok(i) => i,
        Err(r) => return r,
    };
    reply(st.backend.faces_delete_photos(&p, ids).await)
}

async fn face_shares_pending<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.face_shares_pending(&p).await)
}

#[derive(serde::Deserialize)]
struct ConfirmShareBody {
    recipient_hex: String,
    accept: bool,
}

async fn face_shares_confirm<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ConfirmShareBody>) -> Response {
    let recipient = body.recipient_hex.to_ascii_lowercase();
    if recipient.len() != 64 || !recipient.bytes().all(|b| b.is_ascii_hexdigit()) {
        return reply(Err(ApiError::bad_request("clé publique de l'ami attendue (64 hexadécimaux)")));
    }
    reply(st.backend.face_shares_confirm(&p, recipient, body.accept).await)
}

async fn footprint<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.footprint(&p).await)
}

/// Les écrans d'administration sont réservés au Capitaine : 403 pour tout autre compte, même authentifié.
#[allow(clippy::result_large_err)]
fn captain_only(p: &Principal) -> Result<(), Response> {
    if p.captain { Ok(()) } else { Err(reply(Err(ApiError::forbidden("réservé au Capitaine de la station")))) }
}

async fn constellation<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.constellation(&p).await)
}

async fn economy<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.economy(&p).await)
}

/// Carburant de la station, avec le QR de l'adresse à créditer (lu par Cesium, Ğecko…).
async fn fuel<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.fuel(&p).await.map(|mut v| {
        if let Some(svg) = v["master"].as_str().and_then(crate::qr::svg) {
            v["qr_svg"] = Value::String(svg);
        }
        v
    }))
}

async fn domain_info<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.domain_info(&p).await)
}

async fn domain_set<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(b): Json<Value>) -> Response {
    let Some(domain) = b["domain"].as_str().map(str::to_owned) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"domain\": \"station.exemple.org\" }"))) };
    reply(st.backend.domain_set(&p, domain).await)
}

async fn restart<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.restart(&p).await)
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
async fn welcome_list<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Captain(p): Captain, Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
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

async fn welcome_create<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Captain(p): Captain, Json(b): Json<Value>) -> Response {
    let origin = origin_of(&st, &headers);
    reply(st.backend.welcome_create(&p, b).await.map(|v| with_welcome_link(v, &origin, true)))
}

async fn welcome_revoke<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Path(token): Path<String>) -> Response {
    reply(st.backend.welcome_revoke(&p, token).await)
}

/// Lien d'accueil, vu par un visiteur (aucun compte requis : c'est la porte d'entrée).
async fn welcome_preview<B: Backend>(State(st): State<Arc<UiState<B>>>, Path(token): Path<String>) -> Response {
    reply(st.backend.welcome_preview(token).await)
}

/// Arrivée par un lien d'accueil : les secrets de la réponse ne sont remis qu'une fois (`no-store`, comme l'inscription).
async fn welcome_arrive<B: Backend>(State(st): State<Arc<UiState<B>>>, Path(token): Path<String>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<RegisterBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"lang\": \"fr\" }"))) };
    let email = (!body.guest).then_some(body.email);
    reply(st.backend.welcome_arrive(ip, token, email, body.alias, body.lang).await)
}

/// Parrainage : la nouvelle station retire l'offre chiffrée (une seule fois). Sans compte : le jeton est la preuve.
async fn join_take<B: Backend>(State(st): State<Arc<UiState<B>>>, Path(id): Path<String>) -> Response {
    reply(st.backend.join_take(id).await)
}

async fn accounts<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.accounts(&p).await)
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
struct QuotaBody {
    /// Octets ; absent ou `null` : retire le quota propre au compte (retour à celui de sa formule).
    quota_bytes: Option<u64>,
}

async fn account_quota<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>, Json(body): Json<QuotaBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.account_quota(&p, hex.to_ascii_lowercase(), body.quota_bytes).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct PlaneBody {
    name: String,
    /// Contenu d'une `swarm.key` reçue ; absent : on fonde une UPlanet neuve.
    swarm_key: Option<String>,
}

async fn planes<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.planes(&p).await)
}

async fn plane_add<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<PlaneBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if body.name.len() <= 64 && body.swarm_key.as_ref().is_none_or(|k| k.len() <= 1024) => reply(st.backend.plane_add(&p, body.name, body.swarm_key.filter(|k| !k.trim().is_empty())).await),
            Ok(()) => reply(Err(ApiError::bad_request("nom ou clé trop long"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn plane_remove<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<PlaneBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if body.name.len() <= 64 => reply(st.backend.plane_remove(&p, body.name).await),
            Ok(()) => reply(Err(ApiError::bad_request("nom trop long"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
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

async fn account_hosting<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>, Json(body): Json<HostingBody>) -> Response {
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

async fn hosting_defaults<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) => reply(st.backend.hosting_defaults(&p).await),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn set_hosting_defaults<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<HostingBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) => reply(st.backend.set_hosting_defaults(&p, body.quota_bytes, body.weekly_zen).await),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct SayBody {
    text: String,
}

async fn consent<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.consent(&p).await)
}

#[derive(Deserialize)]
struct ConsentBody {
    version: u32,
}

async fn consent_accept<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<ConsentBody>) -> Response {
    reply(st.backend.consent_accept(&p, body.version).await)
}

async fn agents_list<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.agents_list(&p).await)
}

#[derive(Deserialize)]
struct EnrollBody {
    name: String,
    group: String,
    #[serde(default = "default_grant_secs")]
    max_secs: u64,
    #[serde(default)]
    totp: bool,
}

fn default_grant_secs() -> u64 {
    600
}

async fn agents_enroll<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<EnrollBody>) -> Response {
    reply(st.backend.agents_enroll(&p, body.name, body.group, body.max_secs, body.totp).await)
}

#[derive(Deserialize)]
struct AgentIdBody {
    id: String,
}

async fn agents_revoke<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.agents_revoke(&p, body.id).await)
}

async fn agents_requests<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.agents_requests(&p).await)
}

#[derive(Deserialize)]
struct DecideBody {
    id: String,
    approve: bool,
    secs: Option<u64>,
}

async fn agents_decide<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<DecideBody>) -> Response {
    reply(st.backend.agents_decide(&p, body.id, body.approve, body.secs).await)
}

async fn pull_list<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.pull_list(&p).await)
}

async fn pull_add<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.pull_add(&p, body).await)
}

async fn pull_remove<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.pull_remove(&p, body.id).await)
}

#[derive(Deserialize)]
struct PullRunBody {
    id: String,
    #[serde(default)]
    dry_run: bool,
}

async fn pull_run<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<PullRunBody>) -> Response {
    reply(st.backend.pull_run(&p, body.id, body.dry_run).await)
}

async fn pull_stop<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.pull_stop(&p).await)
}

async fn organize_status<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.organize_status(&p).await)
}

async fn organize_preview<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.organize_preview(&p, body).await)
}

async fn organize_run<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.organize_run(&p, body).await)
}

async fn organize_stop<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.organize_stop(&p).await)
}

#[derive(Deserialize)]
struct UndoBody {
    run: String,
}

async fn organize_undo<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<UndoBody>) -> Response {
    reply(st.backend.organize_undo(&p, body.run).await)
}

async fn devices_list<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.devices_list(&p).await)
}

async fn devices_add<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.devices_add(&p, body).await)
}

async fn devices_revoke<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.devices_revoke(&p, body.id).await)
}

/// Serveur MCP : le jeton d'agent (`Authorization: Bearer`) remplace la connexion d'un compte.
async fn mcp<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let bearer = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(|t| t.trim().to_owned());
    match st.backend.mcp(bearer, body).await {
        Ok(Some(v)) => private(Json(v).into_response()),
        Ok(None) => private(StatusCode::ACCEPTED.into_response()),
        Err(e) => {
            // Un jeton refusé annonce le schéma attendu, comme le veut HTTP.
            let mut r = e.into_response();
            if r.status() == StatusCode::UNAUTHORIZED {
                r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            private(r)
        }
    }
}

async fn bro_say<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<SayBody>) -> Response {
    let text = body.text.trim().to_owned();
    if text.is_empty() || text.chars().count() > 2000 {
        return reply(Err(ApiError::bad_request("message de 1 à 2000 caractères attendu")));
    }
    reply(st.backend.bro_say(&p, text).await)
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

#[derive(Deserialize)]
struct RescueBody {
    owner: String,
    accept: bool,
}

async fn coop_get<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.coop_config(&p).await)
}

async fn constellation_get<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.cooperative_view(&p, None).await)
}

#[derive(serde::Deserialize)]
struct LedgerQuery {
    since: Option<u64>,
    limit: Option<usize>,
}

async fn constellation_ledger<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<LedgerQuery>) -> Response {
    reply(st.backend.cooperative_view(&p, Some((q.since.unwrap_or(0), q.limit.unwrap_or(5000).clamp(1, 20_000)))).await)
}

fn client_ip(req: &axum::extract::Request) -> Option<std::net::IpAddr> {
    req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip())
}

#[derive(serde::Deserialize)]
struct BilletQuery {
    g1pub: String,
}

/// Lectures publiques d'un billet (valeur, origine, validité) et d'un destinataire. Jamais mises en cache.
async fn billet_check<B: Backend>(State(st): State<Arc<UiState<B>>>, Query(q): Query<BilletQuery>, req: axum::extract::Request) -> Response {
    no_store(reply(st.backend.billet(client_ip(&req), "check", serde_json::json!({ "g1pub": q.g1pub })).await))
}

async fn billet_destination<B: Backend>(State(st): State<Arc<UiState<B>>>, Query(q): Query<BilletQuery>, req: axum::extract::Request) -> Response {
    no_store(reply(st.backend.billet(client_ip(&req), "destination", serde_json::json!({ "g1pub": q.g1pub })).await))
}

/// Deux parts : reconnaître le billet sans y toucher.
async fn billet_identify<B: Backend>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    billet_post(st, "identify", req).await
}

/// Encaisse : deux parts et un destinataire. Les parts ne transitent qu'ici, en HTTPS, et ne sont jamais journalisées.
async fn billet_redeem<B: Backend>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    billet_post(st, "redeem", req).await
}

/// Création de billets : réservée aux émetteurs que le Capitaine a choisis. Les parts ne sont rendues qu'ici, une fois : `no-store`.
async fn billet_issue<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.billet_issue(&p, body).await))
}

async fn billet_fund<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    no_store(reply(st.backend.billet_fund(&p, body).await))
}

async fn billet_issued<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    no_store(reply(st.backend.billet_issued(&p).await))
}

async fn billet_post<B: Backend>(st: Arc<UiState<B>>, op: &'static str, req: axum::extract::Request) -> Response {
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

async fn charter_accept<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(body): Json<CharterBody>) -> Response {
    reply(st.backend.charter_accept(&p, &body.sha256).await)
}

async fn contacts<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.contacts(&p).await)
}

async fn contacts_sync<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.contacts_sync(&p).await)
}

#[derive(serde::Deserialize)]
struct PersonBody {
    name: String,
    #[serde(default)]
    email: Option<String>,
}

async fn contacts_create<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<PersonBody>) -> Response {
    reply(st.backend.contacts_create(&p, body.name, body.email).await)
}

#[derive(serde::Deserialize)]
struct LinkBody {
    path: String,
    #[serde(default)]
    target: Option<String>,
}

async fn contacts_link<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<LinkBody>) -> Response {
    reply(st.backend.contacts_link(&p, body.path, body.target).await)
}

async fn account<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account(&p).await)
}

async fn account_site<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account_site(&p).await)
}

#[derive(Deserialize)]
struct PublicMoveBody {
    paths: Vec<String>,
    publish: bool,
}

async fn public_move<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<PublicMoveBody>) -> Response {
    reply(st.backend.public_move(&p, body.paths, body.publish).await)
}

async fn public_images<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.public_images(&p).await)
}

#[derive(Deserialize, Default)]
struct SiteExampleBody {
    #[serde(default)]
    force: bool,
}

async fn account_site_example<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, body: Option<Json<SiteExampleBody>>) -> Response {
    reply(st.backend.account_site_example(&p, body.map(|b| b.0.force).unwrap_or(false)).await)
}

async fn account_image<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_image(&p, body).await)
}

/// Image de profil : publique (un profil Nostr l'est), servie telle qu'elle a été réencodée, mise en cache une heure.
async fn profile_image<B: Backend>(State(st): State<Arc<UiState<B>>>, Path((hex, file)): Path<(String, String)>) -> Response {
    let kind = file.strip_suffix(".jpg").unwrap_or("");
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return reply(Err(ApiError::not_found("image inconnue")));
    }
    match st.backend.profile_image(&hex.to_ascii_lowercase(), kind).await {
        Some(bytes) => (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "image/jpeg"), (axum::http::header::CACHE_CONTROL, "public, max-age=3600"), (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")], bytes).into_response(),
        None => reply(Err(ApiError::not_found("image inconnue"))),
    }
}

async fn account_profile<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_profile(&p, body).await)
}

async fn account_position<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_position(&p, body).await)
}

async fn account_token_rotate<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account_token_rotate(&p).await)
}

#[derive(Deserialize)]
struct UnlockBody {
    email: String,
    pass: String,
}

/// Connexion par code PASS : publique (c'est la porte d'entrée). Le jeton rendu n'est jamais mis en cache.
async fn account_unlock<B: Backend>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 1024).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<UnlockBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"pass\": \"123456\" }"))) };
    reply(st.backend.account_unlock(ip, body.email, body.pass).await)
}

async fn account_sign<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_sign(&p, body).await)
}

async fn pass_unlock<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.pass_unlock(&p, hex.to_ascii_lowercase()).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn account_eject<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
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

async fn account_keys<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<KeysBody>) -> Response {
    reply(st.backend.account_export_keys(&p, body.confirm).await)
}

fn valid_section(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

#[derive(Deserialize)]
struct ConsoleQuery {
    ticket: String,
}

/// Console WebSocket. L'authentification est le ticket (30 s, un seul usage, délivré au seul Capitaine) : un navigateur ne peut
/// pas poser d'en-tête `Authorization` sur un WebSocket. Une page d'un autre site ne passe pas : `Origin` doit être cette station.
async fn console_ws<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Query(q): Query<ConsoleQuery>, ws: WebSocketUpgrade) -> Response {
    let same_origin = headers
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .and_then(|o| o.split_once("://").map(|(_, host)| host))
        .zip(headers.get(header::HOST).and_then(|h| h.to_str().ok()))
        .is_some_and(|(origin, host)| origin.eq_ignore_ascii_case(host));
    if !same_origin {
        return reply(Err(ApiError::forbidden("origine refusée")));
    }
    let Some(link) = st.backend.console_open(&q.ticket).await else { return reply(Err(ApiError::forbidden("ticket inconnu, périmé ou déjà utilisé"))) };
    ws.max_message_size(64 * 1024).on_upgrade(move |sock| bridge_console(sock, link))
}

async fn bridge_console(mut sock: WebSocket, link: ConsoleLink) {
    let ConsoleLink { input, mut output } = link;
    loop {
        tokio::select! {
            out = output.recv() => match out {
                Some(data) => if sock.send(Message::Binary(data.into())).await.is_err() { break },
                None => {
                    let _ = sock.send(Message::Close(None)).await;
                    break;
                }
            },
            msg = sock.recv() => match msg {
                Some(Ok(Message::Binary(b))) => if input.send(ConsoleIn::Data(b.to_vec())).await.is_err() { break },
                Some(Ok(Message::Text(t))) => {
                    let size = serde_json::from_str::<Value>(&t).ok().and_then(|v| Some((u16::try_from(v["resize"][0].as_u64()?).ok()?, u16::try_from(v["resize"][1].as_u64()?).ok()?)));
                    if let Some((cols, rows)) = size.filter(|(c, r)| (1..=500).contains(c) && (1..=200).contains(r)) {
                        if input.send(ConsoleIn::Resize(cols, rows)).await.is_err() { break }
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                _ => break,
            },
        }
    }
}

async fn admin_get<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Path(section): Path<String>) -> Response {
    if !valid_section(&section) {
        return reply(Err(ApiError::not_found("section d'administration inconnue")));
    }
    reply(st.backend.admin_get(&p, &section).await)
}

async fn admin_set<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Path(section): Path<String>, Json(body): Json<Value>) -> Response {
    if !valid_section(&section) {
        return reply(Err(ApiError::not_found("section d'administration inconnue")));
    }
    reply(st.backend.admin_set(&p, &section, body).await)
}

#[derive(serde::Deserialize)]
struct CoopBody {
    values: std::collections::BTreeMap<String, Option<String>>,
}

async fn coop_set<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain, Json(body): Json<CoopBody>) -> Response {
    if body.values.is_empty() || body.values.len() > 64 || body.values.keys().any(|k| k.len() > 64 || !k.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')) {
        return reply(Err(ApiError::bad_request("1 à 64 clés en majuscules attendues")));
    }
    reply(st.backend.coop_config_set(&p, body.values).await)
}

async fn friends<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.friends(&p).await)
}

#[derive(serde::Deserialize)]
struct SendBody {
    paths: Vec<String>,
    friends: Vec<String>,
}

async fn ucloud_send<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<SendBody>) -> Response {
    if body.paths.is_empty() || body.paths.len() > 200 || body.paths.iter().any(|p| !p.starts_with('/') || p.len() > 1024 || p.split('/').any(|s| s == "..")) {
        return reply(Err(ApiError::bad_request("1 à 200 chemins absolus attendus (sans « .. »)")));
    }
    if body.friends.is_empty() || body.friends.len() > 50 || body.friends.iter().any(|f| f.len() != 64 || !f.bytes().all(|b| b.is_ascii_hexdigit())) {
        return reply(Err(ApiError::bad_request("1 à 50 proches (clés Nostr de 64 caractères hexadécimaux) attendus")));
    }
    let friends = body.friends.into_iter().map(|f| f.to_ascii_lowercase()).collect();
    reply(st.backend.ucloud_send(&p, body.paths, friends).await)
}

async fn backup_status<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.backup_status(&p).await)
}

#[derive(serde::Deserialize)]
struct BackupSetBody {
    enabled: bool,
    host: Option<String>,
}

async fn backup_set<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<BackupSetBody>) -> Response {
    if body.host.as_ref().is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit())) {
        return reply(Err(ApiError::bad_request("clé de l'ami : 64 caractères hexadécimaux attendus")));
    }
    reply(st.backend.backup_set(&p, body.enabled, body.host.map(|h| h.to_ascii_lowercase())).await)
}

#[derive(serde::Deserialize)]
struct BackupRespondBody {
    owner: String,
    accept: bool,
    /// Volume mis à disposition, en octets (obligatoire pour accepter).
    #[serde(default)]
    quota_bytes: u64,
}

async fn backup_respond<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<BackupRespondBody>) -> Response {
    if body.owner.len() != 64 || !body.owner.bytes().all(|b| b.is_ascii_hexdigit()) {
        return reply(Err(ApiError::bad_request("clé de l'ami : 64 caractères hexadécimaux attendus")));
    }
    reply(st.backend.backup_respond(&p, body.owner.to_ascii_lowercase(), body.accept, body.quota_bytes).await)
}

/// Réplication des données du compte connecté.
async fn replication<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.replication(&p).await)
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
    if let Some(code) = body.setup_code {
        return reply(st.backend.register_captain(ip, code, body.email, body.lang).await);
    }
    if body.guest {
        return reply(st.backend.register_guest(ip, body.alias, body.lang).await);
    }
    reply(st.backend.register(ip, body.email, body.lang).await)
}

/// Rétablit un MULTIPASS depuis sa carte secrète. La réponse porte un nouveau PASS et un nouveau jeton, jamais redonnés : `no-store`.
async fn restore<B: Backend>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<RestoreBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"card\": \"astro-secret:v1|…\", \"lang\": \"fr\" }"))) };
    reply(st.backend.restore(ip, body.card, body.lang).await)
}

/// Secours par les gardiens depuis la carte imprimée (public). Réponse jamais mise en cache.
async fn restore_ask<B: Backend>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<RestoreAskBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"card\": \"astro-share:v1|…\", \"send\": true }"))) };
    reply(st.backend.restore_ask(body.card, body.send).await)
}

/// Réponse du gardien à une demande de secours.
async fn backup_rescue<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<RescueBody>) -> Response {
    if body.owner.len() != 64 || !body.owner.bytes().all(|b| b.is_ascii_hexdigit()) {
        return reply(Err(ApiError::bad_request("clé de l'ami : 64 caractères hexadécimaux attendus")));
    }
    reply(st.backend.backup_rescue(&p, body.owner.to_ascii_lowercase(), body.accept).await)
}

/// Origine publique du lien : celle déclarée par la station, sinon l'en-tête `Host`.
fn origin_of<B>(st: &UiState<B>, headers: &HeaderMap) -> String {
    st.public_origin.clone().or_else(|| headers.get(header::HOST).and_then(|h| h.to_str().ok()).map(|h| format!("http://{h}"))).unwrap_or_default()
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

async fn invite_create<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<Value>) -> Response {
    let origin = origin_of(&st, &headers);
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => reply(st.backend.invite_create(&p, body).await.map(|v| with_invite_link(v, &origin))),
        Err(r) => r,
    }
}

async fn invites<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
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

async fn invite_revoke<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(token): Path<String>) -> Response {
    reply(st.backend.invite_revoke(&p, token).await)
}

/// Public : le jeton du lien est la seule preuve. Jamais mis en cache ni indexé.
async fn invite_preview<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(token): Path<String>) -> Response {
    if nostr_scheme(&headers) {
        return reply(st.backend.invite_preview_signed(&headers, method.as_str(), &uri, token).await);
    }
    reply(st.backend.invite_preview(token).await)
}

async fn invite_thumb<B: Backend>(State(st): State<Arc<UiState<B>>>, Path((token, index)): Path<(String, usize)>) -> Response {
    match st.backend.invite_thumb(token, index).await {
        Ok(png) => private((StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], png).into_response()),
        Err(e) => reply(Err(e)),
    }
}

/// Corps maximal d'un appel de station à station (une liste de contacts signée).
const REMOTE_BODY_BYTES: usize = 256 * 1024;

fn nostr_scheme(headers: &HeaderMap) -> bool {
    headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.split_whitespace().next()).is_some_and(|s| s.eq_ignore_ascii_case("nostr"))
}

/// Accepte l'invitation avec le compte connecté : l'identité vient de la session (Basic) ou d'une signature NIP-98, jamais
/// d'une adresse saisie. Une signature peut venir d'une autre station, au nom d'un compte qu'elle héberge.
async fn invite_accept<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(token): Path<String>, body: axum::body::Bytes) -> Response {
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
async fn invite_deliver<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(token): Path<String>, body: axum::body::Bytes) -> Response {
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
async fn invite_accept_elsewhere<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<RemoteAcceptBody>) -> Response {
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

fn default_lang() -> String {
    "fr".into()
}

/// Crée le MULTIPASS du porteur du lien. Les secrets de la réponse ne sont jamais remis une seconde fois : `no-store`.
async fn invite_claim<B: Backend>(State(st): State<Arc<UiState<B>>>, Path(token): Path<String>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 2048).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<ClaimBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"lang\": \"fr\" }"))) };
    let who = if body.guest { Applicant::Guest(body.alias) } else { Applicant::Email(body.email) };
    reply(st.backend.invite_claim(ip, token, who, body.lang).await)
}

/// Flux SSE : d'abord ce que le client a manqué (`Last-Event-ID`), puis le direct.
async fn activity_stream<B: Backend>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    let principal = match who(&st, &headers, &method, &uri).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let after = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let (backlog, rx) = st.activity.subscribe(after);
    // Tant que ce flux est ouvert, le compte est « là » (présence visio, pour ses amis qui l'ont choisie).
    let here = st.activity.listen(&principal.hex);
    // Un événement personnel n'est ni rejoué ni diffusé à un autre compte (virement, e-mail d'un nouveau membre…).
    let backlog: Vec<_> = backlog.into_iter().filter(|a| a.audience.admits(&principal.hex, principal.captain)).collect();
    let sse = Sse::new(event_stream(backlog, rx, principal, here)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"));
    private(sse.into_response())
}

type Live = tokio::sync::broadcast::Receiver<Arc<astro_core::Activity>>;

fn to_event(a: &astro_core::Activity) -> Event {
    Event::default().id(a.id.to_string()).event(a.kind.clone()).data(serde_json::to_string(a).unwrap_or_default())
}

fn event_stream(backlog: Vec<Arc<astro_core::Activity>>, rx: Live, who: Principal, here: astro_core::activity::ListenGuard) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold((backlog.into_iter(), rx, who, here), |(mut backlog, mut rx, who, here)| async move {
        if let Some(a) = backlog.next() {
            return Some((Ok(to_event(&a)), (backlog, rx, who, here)));
        }
        loop {
            match rx.recv().await {
                Ok(a) if a.audience.admits(&who.hex, who.captain) => return Some((Ok(to_event(&a)), (backlog, rx, who, here))),
                Ok(_) => {}
                // Le client lisait trop lentement : on le dit plutôt que de laisser croire à un flux complet.
                Err(RecvError::Lagged(n)) => return Some((Ok(Event::default().event("lag").data(n.to_string())), (backlog, rx, who, here))),
                Err(RecvError::Closed) => return None,
            }
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
