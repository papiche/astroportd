//! Implémentation du [`Backend`] de l'interface web : câble la base, WebDAV, FaceCloud, BRO, Duniter et le P2P.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use astro_bro::bus;
use astro_bro::{AnyEmbedder, MemoryStore, Meta, OllamaClient, Slot};
use astro_core::Amount;
use astro_dav::{BlobStore, DavState};
use astro_p2p::routing::haversine_km;
use astro_ui::api::{Applicant, AssignTarget, BookOp, ChatOp};
use astro_ui::api::{AccountApi, Auth, CoopApi, FacesApi, OnboardApi, SocialApi, StationApi, UcloudApi};
use astro_ui::{ApiError, Principal, Status, Subsystem};
use axum::http::{HeaderMap, Uri};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::station::StationServices;

/// `Auth` est le même pour chaque domaine : il ne lit que `services` et `dav`, que chaque structure de domaine possède.
macro_rules! impl_auth {
    ($t:ident) => {
        impl<B: BlobStore> Auth for $t<B> {
        async fn authenticate(&self, headers: &HeaderMap, method: &str, uri: &Uri) -> Option<Principal> {
            let account = self.dav.account_of(headers, method, uri).await?;
            let hex = account.owner.clone();
            let (email, role): (Option<String>, Option<String>) = self
                .services
                .db
                .readers()
                .read(move |c| Ok(c.query_row("SELECT email, role FROM identities WHERE hex = ?1", [hex], |r| Ok((r.get(0)?, r.get(1)?))).unwrap_or((None, None))))
                .await
                .ok()?;
            Some(Principal { hex: account.owner, email, captain: role.as_deref() == Some("captain") })
        }
        fn public_origin(&self, host: &str) -> String {
            match &self.services.info.domain {
                Some(d) => format!("https://{d}"),
                None => format!("http://{host}"),
            }
        }
        }
    };
}

mod ucloud;
pub use ucloud::UcloudBackend;
mod faces;
pub use faces::FacesBackend;
mod coop;
pub use coop::CoopBackend;
mod social;
pub use social::SocialBackend;
mod account;
pub use account::AccountBackend;
mod onboarding;
pub use onboarding::OnboardBackend;
mod station;
pub use station::StationBackend;

/// Mémoire de BRO telle que câblée par le démon.
pub type BroMemory = MemoryStore<AnyEmbedder, OllamaClient>;

/// Semaines d'autonomie à pleine jauge.
const GAUGE_WEEKS: f64 = 12.0;
/// Les sondes de `/health` (public) ne déclenchent jamais plus d'un appel sortant par minute.
const PROBE_TTL: Duration = Duration::from_secs(60);

/// Interface web de la station.
pub struct StationUi<B: BlobStore> {
    /// Services partagés.
    pub services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub dav: Arc<DavState<B>>,
    /// Mémoire de BRO (avec `--ollama`).
    pub memory: Option<Arc<BroMemory>>,
    /// Ollama (avec `--ollama`).
    pub ollama: Option<Arc<OllamaClient>>,
    /// FaceCloud est actif.
    pub faces_enabled: bool,
    thumbs_fn: Option<ThumbFn>,
    pending_fn: Option<PendingShareFn>,
    friends_fn: Option<FriendsFn>,
    backup: Option<BackupFns>,
    replicate_fn: Option<ReplicateFn>,
    coop: Option<(CoopGetFn, CoopSetFn)>,
    constellation: Option<ConstellationFn>,
    charter: Option<CharterFn>,
    billet: Option<BilletFn>,
    /// Versement de Ẑen par le Capitaine (même forme d'appel que les billets).
    zen_credit: Option<BilletFn>,
    site: Option<Arc<crate::public_site::Site<B>>>,
    admin: Option<AdminFn>,
    account: Option<Arc<crate::account::AccountService>>,
    contacts: Option<Arc<crate::identity_links::IdentityLinks<B>>>,
    invites: Option<Arc<crate::invites::Invites<B>>>,
    /// Liens d'accueil de la station (plusieurs arrivées par lien).
    welcome: Option<Arc<crate::welcome::Welcome<B>>>,
    agents: Option<Arc<crate::agents::Agents>>,
    pull: Option<Arc<crate::webdav_pull::PullService<B>>>,
    organize: Option<Arc<crate::organize::OrganizeService<B>>>,
    send_fn: Option<SendFn>,
    board: astro_facecloud::board::BoardCache,
    registrar: Option<Arc<crate::register::Registrar>>,
    stations: Option<Arc<crate::beacon::Stations>>,
    feedback: Option<Arc<crate::feedback::Feedback>>,
    oc: Option<Arc<crate::oc_member::OcMember>>,
    /// Code d'installation d'une station sans Capitaine (page `/welcome`).
    setup: Option<Arc<crate::first_run::SetupCode>>,
    /// `station.conf` (domaine réglé depuis le cockpit).
    conf_path: Option<std::path::PathBuf>,
    planes_home: Option<std::path::PathBuf>,
    unit_exe: Option<std::path::PathBuf>,
    ai: Option<Arc<crate::member_ai::MemberAi>>,
    book: Option<Arc<crate::address_book::AddressBook<B>>>,
    chat: Option<Arc<crate::chat::Chat>>,
    /// Visio : port STUN de la station (UDP), secret du relais d'appel s'il est actif, serveurs supplémentaires.
    ice: (Option<u16>, Option<[u8; 32]>, Vec<String>),
    presence: Option<Arc<crate::presence::Presence>>,
    people: Option<Arc<crate::person_keys::PersonKeys>>,
}

/// Réponse au lot de photos en attente pour un ami `(titulaire, ami, accepter)` → `(envoyées, retirées)` ; fourni par le binaire.
pub type PendingShareFn = Arc<dyn Fn(String, String, bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(usize, usize), String>> + Send>> + Send + Sync>;

/// Vignettes de visages d'un titulaire : `(vignettes par visage, visages en échec avec leur raison)` ; fourni par le binaire,
/// qui détient le service FaceCloud (clés, blobs, décodage).
pub type ThumbFn = Arc<dyn Fn(String, Vec<i64>, u32) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(std::collections::HashMap<i64, String>, Vec<(i64, String)>), String>> + Send>> + Send + Sync>;

/// Lecture de la configuration coopérative : vue pour l'interface.
pub type CoopGetFn = Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;
/// Écriture de la configuration coopérative : `{ clé: valeur | null }` ; rend la vue mise à jour.
pub type CoopSetFn = Arc<dyn Fn(std::collections::BTreeMap<String, Option<String>>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// Vision de la constellation pour le compte `viewer` : `None` = portefeuilles et charte, `Some((depuis, au plus))` = historique des virements.
pub type ConstellationFn = Arc<dyn Fn(String, Option<(u64, usize)>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// Acceptation de la charte : `(Capitaine, empreinte)` ; rend `{ version, sha256, at }`.
pub type CharterFn = Arc<dyn Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// Ğ1Billet : `(adresse du client, opération, corps)`.
pub type BilletFn = Arc<dyn Fn(Option<std::net::IpAddr>, &'static str, Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, crate::billet::BilletError>> + Send>> + Send + Sync>;

/// Réglages d'administration : `(section, None)` lit, `(section, Some(corps))` modifie ; rend la vue de la section.
/// Une erreur est un message destiné au Capitaine (« section inconnue » = 404).
pub type AdminFn = Arc<dyn Fn(String, Option<Value>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// Proches réciproques d'un titulaire ; fourni par le binaire (il détient la boîte d'envoi générique).
pub type FriendsFn = Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;
/// Réglage de la sauvegarde privée : `(compte, actif, ami)`.
pub type BackupSetFn = Arc<dyn Fn(String, bool, Option<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;
/// Copie d'une pièce jointe chez les amis hébergeurs : `(propriétaire, CID, octets, échéance)`.
pub type ReplicateFn = Arc<dyn Fn(String, String, u64, i64) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;
/// Réponse de l'hébergeur : `(hébergeur, propriétaire, accepte, volume demandé, volume maximal)`.
pub type BackupRespondFn = Arc<dyn Fn(String, String, bool, u64, u64) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// Secours, nouvelle station : `(carte imprimée P1, envoyer la demande ?)` → état de la demande.
pub type RescueAskFn = Arc<dyn Fn(String, bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;
/// Secours, gardien : `(gardien, compte, c'est bien lui ?)`.
pub type RescueAnswerFn = Arc<dyn Fn(String, String, bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;
/// Secours, nouvelle station : part P3 rendue pour un compte (`Some(part)`), ou fin de la demande (`None` : compte rétabli).
pub type RescueShareFn = Arc<dyn Fn(String, bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> + Send + Sync>;

/// Sauvegarde privée : état, réglage `(compte, actif, ami)` et réponse de l'hébergeur `(hébergeur, propriétaire, accepte, volume, plafond)`.
#[derive(Clone)]
pub struct BackupFns {
    /// État du compte.
    pub status: FriendsFn,
    /// Active ou suspend.
    pub set: BackupSetFn,
    /// Réponse de l'hébergeur.
    pub respond: BackupRespondFn,
    /// Secours : demande aux gardiens depuis la carte imprimée.
    pub rescue_ask: RescueAskFn,
    /// Secours : réponse du gardien.
    pub rescue_answer: RescueAnswerFn,
    /// Secours : part rendue (`false`), ou effacement après restauration (`true`).
    pub rescue_share: RescueShareFn,
}

/// Envoi de fichiers d'un titulaire à des proches : `(titulaire, chemins, proches)`.
pub type SendFn = Arc<dyn Fn(String, Vec<String>, Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

impl<B: BlobStore> StationUi<B> {
    /// Assemble l'interface.
    pub fn new(services: Arc<StationServices>, dav: Arc<DavState<B>>, memory: Option<Arc<BroMemory>>, ollama: Option<Arc<OllamaClient>>, faces_enabled: bool) -> Self {
        Self { services, dav, memory, ollama, faces_enabled, thumbs_fn: None, pending_fn: None, friends_fn: None, backup: None, replicate_fn: None, send_fn: None, coop: None, constellation: None, charter: None, billet: None, zen_credit: None, site: None, admin: None, account: None, contacts: None, invites: None, welcome: None, agents: None, pull: None, organize: None, board: astro_facecloud::board::BoardCache::new(), registrar: None, stations: None, feedback: None, oc: None, setup: None, conf_path: None, planes_home: None, unit_exe: None, ai: None, book: None, chat: None, people: None, ice: (None, None, Vec::new()), presence: None }
    }

    /// Annuaire des clés d'une même personne dans plusieurs UPlanet.
    pub fn with_people(mut self, people: Arc<crate::person_keys::PersonKeys>) -> Self {
        self.people = Some(people);
        self
    }

    /// Chat privé 1 à 1 (NIP-17).
    pub fn with_chat(mut self, chat: Arc<crate::chat::Chat>) -> Self {
        self.chat = Some(chat);
        self
    }

    /// Présence visio (« en ligne », « en appel ») entre amis mutuels, sur option.
    pub fn with_presence(mut self, presence: Arc<crate::presence::Presence>) -> Self {
        self.presence = Some(presence);
        self
    }

    /// Visio : port STUN servi par la station (`None` : aucun), secret du relais d'appel (`None` : pas de relais) et serveurs
    /// STUN/TURN supplémentaires.
    pub fn with_ice(mut self, stun_port: Option<u16>, turn_secret: Option<[u8; 32]>, servers: Vec<String>) -> Self {
        self.ice = (stun_port, turn_secret, servers);
        self
    }

    /// Carnet d'adresses (fiches, groupes, doublons, « Ma fiche »).
    pub fn with_book(mut self, book: Arc<crate::address_book::AddressBook<B>>) -> Self {
        self.book = Some(book);
        self
    }

    /// Paramètres IA des MULTIPASS (Ollama ou clé d'API) : sans eux, BRO n'apparaît pas.
    pub fn with_ai(mut self, ai: Arc<crate::member_ai::MemberAi>) -> Self {
        self.ai = Some(ai);
        self
    }

    /// Dossier personnel où vit `planes.json` : active l'écran des plans du Capitaine.
    pub fn with_planes_home(mut self, home: std::path::PathBuf) -> Self {
        self.planes_home = Some(home);
        self
    }

    /// Exécutable que l'unité systemd doit lancer : celui de ce processus, sauf indication contraire (tests).
    pub fn with_unit_exe(mut self, exe: std::path::PathBuf) -> Self {
        self.unit_exe = Some(exe);
        self
    }

    /// Invitations par lien : photos envoyées à quelqu'un qui n'a pas encore de MULTIPASS.
    pub fn with_invites(mut self, invites: Arc<crate::invites::Invites<B>>) -> Self {
        self.invites = Some(invites);
        self
    }

    /// Liens d'accueil (`/w/<jeton>`).
    pub fn with_welcome(mut self, welcome: Arc<crate::welcome::Welcome<B>>) -> Self {
        self.welcome = Some(welcome);
        self
    }

    /// Registre des agents et serveur MCP avec demande d'accord.
    pub fn with_agents(mut self, agents: Arc<crate::agents::Agents>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// Reprise d'un dossier d'un autre serveur WebDAV (Nextcloud…), lancée depuis l'interface.
    pub fn with_pull(mut self, pull: Arc<crate::webdav_pull::PullService<B>>) -> Self {
        self.pull = Some(pull);
        self
    }

    /// Rangement du uCloud par type, lancé depuis l'interface.
    pub fn with_organize(mut self, organize: Arc<crate::organize::OrganizeService<B>>) -> Self {
        self.organize = Some(organize);
        self
    }

    /// Active l'annuaire (CardDAV, rapprochement photo ↔ fiche ↔ MULTIPASS).
    pub fn with_contacts(mut self, links: Arc<crate::identity_links::IdentityLinks<B>>) -> Self {
        self.contacts = Some(links);
        self
    }

    /// Active « Mon compte » (profil, jeton, export des clés).
    pub fn with_account(mut self, service: Arc<crate::account::AccountService>) -> Self {
        self.account = Some(service);
        self
    }

    /// Active les réglages d'administration du Capitaine (moteur FaceID, matrice des kinds…).
    pub fn with_admin(mut self, f: AdminFn) -> Self {
        self.admin = Some(f);
        self
    }


    /// Active l'écran de configuration coopérative du Capitaine.
    pub fn with_coop(mut self, get: CoopGetFn, set: CoopSetFn) -> Self {
        self.coop = Some((get, set));
        self
    }

    /// Active la vision de la constellation (portefeuilles, historique, charte) pour tous les membres.
    pub fn with_constellation(mut self, f: ConstellationFn, charter: CharterFn) -> Self {
        self.constellation = Some(f);
        self.charter = Some(charter);
        self
    }

    /// Active le site public (dossier `/Public`) : l'écran Mon compte en donne les adresses et dépose un site d'exemple.
    #[must_use]
    pub fn with_site(mut self, site: Arc<crate::public_site::Site<B>>) -> Self {
        self.site = Some(site);
        self
    }

    /// Active la vérification et l'encaissement des Ğ1Billets (page publique `/scan`).
    pub fn with_billet(mut self, f: BilletFn) -> Self {
        self.billet = Some(f);
        self
    }

    /// Active le versement de Ẑen par le Capitaine.
    pub fn with_zen_credit(mut self, f: BilletFn) -> Self {
        self.zen_credit = Some(f);
        self
    }

    /// Active l'envoi de fichiers à des proches.
    pub fn with_sharing(mut self, friends: FriendsFn, send: SendFn) -> Self {
        self.friends_fn = Some(friends);
        self.send_fn = Some(send);
        self
    }

    /// Copie chaque pièce jointe de message chez les amis hébergeurs (ceux qui gardent la part P3), sans retarder la réponse.
    pub fn with_attachment_replication(mut self, f: ReplicateFn) -> Self {
        self.replicate_fn = Some(f);
        self
    }

    /// Active la sauvegarde privée chez un ami.
    pub fn with_backup(mut self, fns: BackupFns) -> Self {
        self.backup = Some(fns);
        self
    }

    /// Active la confirmation des envois de photos en attente.
    pub fn with_pending_shares(mut self, f: PendingShareFn) -> Self {
        self.pending_fn = Some(f);
        self
    }

    /// Active les vignettes de visages de la table de tri.
    pub fn with_face_thumbs(mut self, f: ThumbFn) -> Self {
        self.thumbs_fn = Some(f);
        self
    }

    /// Ouvre l'inscription d'un MULTIPASS depuis l'interface (`/register`).
    pub fn with_registrar(mut self, r: Arc<crate::register::Registrar>) -> Self {
        self.registrar = Some(r);
        self
    }

    /// Retours des utilisateurs en issues Git (`POST /api/v1/feedback`).
    pub fn with_feedback(mut self, f: Arc<crate::feedback::Feedback>) -> Self {
        self.feedback = Some(f);
        self
    }

    /// Vérification d'inscription OpenCollective du compte (`GET /api/v1/account/oc`).
    pub fn with_oc(mut self, o: Arc<crate::oc_member::OcMember>) -> Self {
        self.oc = Some(o);
        self
    }

    /// Active l'annuaire public des stations de l'UPlanet (`GET /api/v1/stations`).
    pub fn with_stations(mut self, s: Arc<crate::beacon::Stations>) -> Self {
        self.stations = Some(s);
        self
    }

    /// `station.conf` de la station : le cockpit y enregistre le nom de domaine.
    pub fn with_conf_path(mut self, path: std::path::PathBuf) -> Self {
        self.conf_path = Some(path);
        self
    }

    /// Code d'installation (station démarrée sans Capitaine).
    pub fn with_setup(mut self, code: Option<Arc<crate::first_run::SetupCode>>) -> Self {
        self.setup = code;
        self
    }

}
impl<B: BlobStore> StationUi<B> {
    /// Assemble l'API : chaque domaine reçoit **sa** structure, qui ne garde que les services dont elle a besoin.
    /// `StationUi` n'est plus qu'un plan de câblage consommé au démarrage.
    pub fn into_router(self, activity: astro_core::ActivityBus, public_origin: Option<String>) -> axum::Router {
        astro_ui::api::Routes::new(activity, public_origin)
            .ucloud(Arc::new(UcloudBackend { services: self.services.clone(), dav: self.dav.clone(), friends_fn: self.friends_fn.clone(), backup: self.backup.clone(), replicate_fn: self.replicate_fn.clone(), site: self.site.clone(), pull: self.pull.clone(), organize: self.organize.clone(), send_fn: self.send_fn.clone() }))
            .faces(Arc::new(FacesBackend { services: self.services.clone(), dav: self.dav.clone(), faces_enabled: self.faces_enabled, thumbs_fn: self.thumbs_fn.clone(), pending_fn: self.pending_fn.clone(), contacts: self.contacts.clone(), board: self.board, book: self.book.clone() }))
            .coop(Arc::new(CoopBackend { services: self.services.clone(), dav: self.dav.clone(), coop: self.coop.clone(), constellation: self.constellation.clone(), charter: self.charter.clone(), billet: self.billet.clone(), zen_credit: self.zen_credit.clone(), stations: self.stations.clone(), oc: self.oc.clone() }))
            .social(Arc::new(SocialBackend { services: self.services.clone(), dav: self.dav.clone(), memory: self.memory.clone(), contacts: self.contacts.clone(), agents: self.agents.clone(), ai: self.ai.clone(), book: self.book.clone(), chat: self.chat.clone(), ice: self.ice.clone(), presence: self.presence.clone(), people: self.people.clone() }))
            .account(Arc::new(AccountBackend { services: self.services.clone(), dav: self.dav.clone(), billet: self.billet.clone(), site: self.site.clone(), account: self.account.clone(), registrar: self.registrar.clone(), feedback: self.feedback.clone(), ai: self.ai.clone() }))
            .onboarding(Arc::new(OnboardBackend { services: self.services.clone(), dav: self.dav.clone(), backup: self.backup.clone(), invites: self.invites.clone(), welcome: self.welcome.clone(), registrar: self.registrar.clone(), stations: self.stations.clone(), setup: self.setup.clone() }))
            .station(Arc::new(StationBackend { services: self.services.clone(), dav: self.dav.clone(), ollama: self.ollama.clone(), faces_enabled: self.faces_enabled, admin: self.admin.clone(), conf_path: self.conf_path.clone(), probe: Mutex::new(None), planes_home: self.planes_home.clone(), unit_exe: self.unit_exe.clone(), ice: self.ice.clone() }))
            .build()
    }
}

fn device_err(e: astro_dav::devices::DeviceError) -> ApiError {
    use astro_dav::devices::DeviceError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound => ApiError::not_found("appareil inconnu"),
        E::Db(m) => db_err(m),
    }
}

fn organize_err(e: crate::organize::OrganizeError) -> ApiError {
    match e {
        crate::organize::OrganizeError::Invalid(m) => ApiError::bad_request(m),
        crate::organize::OrganizeError::Internal(m) => db_err(m),
    }
}

fn organize_options(body: Value) -> Result<crate::organize::Options, ApiError> {
    serde_json::from_value(body).map_err(|_| ApiError::bad_request("réglages de rangement illisibles"))
}

fn pull_err(e: crate::webdav_pull::PullError) -> ApiError {
    use crate::webdav_pull::PullError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::Remote(m) => ApiError::bad_request(format!("serveur distant : {m}")),
        E::Fetch(f) => ApiError::bad_request(f.to_string()),
        E::Internal(m) => db_err(m),
    }
}

fn agent_err(e: crate::agents::AgentError) -> ApiError {
    use crate::agents::AgentError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound => ApiError::not_found("agent ou demande introuvable"),
        E::Limited(m) => ApiError::too_many(m),
        E::Unauthorized => ApiError { status: axum::http::StatusCode::UNAUTHORIZED, message: "jeton d'agent refusé".into(), code: Some("agent_token") },
        E::Internal(m) => db_err(m),
    }
}

fn db_err(e: impl std::fmt::Display) -> ApiError {
    eprintln!("interface : erreur de base : {e}");
    ApiError::internal("erreur interne")
}

fn entry_json(e: &astro_dav::vfs::Entry) -> Value {
    json!({
        "name": astro_dav::vfs::name_of(&e.path), "path": e.path,
        "type": if e.kind == astro_dav::vfs::EntryKind::Dir { "dir" } else { "file" },
        "size": e.size, "mime": e.mime, "mtime": e.mtime,
    })
}

/// Compte hébergé : clé, rôle, email, contrat, état du cycle de vie (JSON), octets utilisés, fichiers.
type AccountRow = (String, String, Option<String>, Option<String>, Option<String>, i64, i64);

/// Miniature d'une entrée, sans son CID ni sa taille : l'interface n'a besoin que des dimensions.
fn thumb_of(json: Option<String>) -> Value {
    let Some(v) = json.and_then(|j| serde_json::from_str::<Value>(&j).ok()) else { return Value::Null };
    json!({ "w": v["w"], "h": v["h"], "ow": v["ow"], "oh": v["oh"] })
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn db_bytes(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |r| r.get(0))
}

/// Le geste qui règle un échec ACME, d'après la cause que donne l'autorité (Let's Encrypt teste la station depuis Internet,
/// comme le ferait une station amie : sa réponse dit déjà ce qui manque).
pub fn acme_hint(error: &str, domain: &str) -> String {
    let e = error.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
    if has(&["ratelimited", "rate limit", "too many"]) {
        "quota de Let's Encrypt atteint pour ce domaine : la station réessaiera d'elle-même (patientez, sans redémarrer en boucle)".into()
    } else if has(&["nxdomain", "no valid ip", "servfail", "dns problem", "no such host", "dns"]) {
        format!("le DNS de {domain} ne mène pas ici : chez votre fournisseur de domaine, l'enregistrement A doit donner l'adresse publique de votre box")
    } else if has(&["timeout", "timed out", "connection refused", "connection reset", "firewall", "connect"]) {
        "Internet n'arrive pas jusqu'à la station : dans la box, redirigez les ports 80 et 443 (TCP) vers cette machine, et vérifiez son pare-feu".into()
    } else if has(&["unauthorized", "404", "invalid response"]) {
        format!("un autre serveur répond pour {domain} sur le port 80 : le DNS ou la redirection de la box vise une autre machine")
    } else {
        format!("vérifiez que le DNS de {domain} pointe sur la box et que la box redirige les ports 80 et 443 vers cette machine")
    }
}

/// Le certificat HTTPS vu par le Capitaine : un échec ACME n'est plus seulement une ligne de journal (les reprises
/// s'espacent jusqu'à 6 h), il remonte au cockpit avec sa cause et le geste qui la règle le plus souvent.
pub fn tls_subsystem(tls: Option<(Vec<String>, astro_ui::acme::AcmeStatus)>, now: i64) -> Subsystem {
    const SOON: i64 = 7 * 24 * 3600;
    let Some((domains, st)) = tls else { return Subsystem::new("https", Status::Disabled, "sans domaine : HTTP local ou proxy (NPM) qui gère le certificat") };
    let domain = domains.first().cloned().unwrap_or_default();
    match (st.last_error, st.not_after) {
        (Some(e), Some(end)) if end > now => Subsystem::new("https", Status::Degraded, format!("renouvellement en échec ({e}) ; le certificat actuel reste valable {} jour(s)", (end - now) / 86_400)),
        (Some(e), _) => Subsystem::new("https", Status::Down, format!("certificat de {domain} non obtenu : {}. ({e}) Nouvel essai automatique.", acme_hint(&e, &domain))),
        (None, Some(end)) if end <= now + SOON => Subsystem::new("https", Status::Degraded, format!("le certificat de {domain} expire dans {} jour(s)", (end - now).max(0) / 86_400)),
        (None, Some(end)) => Subsystem::new("https", Status::Ok, format!("{domain}, valable {} jour(s)", (end - now) / 86_400)),
        (None, None) => Subsystem::new("https", Status::Ok, format!("certificat de {domain} en cours d'obtention")),
    }
}

/// Ce que l'inscription remet au nouveau titulaire, une seule fois.
fn register_error(e: crate::register::RegisterError) -> ApiError {
    use crate::register::RegisterError as E;
    match e {
        E::Closed | E::Full | E::NoCaptain => ApiError::unavailable(e.to_string()),
        E::RateLimited(_) => ApiError::too_many(e.to_string()),
        E::Invalid(m) => ApiError::bad_request(m),
        E::Conflict(m) => ApiError::conflict(m),
        E::Internal(m) => {
            eprintln!("inscription : {m}");
            ApiError::internal("inscription impossible : voir les journaux de la station")
        }
    }
}

fn registered_json(reg: &crate::register::Registered) -> Value {
    let a = &reg.account;
    json!({
        "email": a.email, "token": reg.token, "pass": a.pass, "nsec": &*a.nsec,
        "npub": a.npub, "hex": a.hex, "g1pub": a.g1_ss58, "nostrns": a.nostrns, "constellation": reg.constellation, "lang": a.lang, "guest": reg.guest,
        "card": reg.card, "card_svg": astro_ui::qr::svg(&reg.card),
    })
}

fn invite_error(e: crate::invites::InviteError) -> ApiError {
    use crate::invites::InviteError as E;
    let code = e.code();
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound(m) => ApiError::not_found(m),
        E::Gone(code) => ApiError::gone(crate::invites::gone_message(code)),
        E::EmailTaken => ApiError::conflict("cette adresse a déjà un MULTIPASS : connectez-vous avec, puis rouvrez ce lien pour l'accepter avec votre compte"),
        E::Conflict(m) => ApiError::conflict(m),
        E::RateLimited(m) => ApiError::too_many(m),
        E::Unavailable(m) => ApiError::unavailable(m),
        E::Internal(m) => {
            eprintln!("invitations : {m}");
            ApiError::internal("opération impossible : voir les journaux de la station")
        }
    }
    .with_code(code)
}

fn link_error(e: crate::identity_links::LinkError) -> ApiError {
    use crate::identity_links::LinkError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound(m) => ApiError::not_found(m),
        E::Conflict(m) => ApiError::conflict(m),
        E::Internal(m) => {
            eprintln!("interface : annuaire : {m}");
            ApiError::internal("opération impossible : voir les journaux de la station")
        }
    }
}

fn account_error(e: crate::account::AccountError) -> ApiError {
    use crate::account::AccountError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound => ApiError::not_found("compte non hébergé ici"),
        E::Limited(m) => ApiError::too_many(m).with_code("rate_limited"),
        E::Locked(m) => ApiError::too_many(m).with_code("pass_locked"),
        E::Denied(m) => ApiError::denied(m).with_code("pass_denied"),
        E::Internal(m) => {
            eprintln!("interface : Mon compte : {m}");
            ApiError::internal("opération impossible : voir les journaux de la station")
        }
    }
}

fn admin_error(message: String) -> ApiError {
    if message.starts_with("section inconnue") { ApiError::not_found(message) } else { ApiError::bad_request(message) }
}

/// Corps d'un appel de billet qui engage l'identité du compte : `_who` est posé ici, jamais lu du client.
fn who_body(who: &Principal, body: Value) -> Value {
    let mut map = match body {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    map.insert("_who".into(), json!({ "hex": who.hex, "email": who.email, "captain": who.captain }));
    Value::Object(map)
}

fn billet_api_error(e: crate::billet::BilletError) -> ApiError {
    use crate::billet::BilletError as E;
    match e {
        E::Invalid(c, m) => ApiError::bad_request(m).with_code(c),
        E::Refused(c, m) => ApiError::forbidden(m).with_code(c),
        E::Limited(c, m) => ApiError::too_many(m).with_code(c),
        E::Unavailable(c, m) => ApiError::unavailable(m).with_code(c),
        // Défaillance de la chaîne : le message dit ce qui est arrivé au billet, il ne contient aucun secret.
        E::Failed(c, m) => {
            eprintln!("interface : billet : {m}");
            ApiError::unavailable(m).with_code(c)
        }
    }
}

fn ai_error(e: crate::member_ai::AiError) -> ApiError {
    match e {
        crate::member_ai::AiError::Invalid(m) => ApiError::bad_request(m),
        crate::member_ai::AiError::Internal(m) => {
            eprintln!("interface : paramètres IA : {m}");
            ApiError::internal("paramètres IA indisponibles")
        }
    }
}

fn book_error(e: crate::address_book::BookError) -> ApiError {
    use crate::address_book::BookError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound(m) => ApiError::not_found(m),
        E::Conflict(m) => ApiError::conflict(m),
        E::Internal(m) => {
            eprintln!("interface : carnet d'adresses : {m}");
            ApiError::internal("opération impossible : voir les journaux de la station")
        }
    }
}

/// Secondes depuis l'époque Unix.
fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use astro_ui::acme::AcmeStatus;

    use super::*;

    #[test]
    fn the_https_certificate_reaches_the_cockpit_with_its_cause() {
        const NOW: i64 = 1_800_000_000;
        const DAY: i64 = 86_400;
        let d = || vec!["station.exemple.org".to_owned()];
        let st = |err: Option<&str>, end: Option<i64>| Some((d(), AcmeStatus { issued: 0, not_after: end, last_error: err.map(str::to_owned) }));
        assert_eq!(tls_subsystem(None, NOW).status, Status::Disabled, "sans domaine : rien à surveiller");
        let first = tls_subsystem(st(Some("connexion refusée"), None), NOW);
        assert_eq!(first.status, Status::Down, "jamais obtenu : HTTPS ne marche pas");
        assert!(first.detail.contains("connexion refusée") && first.detail.contains("80 et 443") && first.detail.contains("station.exemple.org"), "{}", first.detail);
        assert_eq!(tls_subsystem(st(Some("délai"), Some(NOW + 20 * DAY)), NOW).status, Status::Degraded, "renouvellement raté, l'ancien vaut encore");
        // La cause de l'autorité devient un geste : box, DNS, quota, autre serveur.
        let hint = |e: &str| acme_hint(e, "station.exemple.org");
        assert!(hint("urn:ietf:params:acme:error:connection :: Timeout during connect (likely firewall problem)").contains("redirigez les ports 80 et 443"));
        assert!(hint("urn:ietf:params:acme:error:dns :: DNS problem: NXDOMAIN looking up A for station.exemple.org").contains("enregistrement A"));
        assert!(hint("urn:ietf:params:acme:error:rateLimited :: too many certificates already issued").contains("quota"));
        assert!(hint("urn:ietf:params:acme:error:unauthorized :: Invalid response from http://station.exemple.org/.well-known/acme-challenge/x: 404").contains("un autre serveur"));
        assert_eq!(tls_subsystem(st(None, Some(NOW + 3 * DAY)), NOW).status, Status::Degraded, "expire bientôt");
        assert_eq!(tls_subsystem(st(None, Some(NOW + 60 * DAY)), NOW).status, Status::Ok);
        assert_eq!(tls_subsystem(st(None, None), NOW).status, Status::Ok, "première demande en cours");
    }
}
