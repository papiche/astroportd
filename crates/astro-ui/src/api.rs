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
//! | `GET /api/v1/faces/board` · `POST …/thumbs` · `…/assign` · `…/forget` | table de tri : positions, groupes, vignettes, rangement |
//! | `GET` · `POST /api/v1/station/coop` | configuration coopérative (Capitaine) : kind 30800, secrets en écriture seule |
//! | `GET` · `POST /api/v1/station/admin/{section}` | réglages du Capitaine par section (`face` : moteur FaceID ; `kinds` : matrice du relai) |
//! | `GET /api/v1/account` · `POST …/profile` · `…/token` · `…/keys` | Mon compte : identité, profil, nouveau jeton WebDAV, export des clés |
//! | `GET /api/v1/contacts` · `POST …/sync` · `…/from-person` · `…/link` | annuaire CardDAV : fiches, personnes sans fiche, rapprochement visage ↔ fiche ↔ MULTIPASS |
//! | `GET /api/v1/friends` · `POST /api/v1/ucloud/send` | proches réciproques ; envoi de fichiers à des proches |
//! | `GET`/`POST /api/v1/backup` · `POST /api/v1/backup/respond` | sauvegarde privée chez un ami : réglage, demandes reçues, réponse de l'hébergeur |
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
    /// Doublons exacts et visuels.
    Duplicates,
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
    /// La table de tri des visages : positions PCA, groupes, suggestions, personnes.
    fn faces_board(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("table de tri non disponible sur cette station")) }
    }
    /// Vignettes carrées de visages (`size` pixels) : `{ thumbs: { id: data URL }, failed: [[id, raison]] }`.
    fn faces_thumbs(&self, _who: &Principal, _ids: Vec<i64>, _size: u32) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("vignettes de visages non disponibles sur cette station")) }
    }
    /// Range des visages dans une personne (`pubkey` : sa clé Nostr, facultative).
    fn faces_assign(&self, _who: &Principal, _ids: Vec<i64>, _name: String, _pubkey: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("FaceCloud non activé")) }
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
    /// Modifie le profil (`name`, `about`, `picture`, `lang`).
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
        .route("/api/v1/ucloud/have", post(library_have::<B>))
        .route("/api/v1/ucloud/memories", get(library_memories::<B>))
        .route("/api/v1/ucloud/timeline", get(library_timeline::<B>))
        .route("/api/v1/ucloud/search", get(library_search::<B>))
        .route("/api/v1/ucloud/duplicates", get(library_duplicates::<B>))
        .route("/api/v1/ucloud/share", post(ucloud_share::<B>))
        .route("/api/v1/faces", get(faces::<B>))
        .route("/api/v1/faces/board", get(faces_board::<B>))
        .route("/api/v1/faces/thumbs", post(faces_thumbs::<B>))
        .route("/api/v1/faces/assign", post(faces_assign::<B>))
        .route("/api/v1/faces/forget", post(faces_forget::<B>))
        .route("/api/v1/faces/{id}/accept", post(face_accept::<B>))
        .route("/api/v1/faces/{id}/name", post(face_name::<B>))
        .route("/api/v1/bro/say", post(bro_say::<B>))
        .route("/api/v1/consent/private-copy", get(consent::<B>).post(consent_accept::<B>))
        .route("/api/v1/me/footprint", get(footprint::<B>))
        .route("/api/v1/register", get(register_info::<B>).post(register::<B>))
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
        .route("/api/v1/account/token", post(account_token_rotate::<B>))
        .route("/api/v1/account/unlock", post(account_unlock::<B>))
        .route("/api/v1/account/sign", post(account_sign::<B>))
        .route("/api/v1/station/accounts/{hex}/unlock-pass", post(pass_unlock::<B>))
        .route("/api/v1/account/keys", post(account_keys::<B>))
        .route("/api/v1/station/admin/{section}", get(admin_get::<B>).post(admin_set::<B>))
        .route("/api/v1/station/console", get(console_ws::<B>))
        .route("/api/v1/station/coop", get(coop_get::<B>).post(coop_set::<B>))
        .route("/api/v1/friends", get(friends::<B>))
        .route("/api/v1/ucloud/send", post(ucloud_send::<B>))
        .route("/api/v1/backup", get(backup_status::<B>).post(backup_set::<B>))
        .route("/api/v1/backup/respond", post(backup_respond::<B>))
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

async fn me<B: Backend>(_: State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    private(Json(json!({ "hex": p.hex, "email": p.email, "captain": p.captain })).into_response())
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

async fn library_duplicates<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.library(&p, LibraryQuery::Duplicates).await)
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

async fn faces_board<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.faces_board(&p).await)
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
    reply(st.backend.faces_assign(&p, ids, name, body.pubkey.map(|k| k.to_ascii_lowercase())).await)
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
}

async fn coop_get<B: Backend>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.coop_config(&p).await)
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

async fn account_profile<B: Backend>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_profile(&p, body).await)
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
    if body.guest {
        return reply(st.backend.register_guest(ip, body.alias, body.lang).await);
    }
    reply(st.backend.register(ip, body.email, body.lang).await)
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
    // Un événement personnel n'est ni rejoué ni diffusé à un autre compte (virement, e-mail d'un nouveau membre…).
    let backlog: Vec<_> = backlog.into_iter().filter(|a| a.audience.admits(&principal.hex, principal.captain)).collect();
    let sse = Sse::new(event_stream(backlog, rx, principal)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"));
    private(sse.into_response())
}

type Live = tokio::sync::broadcast::Receiver<Arc<astro_core::Activity>>;

fn to_event(a: &astro_core::Activity) -> Event {
    Event::default().id(a.id.to_string()).event(a.kind.clone()).data(serde_json::to_string(a).unwrap_or_default())
}

fn event_stream(backlog: Vec<Arc<astro_core::Activity>>, rx: Live, who: Principal) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold((backlog.into_iter(), rx, who), |(mut backlog, mut rx, who)| async move {
        if let Some(a) = backlog.next() {
            return Some((Ok(to_event(&a)), (backlog, rx, who)));
        }
        loop {
            match rx.recv().await {
                Ok(a) if a.audience.admits(&who.hex, who.captain) => return Some((Ok(to_event(&a)), (backlog, rx, who))),
                Ok(_) => {}
                // Le client lisait trop lentement : on le dit plutôt que de laisser croire à un flux complet.
                Err(RecvError::Lagged(n)) => return Some((Ok(Event::default().event("lag").data(n.to_string())), (backlog, rx, who))),
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
