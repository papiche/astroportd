//! Implémentation du [`Backend`] de l'interface web : câble la base, WebDAV, FaceCloud, BRO, Duniter et le P2P.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use astro_bro::bus;
use astro_bro::{AnyEmbedder, MemoryStore, Meta, OllamaClient, Slot};
use astro_core::Amount;
use astro_dav::{BlobStore, DavState};
use astro_p2p::routing::haversine_km;
use astro_ui::api::{Applicant, AssignTarget, BookOp, ChatOp};
use astro_ui::{ApiError, Backend, Principal, Status, Subsystem};
use axum::http::{HeaderMap, Uri};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::station::StationServices;

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
    site: Option<Arc<crate::public_site::Site<B>>>,
    admin: Option<AdminFn>,
    console: Option<Arc<crate::console::Consoles>>,
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
    probe: Mutex<Option<(Instant, Result<(), String>)>>,
    planes_home: Option<std::path::PathBuf>,
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
        Self { services, dav, memory, ollama, faces_enabled, thumbs_fn: None, pending_fn: None, friends_fn: None, backup: None, replicate_fn: None, send_fn: None, coop: None, constellation: None, charter: None, billet: None, site: None, admin: None, console: None, account: None, contacts: None, invites: None, welcome: None, agents: None, pull: None, organize: None, board: astro_facecloud::board::BoardCache::new(), registrar: None, stations: None, feedback: None, oc: None, setup: None, conf_path: None, probe: Mutex::new(None), planes_home: None, ai: None, book: None, chat: None, people: None, ice: (None, None, Vec::new()), presence: None }
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

    /// Comment l'interface est servie maintenant : `https` (certificat de la station), `proxy` (reverse proxy) ou `local`.
    fn web_mode(&self) -> &'static str {
        match (self.services.tls_status().is_some(), self.services.info.domain.is_some()) {
            (true, _) => "https",
            (false, true) => "proxy",
            (false, false) => "local",
        }
    }

    /// Port TCP du maillage (le premier écouté hors circuit de relais).
    fn p2p_port(&self) -> Option<u16> {
        let addrs = self.services.p2p.as_ref()?.listen_addrs();
        addrs.iter().map(ToString::to_string).filter(|a| !a.contains("/p2p-circuit")).find_map(|a| a.split("/tcp/").nth(1)?.split('/').next()?.parse().ok())
    }

    /// Port UDP de la visio dans la configuration donnée (`None` : visio coupée, ou pas de domaine).
    fn visio_port(&self, proxy: bool, has_domain: bool) -> Option<u16> {
        self.ice.0?;
        has_domain.then_some(if proxy { crate::station_domain::UI_PORT } else { 443 })
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

    fn planes_home(&self) -> Result<&std::path::Path, ApiError> {
        self.planes_home.as_deref().ok_or_else(|| ApiError::unavailable("configuration des plans non disponible sur cette station"))
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

    fn welcome_service(&self) -> Result<&Arc<crate::welcome::Welcome<B>>, ApiError> {
        self.welcome.as_ref().ok_or_else(|| ApiError::unavailable("liens d'accueil non disponibles sur cette station"))
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

    fn organize_service(&self) -> Result<&Arc<crate::organize::OrganizeService<B>>, ApiError> {
        self.organize.as_ref().ok_or_else(|| ApiError::unavailable("rangement non disponible sur cette station"))
    }

    fn pull_service(&self) -> Result<&Arc<crate::webdav_pull::PullService<B>>, ApiError> {
        self.pull.as_ref().ok_or_else(|| ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station"))
    }

    fn agents_service(&self) -> Result<&Arc<crate::agents::Agents>, ApiError> {
        self.agents.as_ref().ok_or_else(|| ApiError::unavailable("agents non disponibles sur cette station"))
    }

    fn invites_service(&self) -> Result<&Arc<crate::invites::Invites<B>>, ApiError> {
        self.invites.as_ref().ok_or_else(|| ApiError::unavailable("invitations non disponibles sur cette station"))
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

    /// Active la console SSH de l'interface (aide acceptée par un ami).
    pub fn with_console(mut self, consoles: Arc<crate::console::Consoles>) -> Self {
        self.console = Some(consoles);
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

    /// Appel d'une opération de billet au nom d'un compte authentifié.
    async fn billet_call(&self, op: &'static str, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let f = self.billet.as_ref().ok_or_else(|| ApiError::unavailable("Ğ1Billets non activés sur cette station"))?;
        f(None, op, who_body(who, body)).await.map_err(billet_api_error)
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

    fn now() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
    }

    /// Ollama répond-il ? Résultat gardé 60 s.
    async fn ollama_probe(&self) -> Option<Result<(), String>> {
        let client = self.ollama.as_ref()?;
        if let Some((at, res)) = self.probe.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if at.elapsed() < PROBE_TTL {
                return Some(res.clone());
            }
        }
        let res = client.ping().await.map_err(|e| e.to_string());
        *self.probe.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), res.clone()));
        Some(res)
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

impl<B: BlobStore> Backend for StationUi<B> {
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

    async fn register_info(&self) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        let limits = r.limits();
        let accounts = r.hosted_accounts().await.map_err(|e| ApiError::internal(e.to_string()))?;
        let waiting = r.awaiting_captain().await;
        let open = limits.open && !waiting && (limits.max_accounts == 0 || accounts < limits.max_accounts);
        let mut out = json!({ "open": open, "constellation": r.constellation(), "accounts": accounts, "max_accounts": limits.max_accounts });
        if waiting {
            out["error"] = json!(crate::register::RegisterError::NoCaptain.to_string());
            out["captain_setup"] = json!(self.setup.is_some());
        }
        Ok(out)
    }

    async fn check_balance(&self, g1pub: &str) -> Result<Value, ApiError> {
        let b = self.services.balance_of_address(g1pub).await.map_err(|e| if e.starts_with("adresse") { ApiError::bad_request(e) } else if e.starts_with("trop de lectures") { ApiError::too_many(e) } else { ApiError::unavailable(e) })?;
        let dec = |cents: u64| format!("{}.{:02}", cents / 100, cents % 100);
        // Montants en entiers de centimes (`*_cents`) ; les chaînes décimales servent aux clients historiques, qui lisent `balance`.
        Ok(json!({ "g1pub": g1pub, "balance": dec(b.g1_cents), "g1_cents": b.g1_cents, "zen": dec(b.zen_cents), "zen_cents": b.zen_cents }))
    }

    async fn feedback(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        use crate::feedback::FeedbackError as E;
        let f = self.feedback.as_ref().ok_or_else(|| ApiError::unavailable("retours non activés sur cette station"))?;
        f.submit(&who.hex, &body).await.map_err(|e| match e {
            E::Invalid(m) => ApiError::bad_request(m),
            E::Limited(m) => ApiError::too_many(m).with_code("rate_limited"),
            E::Internal(m) => {
                eprintln!("interface : retour : {m}");
                ApiError::internal("retour non enregistré : voir les journaux de la station")
            }
        })
    }

    async fn account_oc(&self, who: &Principal) -> Result<Value, ApiError> {
        let oc = self.oc.as_ref().ok_or_else(|| ApiError::unavailable("vérification OpenCollective non activée sur cette station"))?;
        let email = who.email.as_deref().ok_or_else(|| ApiError::bad_request("ce compte n'a pas d'adresse e-mail"))?;
        Ok(oc.check(email).await)
    }

    fn public_origin(&self, host: &str) -> String {
        match &self.services.info.domain {
            Some(d) => format!("https://{d}"),
            None => format!("http://{host}"),
        }
    }

    async fn upload_image(&self, who: &Principal, data: Vec<u8>, host: String) -> Result<Value, ApiError> {
        use sha2::{Digest, Sha256};
        // Deux images décodées à la fois au plus : le décodage est du processeur, et la station a d'autres métiers.
        static BUSY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let email = who.email.clone().ok_or_else(|| ApiError::bad_request("ce compte n'a pas de dossier public : envoi impossible"))?;
        if self.site.is_none() {
            return Err(ApiError::unavailable("site public non activé sur cette station"));
        }
        let _permit = BUSY.try_acquire().map_err(|_| ApiError::too_many("des images sont en cours de traitement : réessayez dans un instant"))?;
        let original = hex::encode(Sha256::digest(&data));
        let prepared = tokio::task::spawn_blocking(move || crate::upload_image::normalize(&data)).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(ApiError::bad_request)?;
        let name = format!("{}.{}", &prepared.sha256[..32], prepared.ext);
        let size = prepared.data.len();
        astro_dav::ingest::replace(&self.dav, &who.hex, "/Public/uploads", &name, Some(prepared.mime), std::io::Cursor::new(prepared.data)).await.map_err(|e| match e {
            astro_dav::ingest::IngestError::TooLarge(m) => ApiError::bad_request(m),
            other => ApiError::internal(other.to_string()),
        })?;
        let url = format!("{}/site/{}/uploads/{name}", self.public_origin(&host), crate::public_site::name_of(&email));
        // Réponse NIP-96 (`nip94_event`), plus `url` à la racine : c'est ce que lisent les clients historiques.
        Ok(json!({
            "status": "success", "message": "Upload successful.", "url": url,
            "nip94_event": { "tags": [["url", url], ["ox", original], ["x", prepared.sha256], ["m", prepared.mime], ["size", size.to_string()]], "content": "" },
        }))
    }

    async fn attach(&self, who: &Principal, data: Vec<u8>, days: i64, origin: String) -> Result<Value, ApiError> {
        use astro_dav::share::{ShareError, store_attachment};
        if !self.dav.config.ipfs_gateway {
            return Err(ApiError::unavailable("passerelle /ipfs non activée sur cette station (--ipfs-gateway) : pièces jointes impossibles"));
        }
        let a = store_attachment(&self.dav.db, self.dav.blobs.as_ref(), &who.hex, &data, days * 86_400, Self::now()).await.map_err(|e| match e {
            ShareError::Refused(m) => ApiError::bad_request(m),
            other => {
                eprintln!("interface : pièce jointe impossible : {other}");
                ApiError::internal("dépôt de la pièce jointe impossible")
            }
        })?;
        if let Some(replicate) = self.replicate_fn.clone() {
            let (owner, cid, size, expires_at) = (who.hex.clone(), a.cid.clone(), a.size, a.expires_at);
            tokio::spawn(async move { replicate(owner, cid, size, expires_at).await });
        }
        // L'adresse est celle d'un contenu : n'importe quelle passerelle de la constellation qui l'a peut le servir à la même
        // adresse ; `gateway` n'est que celle où il se trouve aujourd'hui.
        Ok(json!({ "cid": a.cid, "size": a.size, "expires_at": a.expires_at, "gateway": origin, "url": format!("{origin}/ipfs/{}", a.cid) }))
    }

    async fn station_card(&self) -> Result<Value, ApiError> {
        let svc = &self.services;
        let keys = self.stations.as_ref().ok_or_else(|| ApiError::unavailable("carte de station non disponible"))?.public_keys().await.map_err(ApiError::internal)?;
        let captain: Option<String> = svc
            .db
            .readers()
            .read(|c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT hex FROM identities WHERE hosted = 1 AND role = 'captain' ORDER BY hex LIMIT 1", [], |r| r.get::<_, String>(0)))?))
            .await
            .map_err(db_err)?;
        // Seulement ce que la station publie déjà ailleurs (balise 30850, carte de maillage) : jamais d'e-mail, de secret ni de solde.
        let mut card = json!({
            "_source": "astroportd", "version": env!("CARGO_PKG_VERSION"), "hostname": crate::init::hostname(),
            "NODEHEX": svc.info.node_hex, "UPLANETG1PUB": keys.g1pub, "UPLANETNAME_G1": keys.name_g1,
        });
        if let Some(id) = &svc.info.peer_id {
            card["ipfsnodeid"] = json!(id);
        }
        if let Some(c) = captain {
            card["captainHEX"] = json!(c);
        }
        if let Some(d) = &svc.info.domain {
            card["myRELAY"] = json!(format!("wss://{d}"));
            card["uSPOT"] = json!(format!("https://{d}"));
            if self.dav.config.ipfs_gateway {
                card["myIPFS"] = json!(format!("https://{d}"));
            }
        }
        if let Some((lat, lon)) = svc.origin {
            card["STATION_LAT"] = json!(crate::beacon::umap_coord(lat));
            card["STATION_LON"] = json!(crate::beacon::umap_coord(lon));
        }
        Ok(card)
    }

    async fn uplanet_hex(&self, g1: bool) -> Result<String, ApiError> {
        let keys = self.stations.as_ref().ok_or_else(|| ApiError::unavailable("clé de l'UPlanet non disponible"))?.public_keys().await.map_err(ApiError::internal)?;
        Ok(if g1 { keys.g1hex.clone() } else { keys.hex.clone() })
    }

    async fn stations(&self) -> Result<Value, ApiError> {
        let s = self.stations.as_ref().ok_or_else(|| ApiError::unavailable("annuaire des stations non disponible"))?;
        s.list(u64::try_from(crate::private_backup::now()).unwrap_or(0)).await.map_err(ApiError::internal)
    }

    async fn register_captain(&self, ip: Option<std::net::IpAddr>, code: String, email: String, lang: String) -> Result<Value, ApiError> {
        let (Some(r), Some(setup)) = (self.registrar.as_ref(), self.setup.as_ref()) else { return Err(ApiError::unavailable("cette station n'attend pas de Capitaine")) };
        if !r.awaiting_captain().await {
            return Err(ApiError::conflict("cette station a déjà un Capitaine"));
        }
        if !setup.redeem(&code) {
            return Err(ApiError::forbidden("code d'installation incorrect : il est écrit dans le journal de la station (terminal ou journalctl)"));
        }
        r.register_captain(ip, &email, &lang).await.map(|reg| registered_json(&reg)).map_err(register_error)
    }

    async fn register(&self, ip: Option<std::net::IpAddr>, email: String, lang: String) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        r.register(ip, &email, &lang).await.map(|reg| registered_json(&reg)).map_err(register_error)
    }

    async fn register_guest(&self, ip: Option<std::net::IpAddr>, alias: Option<String>, lang: String) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        r.register_guest(ip, alias.as_deref(), &lang).await.map(|reg| registered_json(&reg)).map_err(register_error)
    }

    async fn restore(&self, ip: Option<std::net::IpAddr>, card: String, lang: String) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("restauration non disponible sur cette station"))?;
        // Carte imprimée seule : la part qu'un gardien a rendue (secours, 24 h) la complète, sans passer par le navigateur.
        let single = astro_lifecycle::share::SecretShare::parse(card.trim()).ok().filter(|s| s.index == 1);
        let rescued = match (&single, &self.backup) {
            (Some(s), Some(b)) => (b.rescue_share)(s.hex.clone(), false).await,
            _ => None,
        };
        let text = match &rescued {
            Some(p3) => format!("{}\n{p3}", card.trim()),
            None => card,
        };
        let card = astro_lifecycle::share::Recovery::parse(&text).map_err(|e| ApiError::bad_request(e.to_string()))?;
        let lang = if lang.is_empty() { "fr".to_owned() } else { lang };
        let reg = r.recover(ip, &card, &lang).await.map_err(register_error)?;
        if let (Some(_), Some(b)) = (&rescued, &self.backup) {
            (b.rescue_share)(card.card.hex.clone(), true).await;
        }
        let mut out = registered_json(&reg);
        out["restoring"] = json!(true);
        Ok(out)
    }

    async fn invite_create(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let req: crate::invites::CreateRequest = serde_json::from_value(body).map_err(|e| ApiError::bad_request(format!("demande illisible : {e}")))?;
        self.invites_service()?.create(&who.hex, req).await.map_err(invite_error)
    }

    async fn invites(&self, who: &Principal) -> Result<Value, ApiError> {
        self.invites_service()?.list(&who.hex).await.map_err(invite_error)
    }

    async fn invite_revoke(&self, who: &Principal, token: String) -> Result<Value, ApiError> {
        self.invites_service()?.revoke(&who.hex, &token).await.map_err(invite_error)
    }

    async fn invite_preview(&self, token: String) -> Result<Value, ApiError> {
        self.invites_service()?.preview(&token, false).await.map_err(invite_error)
    }

    async fn invite_preview_signed(&self, headers: &HeaderMap, method: &str, uri: &Uri, token: String) -> Result<Value, ApiError> {
        self.nostr_caller(headers, method, uri, &[]).await?;
        self.invites_service()?.preview(&token, true).await.map_err(invite_error)
    }

    async fn invite_thumb(&self, token: String, index: usize) -> Result<Vec<u8>, ApiError> {
        self.invites_service()?.thumb(&token, index).await.map_err(invite_error)
    }

    async fn invite_accept(&self, who: &Principal, token: String) -> Result<Value, ApiError> {
        let a = self.invites_service()?.accept(&token, &who.hex).await.map_err(invite_error)?;
        Ok(json!({ "from": a.from, "shared": a.shared, "warnings": a.warnings }))
    }

    async fn invite_accept_nostr(&self, headers: &HeaderMap, method: &str, uri: &Uri, token: String, body: Vec<u8>) -> Result<Value, ApiError> {
        let pubkey = self.nostr_caller(headers, method, uri, &body).await?;
        let svc = self.invites_service()?;
        let hosted = self.is_hosted(&pubkey).await;
        if hosted {
            let a = svc.accept(&token, &pubkey).await.map_err(invite_error)?;
            return Ok(json!({ "from": a.from, "shared": a.shared, "warnings": a.warnings }));
        }
        let proof: crate::invites::RemoteProof = serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("preuve illisible : {e}")))?;
        svc.accept_remote(&token, &pubkey, proof).await.map_err(invite_error)
    }

    async fn invite_deliver(&self, headers: &HeaderMap, method: &str, uri: &Uri, token: String, body: Vec<u8>) -> Result<Value, ApiError> {
        let pubkey = self.nostr_caller(headers, method, uri, &body).await?;
        self.invites_service()?.deliver_remote(&token, &pubkey).await.map_err(invite_error)
    }

    async fn invite_accept_elsewhere(&self, who: &Principal, origin: String, token: String) -> Result<Value, ApiError> {
        self.invites_service()?.accept_elsewhere(&who.hex, &origin, &token).await.map_err(invite_error)
    }

    async fn invite_claim(&self, ip: Option<std::net::IpAddr>, token: String, who: Applicant, lang: String) -> Result<Value, ApiError> {
        let svc = self.invites_service()?;
        let claimed = match who {
            Applicant::Email(email) => svc.claim(&token, ip, &email, &lang).await,
            Applicant::Guest(alias) => svc.claim_guest(&token, ip, alias.as_deref(), &lang).await,
        }
        .map_err(invite_error)?;
        let mut out = registered_json(&claimed.registered);
        out["from"] = json!(claimed.from);
        out["shared"] = json!(claimed.shared);
        out["warnings"] = json!(claimed.warnings);
        Ok(out)
    }

    async fn dashboard(&self, who: &Principal) -> Result<Value, ApiError> {
        let svc = &self.services;
        let owner = who.hex.clone();
        let (events, last_event, size, files, bytes, manifest) = svc
            .db
            .readers()
            .read(move |c| {
                let (events, last): (i64, i64) = c.query_row("SELECT COUNT(*), COALESCE(MAX(created_at), 0) FROM nostr_events", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let (files, bytes): (i64, i64) = c.query_row(
                    "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL",
                    [&owner],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let manifest: Option<(String, i64, String)> = c
                    .query_row("SELECT id, created_at, tags FROM nostr_events WHERE kind = 30078 AND pubkey = ?1 AND d_tag = ?2", params![owner, astro_dav::manifest::D_TAG], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .ok();
                Ok((events, last, db_bytes(c)?, files, bytes, manifest))
            })
            .await
            .map_err(db_err)?;
        let (m_at, m_event, m_rev) = match manifest {
            Some((id, at, tags)) => {
                let rev = serde_json::from_str::<Vec<Vec<String>>>(&tags).ok().and_then(|t| t.into_iter().find(|t| t.first().map(String::as_str) == Some("rev")).and_then(|t| t.get(1).cloned()));
                (Some(at), Some(id.chars().take(12).collect::<String>()), rev)
            }
            None => (None, None, None),
        };
        let (balance, weeks) = match svc.balance_quick(&who.hex).await {
            Ok((b, stale)) => (json!({ "g1_cents": b.g1_cents, "zen_cents": b.zen_cents, "error": null, "stale": stale }), (svc.weekly_rent_cents > 0).then(|| b.zen_cents as f64 / svc.weekly_rent_cents as f64)),
            Err(e) => (json!({ "g1_cents": null, "zen_cents": null, "error": e }), None),
        };
        let p2p = match &svc.p2p {
            Some(h) => json!({ "enabled": true, "peers": h.connected_peers().len(), "listen": h.listen_addrs().len(), "score": (svc.hardware.score() * 10.0).round() / 10.0, "tier": svc.hardware.tier().label() }),
            None => json!({ "enabled": false, "score": (svc.hardware.score() * 10.0).round() / 10.0, "tier": svc.hardware.tier().label() }),
        };
        Ok(json!({
            "station": { "node": svc.info.node_hex.chars().take(12).collect::<String>(), "peer_id": svc.info.peer_id, "domain": svc.info.domain, "version": env!("CARGO_PKG_VERSION"), "uptime_secs": svc.info.started.elapsed().as_secs(), "captain": who.captain },
            "relay": { "events": events, "last_event_at": (last_event > 0).then_some(last_event), "db_bytes": size },
            "p2p": p2p,
            "balance": balance,
            "energy": { "weeks": weeks, "max_weeks": GAUGE_WEEKS, "weekly_rent_cents": svc.weekly_rent_cents },
            "sovereignty": { "encrypted_files": files, "encrypted_bytes": bytes, "last_manifest_at": m_at, "last_manifest_event": m_event, "last_manifest_rev": m_rev },
        }))
    }

    async fn ucloud_list(&self, who: &Principal, path: String) -> Result<Value, ApiError> {
        let path = astro_dav::vfs::normalize_path(&path).map_err(|_| ApiError::bad_request("chemin invalide"))?;
        let (owner, p) = (who.hex.clone(), path.clone());
        let listing = self
            .services
            .db
            .readers()
            .read(move |c| {
                let Some(dir) = astro_dav::vfs::get_entry(c, &owner, &p).map_err(|_| astro_db::DbError::Aborted)? else { return Ok(None) };
                if dir.kind != astro_dav::vfs::EntryKind::Dir {
                    return Ok(Some(Vec::new()));
                }
                Ok(Some(astro_dav::vfs::list_children(c, &owner, &p).map_err(|_| astro_db::DbError::Aborted)?))
            })
            .await
            .map_err(db_err)?;
        let Some(entries) = listing else { return Err(ApiError::not_found("dossier introuvable")) };
        let mut entries: Vec<Value> = entries.iter().map(entry_json).collect();
        // Dossiers d'abord, puis par nom.
        entries.sort_by(|a, b| (a["type"] != "dir", a["name"].as_str()).cmp(&(b["type"] != "dir", b["name"].as_str())));
        Ok(json!({ "path": path, "dav": self.dav.config.mount, "entries": entries }))
    }

    async fn ucloud_photos(&self, who: &Principal, before: Option<i64>, limit: usize) -> Result<Value, ApiError> {
        let owner = who.hex.clone();
        let limit_i = i64::try_from(limit).unwrap_or(60);
        let (items, next) = self
            .services
            .db
            .readers()
            .read(move |c| {
                const COLS: &str = "path, mime, size, mtime, taken_at, COALESCE(taken_at, mtime) AS t, json_extract(data, '$.thumb')";
                const KIND: &str = "owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL AND (mime LIKE 'image/%' OR mime LIKE 'video/%')";
                let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(i64, Value)> {
                    let t: i64 = r.get(5)?;
                    Ok((t, json!({ "path": r.get::<_, String>(0)?, "mime": r.get::<_, Option<String>>(1)?, "size": r.get::<_, Option<i64>>(2)?, "mtime": r.get::<_, Option<i64>>(3)?, "taken_at": r.get::<_, Option<i64>>(4)?, "thumb": thumb_of(r.get::<_, Option<String>>(6)?) })))
                };
                let mut stmt = c.prepare_cached(&format!("SELECT {COLS} FROM ucloud_entries WHERE {KIND} AND (?2 IS NULL OR COALESCE(taken_at, mtime) < ?2) ORDER BY t DESC, path LIMIT ?3"))?;
                let mut rows = stmt.query_map(params![owner, before, limit_i], map)?.collect::<Result<Vec<_>, _>>()?;
                let mut next = None;
                if rows.len() as i64 == limit_i {
                    // Page pleine : on y ajoute les photos de la même seconde que la dernière (ordre par chemin),
                    // sinon la page suivante (`t < dernier`) les sauterait.
                    if let Some((t, last)) = rows.last().cloned() {
                        let last_path = last["path"].as_str().unwrap_or("").to_owned();
                        let mut tie = c.prepare_cached(&format!("SELECT {COLS} FROM ucloud_entries WHERE {KIND} AND COALESCE(taken_at, mtime) = ?2 AND path > ?3 ORDER BY path"))?;
                        rows.extend(tie.query_map(params![owner, t, last_path], map)?.collect::<Result<Vec<_>, _>>()?);
                        next = Some(t);
                    }
                }
                Ok((rows.into_iter().map(|r| r.1).collect::<Vec<_>>(), next))
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "dav": self.dav.config.mount, "items": items, "next": next }))
    }

    async fn library(&self, who: &Principal, q: astro_ui::api::LibraryQuery) -> Result<Value, ApiError> {
        use astro_ui::api::LibraryQuery as Q;
        let owner = who.hex.clone();
        let now = now_secs();
        self.services
            .db
            .readers()
            .read(move |c| {
                Ok(match q {
                    Q::Have(shas) => Ok(json!({ "have": crate::library::have(c, &owner, &shas)? })),
                    Q::Memories(day) => {
                        if let Some((md, y)) = day {
                            return Ok(Ok(json!({ "memories": crate::library::memories(c, &owner, &md, y)? })));
                        }
                        let (y, md): (i64, String) = c.query_row("SELECT CAST(strftime('%Y', ?1, 'unixepoch') AS INTEGER), strftime('%m-%d', ?1, 'unixepoch')", [now], |r| Ok((r.get(0)?, r.get(1)?)))?;
                        Ok(json!({ "memories": crate::library::memories(c, &owner, &md, y)? }))
                    }
                    Q::Timeline(None) => Ok(json!({ "months": crate::library::timeline_buckets(c, &owner)? })),
                    Q::Timeline(Some(ym)) => crate::library::timeline_month(c, &owner, &ym)?.map(|items| json!({ "ym": ym, "items": items })).ok_or_else(|| ApiError::bad_request("mois attendu au format AAAA-MM")),
                    Q::Search(s) => {
                        let bbox = match s.bbox.as_deref() {
                            None => None,
                            Some(b) => {
                                let v: Vec<f64> = b.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                                match v[..] {
                                    [s, w, n, e] if (-90.0..=90.0).contains(&s) && (-90.0..=90.0).contains(&n) && s <= n && (-180.0..=180.0).contains(&w) && (-180.0..=180.0).contains(&e) => Some((s, w, n, e)),
                                    _ => return Ok(Err(ApiError::bad_request("zone attendue : sud,ouest,nord,est en degrés"))),
                                }
                            }
                        };
                        let after = s.after_t.zip(s.after_p.clone());
                        let query = crate::library::Search { q: s.q, person: s.person, from: s.from, to: s.to, bbox, kind: s.kind, after, limit: s.limit.unwrap_or(60) };
                        Ok(crate::library::search(c, &owner, &query)?)
                    }
                    Q::Map(m) => {
                        let bbox = m.bbox.as_deref().and_then(|b| {
                            let v: Vec<f64> = b.split(',').filter_map(|x| x.parse().ok()).collect();
                            (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
                        });
                        let area = crate::library::MapArea { from: m.from, to: m.to, bbox, cell: m.cell.unwrap_or(5.0), items: m.items == Some(1) };
                        Ok(crate::library::map(c, &owner, &area)?)
                    }
                    Q::Duplicates(all) => Ok(crate::library::duplicates(c, &owner, all)?),
                })
            })
            .await
            .map_err(db_err)?
    }

    async fn ucloud_share(&self, who: &Principal, paths: Vec<String>, ttl_secs: i64, origin: String) -> Result<Value, ApiError> {
        use astro_dav::share::{ShareError, create_guest_link};
        let title = who.email.clone().unwrap_or_else(|| "uCloud".into());
        let link = create_guest_link(&self.dav.db, self.dav.blobs.as_ref(), &who.hex, &paths, &title, ttl_secs, Self::now()).await.map_err(|e| match e {
            ShareError::Nothing(p) => ApiError::not_found(format!("rien à partager : {p}")),
            ShareError::TooMany => ApiError::bad_request("trop de fichiers pour un seul lien"),
            other => {
                eprintln!("interface : partage impossible : {other}");
                ApiError::internal("création du lien impossible")
            }
        })?;
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "ucloud", "ui:share", format!("lien magique créé ({} fichier(s))", link.items), None);
        Ok(json!({ "url": format!("{origin}{}#{}", link.path, link.key_hex), "expires_at": link.expires_at, "items": link.items }))
    }

    async fn faces(&self, who: &Principal) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Ok(json!({ "enabled": false, "dav": self.dav.config.mount, "people": [], "unknown": 0, "suggestions": [] }));
        }
        let owner = who.hex.clone();
        let (people, unknown, suggestions) = self
            .services
            .db
            .readers()
            .read(move |c| {
                let bbox = |r: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<Value> { Ok(json!([r.get::<_, f64>(i)?, r.get::<_, f64>(i + 1)?, r.get::<_, f64>(i + 2)?, r.get::<_, f64>(i + 3)?])) };
                // La ligne de meilleur score de détection représente la personne (colonnes nues de MAX()).
                let mut p = c.prepare_cached(
                    "SELECT name, COUNT(DISTINCT cid), face_id, ucloud_path, bbox_x1, bbox_y1, bbox_x2, bbox_y2, MAX(det_score), \
                            (SELECT json_extract(e.data, '$.thumb') FROM ucloud_entries e WHERE e.owner_hex = faces.owner_hex AND e.path = faces.ucloud_path) FROM faces \
                     WHERE owner_hex = ?1 AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL AND name NOT LIKE 'Inconnu\\_%' ESCAPE '\\' GROUP BY coalesce('fiche:' || contact_uid, 'nom:' || name) ORDER BY COUNT(DISTINCT cid) DESC, name LIMIT 200",
                )?;
                let people = p
                    .query_map([&owner], |r| Ok(json!({ "name": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?, "sample": { "face_id": r.get::<_, i64>(2)?, "name": r.get::<_, String>(0)?, "path": r.get::<_, String>(3)?, "bbox": bbox(r, 4)?, "thumb": thumb_of(r.get::<_, Option<String>>(9)?) } })))?
                    .collect::<Result<Vec<_>, _>>()?;
                let unknown: i64 = c.query_row("SELECT COUNT(*) FROM faces WHERE owner_hex = ?1 AND name LIKE 'Inconnu\\_%' ESCAPE '\\' AND suggest_name IS NULL", [&owner], |r| r.get(0))?;
                let mut s = c.prepare_cached(
                    "SELECT face_id, ucloud_path, bbox_x1, bbox_y1, bbox_x2, bbox_y2, suggest_name, suggest_score, \
                            (SELECT json_extract(e.data, '$.thumb') FROM ucloud_entries e WHERE e.owner_hex = faces.owner_hex AND e.path = faces.ucloud_path) FROM faces \
                     WHERE owner_hex = ?1 AND suggest_name IS NOT NULL AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL ORDER BY suggest_score DESC LIMIT 50",
                )?;
                let suggestions = s
                    .query_map([&owner], |r| Ok(json!({ "face_id": r.get::<_, i64>(0)?, "path": r.get::<_, String>(1)?, "bbox": bbox(r, 2)?, "suggest_name": r.get::<_, String>(6)?, "score": r.get::<_, Option<f64>>(7)?.unwrap_or(0.0), "thumb": thumb_of(r.get::<_, Option<String>>(8)?) })))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((people, unknown, suggestions))
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "enabled": true, "dav": self.dav.config.mount, "people": people, "unknown": unknown, "suggestions": suggestions }))
    }

    async fn face_accept(&self, who: &Principal, face_id: i64) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        match astro_facecloud::review::accept_suggestion(&self.services.db, &who.hex, face_id).await {
            Ok(true) => {
                self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", "suggestion validée", None);
                Ok(json!({ "accepted": true }))
            }
            Ok(false) => Err(ApiError::not_found("aucune suggestion à valider pour ce visage")),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn face_name(&self, who: &Principal, face_id: i64, name: String) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        // Même chemin que la table : la fiche de la personne d'abord, le visage s'y relie par son UID.
        let r = self.faces_assign(who, vec![face_id], AssignTarget { name, pubkey: None, contact: None, new: false }).await?;
        if r["assigned"] == 0 {
            return Err(ApiError::not_found("visage introuvable"));
        }
        Ok(json!({ "named": r["name"] }))
    }

    async fn devices_list(&self, who: &Principal) -> Result<Value, ApiError> {
        let list = astro_dav::devices::list(&self.services.db, &who.hex).await.map_err(device_err)?;
        Ok(json!({ "devices": list.iter().map(|d| json!({ "id": d.id, "label": d.label, "root": d.root, "mode": d.mode.as_str(), "created_at": d.created_at, "last_used": d.last_used })).collect::<Vec<_>>(), "max": astro_dav::devices::MAX_DEVICES }))
    }

    async fn devices_add(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let text = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let mode = astro_dav::devices::Mode::parse(&text("mode")).ok_or_else(|| ApiError::bad_request("mode attendu : write, readwrite ou read"))?;
        let (d, token) = astro_dav::devices::create(&self.services.db, &who.hex, &text("label"), &text("root"), mode).await.map_err(device_err)?;
        self.dav.publisher.as_ref().inspect(|p| p.notify(&who.hex));
        Ok(json!({ "id": d.id, "label": d.label, "root": d.root, "mode": d.mode.as_str(), "token": token, "note": "Ce jeton ne sera plus jamais affiché : copiez-le maintenant dans l'application de l'appareil." }))
    }

    async fn devices_revoke(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        astro_dav::devices::revoke(&self.services.db, &who.hex, &id).await.map_err(device_err)?;
        Ok(json!({ "revoked": id }))
    }

    async fn organize_status(&self, who: &Principal) -> Result<Value, ApiError> {
        self.organize_service()?.status(&who.hex).await.map_err(organize_err)
    }

    async fn organize_preview(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.organize_service()?.preview(&who.hex, &organize_options(body)?).await.map_err(organize_err)
    }

    async fn organize_run(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.organize_service()?.run(&who.hex, organize_options(body)?).await.map_err(organize_err)?;
        Ok(json!({ "started": true }))
    }

    async fn organize_stop(&self, who: &Principal) -> Result<Value, ApiError> {
        self.organize_service()?.stop(&who.hex).map_err(organize_err)?;
        Ok(json!({ "stopping": true }))
    }

    async fn organize_undo(&self, who: &Principal, run: String) -> Result<Value, ApiError> {
        let n = self.organize_service()?.undo(&who.hex, &run).await.map_err(organize_err)?;
        Ok(json!({ "restored": n }))
    }

    async fn pull_list(&self, who: &Principal) -> Result<Value, ApiError> {
        self.pull_service()?.list(&who.hex).await.map_err(pull_err)
    }

    async fn pull_add(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let draft: crate::webdav_pull::Draft = serde_json::from_value(body).map_err(|_| ApiError::bad_request("adresse, identifiant et mot de passe attendus"))?;
        self.pull_service()?.add(&who.hex, draft).await.map_err(pull_err)
    }

    async fn pull_remove(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        self.pull_service()?.remove(&who.hex, &id).await.map_err(pull_err)?;
        Ok(json!({ "removed": id }))
    }

    async fn pull_run(&self, who: &Principal, id: String, dry_run: bool) -> Result<Value, ApiError> {
        self.pull_service()?.run(&who.hex, &id, dry_run).await.map_err(pull_err)?;
        Ok(json!({ "started": id, "dry_run": dry_run }))
    }

    async fn pull_stop(&self, who: &Principal) -> Result<Value, ApiError> {
        self.pull_service()?.stop(&who.hex).map_err(pull_err)?;
        Ok(json!({ "stopping": true }))
    }

    async fn agents_list(&self, who: &Principal) -> Result<Value, ApiError> {
        self.agents_service()?.list(&who.hex).await.map_err(agent_err)
    }

    async fn agents_enroll(&self, who: &Principal, name: String, group: String, max_secs: u64, totp: bool) -> Result<Value, ApiError> {
        self.agents_service()?.enroll(&who.hex, &name, &group, max_secs, totp).await.map_err(agent_err)
    }

    async fn agents_revoke(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        self.agents_service()?.revoke(&who.hex, &id).await.map_err(agent_err)
    }

    async fn agents_requests(&self, who: &Principal) -> Result<Value, ApiError> {
        Ok(self.agents_service()?.requests(&who.hex))
    }

    async fn agents_decide(&self, who: &Principal, id: String, approve: bool, secs: Option<u64>) -> Result<Value, ApiError> {
        self.agents_service()?.decide(&who.hex, &id, approve, secs).await.map_err(agent_err)
    }

    async fn mcp(&self, bearer: Option<String>, body: Value) -> Result<Option<Value>, ApiError> {
        self.agents_service()?.mcp(bearer.as_deref(), &body).await.map_err(agent_err)
    }

    async fn footprint(&self, who: &Principal) -> Result<Value, ApiError> {
        let svc = &self.services;
        let account = astro_dav::auth::lookup_by_hex(&svc.db, &who.hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        let limits = self.dav.config.limits_for(&account);
        let unlimited = limits.quota_total == u64::MAX;
        // Sans quota (Capitaine), la mesure utile est le disque de la station.
        let quota = if unlimited { self.dav.blobs.disk_space().map_or(0, |(_, total)| total) } else { limits.quota_total };
        let owner = who.hex.clone();
        let (opex, capex) = self.opex_capex(&who.hex, &account).await?;
        let hosting = {
            let h = who.hex.clone();
            svc.db.readers().read(move |c| Ok(json!({ "weekly_zen": crate::hosting_fee::weekly_price(c, &h)?, "lifecycle": crate::hosting_fee::lifecycle_summary(c, &h) }))).await.map_err(db_err)?
        };
        let (used, files) = svc
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row("SELECT COALESCE(SUM(size), 0), COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", [&owner], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?)
            })
            .await
            .map_err(db_err)?;
        Ok(json!({
            "disk": { "used_bytes": used, "files": files, "quota_bytes": quota, "unlimited": unlimited, "tier": format!("{:?}", account.tier) },
            "opex": opex, "capex": capex,
            "hosting": hosting,
        }))
    }

    async fn faces_board(&self, who: &Principal, dir: Option<String>) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Ok(json!({ "enabled": false, "faces": [], "people": [], "unknown": 0 }));
        }
        let b = self.board.board(&self.services.db, &who.hex, dir.as_deref()).await.map_err(db_err)?;
        let faces: Vec<Value> = b
            .faces
            .iter()
            .map(|f| {
                json!({
                    "id": f.face_id, "name": f.name, "pubkey": f.pubkey, "contact": f.contact, "named": f.named, "path": f.path, "bbox": f.bbox,
                    "x": (f64::from(f.x) * 10_000.0).round() / 10_000.0, "y": (f64::from(f.y) * 10_000.0).round() / 10_000.0,
                    "group_id": f.group.as_ref().map(|g| g.0.clone()), "group_size": f.group.as_ref().map(|g| g.1),
                    "suggest": f.suggest.as_ref().map(|(name, score)| json!({ "name": name, "score": score, "contact": f.suggest_contact })),
                })
            })
            .collect();
        let people: Vec<Value> = b.people.iter().map(|p| json!({ "name": p.name, "contact": p.contact, "pubkey": p.pubkey, "faces": p.faces, "photos": p.photos })).collect();
        Ok(json!({ "enabled": true, "dav": self.dav.config.mount, "faces": faces, "people": people, "unknown": b.unknown, "groups_skipped": b.groups_skipped, "truncated": b.truncated }))
    }

    async fn faces_thumbs(&self, who: &Principal, ids: Vec<i64>, size: u32) -> Result<Value, ApiError> {
        let f = self.thumbs_fn.as_ref().ok_or_else(|| ApiError::unavailable("Les vignettes de visages ne sont pas disponibles : la reconnaissance des visages n'est pas activée."))?;
        let (thumbs, failed) = f(who.hex.clone(), ids, size).await.map_err(|e| {
            eprintln!("interface : vignettes de visages : {e}");
            ApiError::internal("vignettes indisponibles")
        })?;
        let map: serde_json::Map<String, Value> = thumbs.into_iter().map(|(id, url)| (id.to_string(), Value::String(url))).collect();
        Ok(json!({ "thumbs": map, "failed": failed.into_iter().map(|(id, why)| json!([id, why])).collect::<Vec<_>>() }))
    }

    async fn faces_assign(&self, who: &Principal, ids: Vec<i64>, target: AssignTarget) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        // La fiche d'abord : son UID relie les visages (jamais le nom) et son nom fait foi. Un nom ambigu est refusé avec le
        // nombre de fiches en cause : l'interface fait choisir, elle ne devine pas.
        let contact = match &self.book {
            Some(book) => Some(book.person_card(&who.hex, &target.name, target.contact.as_deref(), target.new).await.map_err(book_error)?),
            None => None,
        };
        let uid = contact.as_ref().and_then(|c| c["uid"].as_str()).map(str::to_owned);
        let name = contact.as_ref().and_then(|c| c["name"].as_str()).map_or_else(|| target.name.clone(), str::to_owned);
        match astro_facecloud::review::name_faces(&self.services.db, &who.hex, &ids, uid.as_deref(), target.pubkey.as_deref(), &name).await {
            Ok(changed) => {
                self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", format!("{changed} visage(s) rangé(s) dans « {name} »"), None);
                // La fiche reliée donne aussitôt sa clé Nostr aux visages (et partage leurs photos, amis réciproques).
                if let Some(links) = &self.contacts {
                    let (links, owner) = (links.clone(), who.hex.clone());
                    tokio::spawn(async move {
                        if let Err(e) = links.reconcile(&owner).await {
                            eprintln!("annuaire : rapprochement impossible : {e:?}");
                        }
                    });
                }
                Ok(json!({ "assigned": changed, "name": name, "contact": contact }))
            }
            Err(astro_facecloud::FaceError::Invalid(m)) => Err(ApiError::bad_request(m)),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn face_shares_pending(&self, who: &Principal) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        let batches = astro_facecloud::review::pending_batches(&self.services.db, &who.hex).await.map_err(db_err)?;
        let pending: Vec<Value> = batches.into_iter().map(|b| json!({ "recipient_hex": b.recipient_hex, "name": b.name, "count": b.count, "sample_path": b.sample_path })).collect();
        Ok(json!({ "pending": pending }))
    }

    async fn face_shares_confirm(&self, who: &Principal, recipient: String, accept: bool) -> Result<Value, ApiError> {
        let f = self.pending_fn.as_ref().ok_or_else(|| ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station."))?;
        let (sent, dropped) = f(who.hex.clone(), recipient, accept).await.map_err(|e| {
            eprintln!("interface : envois de photos en attente : {e}");
            ApiError::internal("envoi impossible, réessayez plus tard")
        })?;
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", if accept { format!("{sent} photo(s) partagée(s) après confirmation") } else { "partage de photos refusé".to_owned() }, None);
        Ok(json!({ "sent": sent, "dropped": dropped }))
    }

    async fn faces_forget(&self, who: &Principal, ids: Vec<i64>) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        match astro_facecloud::review::forget_faces(&self.services.db, &who.hex, &ids).await {
            Ok(gone) => {
                self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", format!("{gone} visage(s) oublié(s)"), None);
                Ok(json!({ "forgotten": gone }))
            }
            Err(astro_facecloud::FaceError::Invalid(m)) => Err(ApiError::bad_request(m)),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn rotate_photos(&self, who: &Principal, paths: Vec<String>, turn: i8) -> Result<Value, ApiError> {
        let (mut rotated, mut failed) = (0, Vec::new());
        for p in paths {
            match crate::rotate::rotate_entry(&self.dav, &who.hex, &p, turn).await {
                Ok(()) => rotated += 1,
                Err(error) => failed.push(json!({ "path": p, "error": error })),
            }
        }
        Ok(json!({ "rotated": rotated, "failed": failed }))
    }

    async fn book(&self, who: &Principal, op: BookOp) -> Result<Value, ApiError> {
        let book = self.book.as_ref().ok_or_else(|| ApiError::unavailable("le carnet d'adresses n'est pas disponible sur cette station"))?;
        let o = who.hex.as_str();
        let fields = |v: Value| serde_json::from_value::<astro_dav::vcard::Fields>(v).map_err(|e| ApiError::bad_request(format!("champs de la fiche invalides : {e}")));
        let out = match op {
            BookOp::List(q, group) => book.list(o, &q, group.as_deref()).await,
            BookOp::Get(path) => match book.get(o, &path).await {
                Ok(mut card) => {
                    self.add_keys(&mut card).await;
                    Ok(card)
                }
                e => e,
            },
            BookOp::Save(path, f) => book.save(o, path.as_deref(), &fields(f)?).await,
            BookOp::Delete(path) => book.delete(o, &path).await,
            BookOp::DeleteMany(paths) => book.delete_many(o, &paths).await,
            BookOp::Photo(path) => book.photo(o, &path).await,
            BookOp::PhotoChoices(path) => book.photo_choices(o, &path).await,
            BookOp::SetPhoto(path, uri) => book.set_photo(o, &path, uri.as_deref()).await,
            BookOp::Take(path, keys) => book.take(o, &path, &keys).await,
            BookOp::Group(from, to) => book.set_group(o, &from, to.as_deref()).await,
            BookOp::Import(vcf) => book.import(o, &vcf).await,
            BookOp::Duplicates => book.duplicates(o).await,
            BookOp::Merge(keep, drop) => book.merge(o, &keep, &drop).await,
            BookOp::MergeExact(apply, limit) => book.merge_exact(o, apply, limit).await,
            BookOp::Me => book.me(o).await,
            BookOp::SaveMe(f) => book.save_me(o, &fields(f)?).await,
        };
        out.map_err(book_error)
    }

    async fn my_links(&self, who: &Principal) -> Result<Value, ApiError> {
        let people = self.people.as_ref().ok_or_else(|| ApiError::unavailable("annuaire des UPlanet non disponible sur cette station"))?;
        let keys = people.class_of(&who.hex).await;
        Ok(json!({ "plane": people.plane(), "siblings": people.siblings().iter().map(|s| &s.name).collect::<Vec<_>>(), "keys": keys }))
    }

    async fn announce_links(&self, who: &Principal) -> Result<Value, ApiError> {
        let people = self.people.as_ref().ok_or_else(|| ApiError::unavailable("annuaire des UPlanet non disponible sur cette station"))?;
        let email = who.email.as_deref().ok_or_else(|| ApiError::bad_request("ce compte n'a pas d'email : rien à relier"))?;
        people.announce(&who.hex, email).await.map_err(ApiError::bad_request)
    }

    async fn presence(&self, who: &Principal, body: Option<Value>) -> Result<Value, ApiError> {
        let p = self.presence.as_ref().ok_or_else(|| ApiError::unavailable("la présence visio n'est pas disponible sur cette station"))?;
        if let Some(b) = body {
            if let Some(on) = b["reachable"].as_bool() {
                p.set_reachable(&who.hex, on).await.map_err(ApiError::unavailable)?;
            }
            if let Some(on) = b["in_call"].as_bool() {
                p.set_in_call(&who.hex, on).await;
            }
        }
        Ok(p.view(&who.hex).await)
    }

    async fn call_ice(&self, who: &Principal) -> Result<Value, ApiError> {
        // Les identifiants du relais sont fabriqués pour ce membre seul (l'API est authentifiée) ; ses amis N¹ reçoivent les leurs
        // dans l'offre d'appel.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let turn = self.ice.1.as_ref().map(|s| crate::turn::credentials(s, &who.hex[..16.min(who.hex.len())], now, crate::turn::CREDENTIAL_TTL)).map(|(u, c)| json!({ "username": u, "credential": c }));
        Ok(json!({ "stun_port": self.ice.0, "turn": turn, "servers": self.ice.2 }))
    }

    async fn chat(&self, who: &Principal, op: ChatOp) -> Result<Value, ApiError> {
        let chat = self.chat.as_ref().ok_or_else(|| ApiError::unavailable("le chat privé n'est pas disponible sur cette station"))?;
        let o = who.hex.as_str();
        let out = match op {
            ChatOp::List => chat.conversations(o).await,
            ChatOp::Messages(peer, before) => chat.messages(o, &peer, before).await,
            ChatOp::Send(peer, text) => chat.send(o, &peer, &text).await,
            ChatOp::Read(peer) => chat.mark_read(o, &peer).await,
            ChatOp::Signal(sig) => chat.signal(o, sig["peer"].as_str().unwrap_or_default(), &sig).await,
        };
        out.map_err(|e| match e {
            crate::chat::ChatError::Invalid(m) => ApiError::bad_request(m),
            crate::chat::ChatError::NoKey => ApiError::unavailable("clé du compte indisponible sur cette station : envoi impossible"),
            crate::chat::ChatError::Unreachable(m) => ApiError::unavailable(m),
            crate::chat::ChatError::Db(m) => {
                eprintln!("interface : chat : {m}");
                ApiError::internal("opération impossible : voir les journaux de la station")
            }
        })
    }

    async fn ai_declared(&self, who: &Principal) -> bool {
        match &self.ai {
            Some(ai) => ai.declared(&who.hex).await,
            None => false,
        }
    }

    async fn ai_params(&self, who: &Principal) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.get(&who.hex).await.map(|p| p.view()).map_err(ai_error)
    }

    async fn set_ai_params(&self, who: &Principal, patch: Value) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.set(&who.hex, &patch).await.map(|p| p.view()).map_err(ai_error)
    }

    async fn planes(&self, _who: &Principal) -> Result<Value, ApiError> {
        use crate::plane;
        let home = self.planes_home()?;
        let path = plane::default_path(home);
        let (file, existed) = plane::load(&path, home).map_err(|e| ApiError::internal(e.to_string()))?;
        let info = plane::describe(&file, home).map_err(|e| ApiError::internal(e.to_string()))?;
        let multi = !plane::running().is_empty();
        let pending: Vec<&str> = info.iter().filter(|p| !p.running).map(|p| p.name.as_str()).collect();
        Ok(json!({ "configured": existed, "multi_process": multi, "front": plane::front(), "planes": info, "restart_needed": !pending.is_empty(), "pending": pending, "path": path.display().to_string() }))
    }

    async fn plane_add(&self, _who: &Principal, name: String, swarm_key: Option<String>) -> Result<Value, ApiError> {
        use crate::plane;
        let home = self.planes_home()?;
        let path = plane::default_path(home);
        let (mut file, _) = plane::load(&path, home).map_err(|e| ApiError::internal(e.to_string()))?;
        let added = plane::add(&mut file, &plane::NewPlane { name: name.clone(), swarm_key, ..plane::NewPlane::default() }, home).map_err(|e| ApiError::bad_request(e.to_string()))?;
        plane::save(&path, &file).map_err(|e| ApiError::internal(format!("planes.json non écrit : {e}")))?;
        let node_game = file.game_dir.as_deref().map_or_else(|| home.join(".zen/game"), |g| g.strip_prefix("~/").map_or_else(|| std::path::PathBuf::from(g), |r| home.join(r)));
        if let Ok(planes) = plane::plan(&file, home) {
            if let Some(p) = planes.iter().find(|p| p.name == name.trim()) {
                plane::prepare(p, &node_game).map_err(|e| ApiError::internal(format!("dossier du plan : {e}")))?;
            }
        }
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:planes", format!("plan « {} » ajouté (redémarrage de `astroportd planes` nécessaire)", name.trim()), None);
        Ok(json!({ "name": name.trim(), "http": added.http_listen, "p2p": added.p2p_listen, "swarm_key_path": added.swarm_key_path.display().to_string(), "created_key": added.created_key }))
    }

    async fn plane_remove(&self, _who: &Principal, name: String) -> Result<Value, ApiError> {
        use crate::plane;
        let home = self.planes_home()?;
        let path = plane::default_path(home);
        let (mut file, existed) = plane::load(&path, home).map_err(|e| ApiError::internal(e.to_string()))?;
        if !existed {
            return Err(ApiError::bad_request("aucun planes.json : rien à retirer"));
        }
        plane::remove(&mut file, name.trim()).map_err(|e| ApiError::bad_request(e.to_string()))?;
        plane::save(&path, &file).map_err(|e| ApiError::internal(format!("planes.json non écrit : {e}")))?;
        Ok(json!({ "removed": name.trim() }))
    }

    async fn faces_folders(&self, who: &Principal) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Ok(json!({ "folders": [] }));
        }
        let owner = who.hex.clone();
        let rows: Vec<(String, i64, i64)> = self
            .services
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT e.parent, COUNT(*), COALESCE(SUM(f.name LIKE 'Inconnu%' AND f.pubkey IS NULL), 0) FROM faces f \
                     JOIN ucloud_entries e ON e.owner_hex = f.owner_hex AND e.path = f.ucloud_path \
                     WHERE f.owner_hex = ?1 AND f.ref IS NULL AND f.bbox_x1 IS NOT NULL GROUP BY e.parent ORDER BY e.parent DESC LIMIT 2000",
                )?;
                Ok(stmt.query_map([&owner], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "folders": rows.into_iter().map(|(dir, faces, todo)| json!({ "dir": dir, "faces": faces, "todo": todo })).collect::<Vec<_>>() }))
    }

    async fn faces_delete_photos(&self, who: &Principal, ids: Vec<i64>) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        // Les photos où apparaissent ces visages (les visages de référence des contacts n'ont pas de photo à nous).
        let (owner, wanted) = (who.hex.clone(), ids);
        let paths: Vec<String> = self
            .services
            .db
            .readers()
            .read(move |c| {
                let mut out = std::collections::BTreeSet::new();
                let mut stmt = c.prepare("SELECT ucloud_path FROM faces WHERE owner_hex = ?1 AND face_id = ?2 AND ref IS NULL AND ucloud_path IS NOT NULL")?;
                for id in wanted {
                    if let Ok(p) = stmt.query_row(rusqlite::params![owner, id], |r| r.get::<_, String>(0)) {
                        out.insert(p);
                    }
                }
                Ok(out.into_iter().collect())
            })
            .await
            .map_err(db_err)?;
        let (mut deleted, mut missing, mut bytes) = (0u64, 0u64, 0u64);
        for chunk in paths.chunks(200) {
            let (owner, chunk) = (who.hex.clone(), chunk.to_vec());
            let done = astro_dav::vfs::vfs_tx(&self.dav.db, move |tx| {
                let mut out = Vec::new();
                for p in chunk {
                    let size: i64 = tx.query_row("SELECT COALESCE(size, 0) FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'", rusqlite::params![owner, p], |r| r.get(0)).unwrap_or(0);
                    match astro_dav::vfs::delete_tree_tx(tx, &owner, &p) {
                        Ok(cids) => out.push((cids, u64::try_from(size).unwrap_or(0), true)),
                        Err(astro_dav::vfs::VfsError::NotFound) => out.push((Vec::new(), 0, false)),
                        Err(e) => return Err(e),
                    }
                }
                Ok(out)
            })
            .await
            .map_err(|e| ApiError::internal(format!("suppression impossible : {e}")))?;
            for (cids, size, found) in done {
                if found {
                    deleted += 1;
                    bytes += size;
                } else {
                    missing += 1;
                }
                astro_dav::server::release_blobs(&self.dav, cids).await;
            }
        }
        if let Some(p) = &self.dav.publisher {
            p.notify(&who.hex);
        }
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", format!("{deleted} photo(s) supprimée(s) ({} Mo)", bytes >> 20), None);
        Ok(json!({ "deleted": deleted, "missing": missing, "bytes": bytes }))
    }

    async fn contacts(&self, who: &Principal) -> Result<Value, ApiError> {
        let links = self.contacts_service()?;
        let mut view = links.list(&who.hex).await.map_err(link_error)?;
        let login = crate::account_login(&self.services.db, &who.hex).await;
        let base = self.dav.config.public_base_url.clone().unwrap_or_default();
        view["carddav"] = json!({ "url": format!("{}{}/addressbooks/{login}/contacts/", base.trim_end_matches('/'), self.dav.config.mount.trim_end_matches('/')), "server": format!("{}{}/", base.trim_end_matches('/'), self.dav.config.mount.trim_end_matches('/')), "login": login });
        Ok(view)
    }

    async fn contacts_sync(&self, who: &Principal) -> Result<Value, ApiError> {
        let r = self.contacts_service()?.sync_owner(&who.hex).await.map_err(link_error)?;
        Ok(json!({ "indexed": r.indexed, "removed": r.removed, "linked": r.linked.iter().map(|(n, _)| n).collect::<Vec<_>>() }))
    }

    async fn contacts_create(&self, who: &Principal, name: String, email: Option<String>) -> Result<Value, ApiError> {
        self.contacts_service()?.create_from_person(&who.hex, &name, email.as_deref(), false).await.map_err(link_error)
    }

    async fn contacts_link(&self, who: &Principal, path: String, target: Option<String>) -> Result<Value, ApiError> {
        self.contacts_service()?.link(&who.hex, &path, target.as_deref()).await.map_err(link_error)
    }

    async fn account(&self, who: &Principal) -> Result<Value, ApiError> {
        self.account_service()?.view(&who.hex).await.map_err(account_error)
    }

    async fn account_site(&self, who: &Principal) -> Result<Value, ApiError> {
        let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
        let email = who.email.as_deref().ok_or_else(|| ApiError::bad_request("ce compte n'a pas d'adresse : pas de site public"))?;
        Ok(site.info(&who.hex, email).await)
    }

    async fn account_site_example(&self, who: &Principal, force: bool) -> Result<Value, ApiError> {
        let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
        let written = site.install_example(&who.hex, force).await.map_err(|e| if e.contains("existe déjà") { ApiError::conflict(e) } else { ApiError::internal(e) })?;
        Ok(json!({ "written": written }))
    }

    async fn public_move(&self, who: &Principal, paths: Vec<String>, publish: bool) -> Result<Value, ApiError> {
        let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
        if paths.is_empty() {
            return Err(ApiError::bad_request("aucun fichier choisi"));
        }
        let out = site.set_public(&who.hex, &paths, publish).await;
        let n = out["moved"].as_array().map_or(0, Vec::len);
        if n > 0 {
            self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "system", "public", format!("{n} élément(s) {} Public", if publish { "publié(s) dans" } else { "retiré(s) de" }), None);
        }
        Ok(out)
    }

    async fn public_images(&self, who: &Principal) -> Result<Value, ApiError> {
        let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
        Ok(site.images(&who.hex).await)
    }

    async fn account_image(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        use base64::Engine as _;
        let svc = self.account_service()?;
        let kind = body["kind"].as_str().unwrap_or("");
        // Une image déjà publiée dans /Public : la station la lit elle-même (aucun autre dossier de l'uCloud n'est accepté).
        let from_public;
        let data = match body["public_path"].as_str() {
            Some(path) => {
                let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
                from_public = base64::engine::general_purpose::STANDARD.encode(site.read_public(&who.hex, path).await.map_err(ApiError::bad_request)?);
                from_public.as_str()
            }
            None => body["data"].as_str().unwrap_or(""),
        };
        svc.set_image(&who.hex, kind, data, body["origin"].as_str()).await.map_err(account_error)
    }

    async fn profile_image(&self, hex: &str, kind: &str) -> Option<Vec<u8>> {
        crate::profile_image::load(&self.services.db, hex, crate::profile_image::Kind::parse(kind)?).await
    }

    async fn account_profile(&self, who: &Principal, patch: Value) -> Result<Value, ApiError> {
        self.account_service()?.update_profile(&who.hex, &patch).await.map_err(account_error)
    }

    async fn account_unlock(&self, ip: Option<std::net::IpAddr>, email: String, pass: String) -> Result<Value, ApiError> {
        self.account_service()?.unlock(ip, &email, &pass).await.map_err(account_error)
    }

    async fn account_sign(&self, who: &Principal, event: Value) -> Result<Value, ApiError> {
        self.account_service()?.sign(&who.hex, &event).await.map_err(account_error)
    }

    async fn pass_unlock(&self, _who: &Principal, hex: String) -> Result<Value, ApiError> {
        let v = self.account_service()?.clear_pass_lock(&hex).await.map_err(account_error)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "pass", format!("verrouillage du PASS levé pour {}", &hex[..8]), None);
        Ok(v)
    }

    async fn account_position(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        let pos = match (body["lat"].as_f64(), body["lon"].as_f64()) {
            (Some(lat), Some(lon)) => Some((lat, lon)),
            (None, None) if body.get("lat").is_none() => None,
            _ => return Err(ApiError::bad_request("position attendue : {lat, lon} en degrés")),
        };
        let umap = r.set_position(&who.hex, pos).await.map_err(register_error)?;
        Ok(json!({ "umap": umap }))
    }

    async fn account_eject(&self, _who: &Principal, hex: String) -> Result<Value, ApiError> {
        use astro_duniter::Chain;
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        let email = r.ejectable(&hex).await.map_err(register_error)?;
        // Ẑen éventuels (reçus d'un ami, d'un don…) : reversés au portefeuille primal UPLANETNAME_G1 avant l'éjection, comme
        // le « CASH-BACK » de nostr_DESTROY_TW.sh ; ils seront restitués si le membre revient. Solde illisible = pas d'éjection.
        let dunikey = r.nostr_root().join(&email).join(".secret.dunikey");
        let mut cashback = 0u64;
        if let Ok(text) = std::fs::read_to_string(&dunikey) {
            let chain = self.services.chain.clone().ok_or_else(|| ApiError::unavailable("station non reliée à la Ğ1 : impossible de vérifier le solde du compte avant de l'éjecter"))?;
            let key = astro_lifecycle::dunikey::Dunikey::parse(&text).map_err(|e| ApiError::internal(format!("portefeuille du compte illisible : {e}")))?;
            let signer = astro_duniter::Ed25519Signer::from_seed(*key.ed25519_seed());
            let balance = chain.balance(&signer.account()).await.map_err(|e| ApiError::unavailable(format!("solde du compte illisible ({e}) : éjection reportée")))?;
            if balance.total.0 > 0 {
                let master = crate::coop_wallets::Wallets::new(r.uplanet_name().to_owned(), self.services.db.clone()).master().await.map_err(ApiError::internal)?;
                let user = email.split('@').next().unwrap_or(&email);
                let memo = format!("MULTIPASS:{user}:PRIMAL:CASH-BACK");
                match chain.transfer(&signer, &master.account(), balance.total, astro_duniter::TransferMode::AllowDeath, Some(&memo)).await {
                    Ok(out) => {
                        cashback = balance.total.0;
                        let record = json!({ "hex": hex, "email": email, "from": signer.account().ss58(), "to": master.account().ss58(), "g1_cents": cashback, "block": hex::encode(out.block_hash) });
                        let _ = self.services.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('eject_cashback', ?1, ?2)", vec![now_secs().into(), record.to_string().into()]).await;
                    }
                    Err(e) => return Err(ApiError::unavailable(format!("reversement des {} Ğ1 du compte impossible ({e}) : éjection annulée, rien n'a changé", balance.total))),
                }
            }
        }
        let email = r.eject(&hex).await.map_err(register_error)?;
        if cashback > 0 {
            self.services.activity.publish_to(astro_core::Audience::Captain, "system", "register", format!("{email} : {} Ğ1 reversés au portefeuille primal (CASH-BACK) avant l'éjection", Amount(cashback)), None);
        }
        Ok(json!({ "ejected": email, "cashback_g1_cents": cashback }))
    }

    async fn account_token_rotate(&self, who: &Principal) -> Result<Value, ApiError> {
        let v = self.account_service()?.rotate_token(&who.hex).await.map_err(account_error)?;
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "system", "ui:token", "jeton de connexion changé : l'ancien ne fonctionne plus".to_owned(), None);
        Ok(v)
    }

    async fn account_export_keys(&self, who: &Principal, confirm: bool) -> Result<Value, ApiError> {
        let v = self.account_service()?.export_keys(&who.hex, confirm).await.map_err(account_error)?;
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "alert", "ui:keys", "vos clés secrètes viennent d'être exportées".to_owned(), None);
        Ok(v)
    }

    async fn admin_get(&self, _who: &Principal, section: &str) -> Result<Value, ApiError> {
        let f = self.admin.as_ref().ok_or_else(|| ApiError::not_found("section d'administration inconnue"))?;
        f(section.to_owned(), None).await.map_err(admin_error)
    }

    async fn admin_set(&self, _who: &Principal, section: &str, body: Value) -> Result<Value, ApiError> {
        let f = self.admin.as_ref().ok_or_else(|| ApiError::not_found("section d'administration inconnue"))?;
        let view = f(section.to_owned(), Some(body)).await.map_err(admin_error)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:admin", format!("réglage « {section} » modifié par le Capitaine"), None);
        Ok(view)
    }

    async fn console_open(&self, ticket: &str) -> Option<astro_ui::api::ConsoleLink> {
        self.console.as_ref()?.open(ticket)
    }

    async fn coop_config(&self, _who: &Principal) -> Result<Value, ApiError> {
        let (get, _) = self.coop.as_ref().ok_or_else(|| ApiError::unavailable("configuration coopérative non activée"))?;
        get().await.map_err(|e| {
            eprintln!("interface : configuration coopérative : {e}");
            ApiError::internal("configuration coopérative illisible")
        })
    }

    async fn coop_config_set(&self, who: &Principal, patch: std::collections::BTreeMap<String, Option<String>>) -> Result<Value, ApiError> {
        let (_, set) = self.coop.as_ref().ok_or_else(|| ApiError::unavailable("configuration coopérative non activée"))?;
        let keys: Vec<String> = patch.keys().cloned().collect();
        let view = set(patch).await.map_err(ApiError::bad_request)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:coop", format!("configuration coopérative modifiée : {}", keys.join(", ")), None);
        let _ = who;
        Ok(view)
    }

    async fn cooperative_view(&self, who: &Principal, ledger: Option<(u64, usize)>) -> Result<Value, ApiError> {
        let f = self.constellation.as_ref().ok_or_else(|| ApiError::unavailable("vision de la constellation non activée"))?;
        f(who.hex.clone(), ledger).await.map_err(|e| {
            eprintln!("interface : constellation : {e}");
            ApiError::internal("constellation indisponible")
        })
    }

    async fn charter_accept(&self, who: &Principal, sha256: &str) -> Result<Value, ApiError> {
        let f = self.charter.as_ref().ok_or_else(|| ApiError::unavailable("acceptation de la charte non activée"))?;
        let out = f(who.hex.clone(), sha256.to_owned()).await.map_err(ApiError::bad_request)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:charter", format!("charte {} acceptée par {}", out["version"].as_str().unwrap_or("?"), who.email.as_deref().unwrap_or("le Capitaine")), None);
        Ok(out)
    }

    async fn billet(&self, ip: Option<std::net::IpAddr>, op: &'static str, body: Value) -> Result<Value, ApiError> {
        let f = self.billet.as_ref().ok_or_else(|| ApiError::unavailable("Ğ1Billets non activés sur cette station"))?;
        f(ip, op, body).await.map_err(billet_api_error)
    }

    async fn billet_issuer(&self, who: &Principal) -> bool {
        match &self.billet {
            Some(f) => f(None, "is_issuer", who_body(who, Value::Null)).await.map(|v| v["issuer"] == true).unwrap_or(false),
            None => false,
        }
    }

    async fn account_billet_issuer(&self, who: &Principal, hex: String, allowed: bool) -> Result<Value, ApiError> {
        let r = self.billet_call("set_issuer", who, json!({ "hex": hex, "allowed": allowed })).await?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "quota", "billet", format!("Création de Ğ1Billets {} pour {}", if allowed { "autorisée" } else { "retirée" }, hex.get(..8).unwrap_or(&hex)), None);
        Ok(r)
    }

    async fn billet_issue(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.billet_call("issue", who, body).await
    }

    async fn billet_fund(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.billet_call("fund", who, body).await
    }

    async fn billet_issued(&self, who: &Principal) -> Result<Value, ApiError> {
        self.billet_call("issued", who, Value::Null).await
    }

    async fn friends(&self, who: &Principal) -> Result<Value, ApiError> {
        let f = self.friends_fn.as_ref().ok_or_else(|| ApiError::unavailable("envoi à des proches non activé"))?;
        f(who.hex.clone()).await.map_err(|e| {
            eprintln!("interface : proches : {e}");
            ApiError::internal("liste des proches indisponible")
        })
    }

    async fn backup_status(&self, who: &Principal) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("sauvegarde privée non activée"))?;
        (b.status)(who.hex.clone()).await.map_err(|e| {
            eprintln!("interface : sauvegarde privée : {e}");
            ApiError::internal("état de la sauvegarde indisponible")
        })
    }

    async fn backup_rescue(&self, who: &Principal, owner: String, accept: bool) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("sauvegarde privée non activée"))?;
        (b.rescue_answer)(who.hex.clone(), owner, accept).await.map_err(ApiError::bad_request)
    }

    async fn restore_ask(&self, card: String, send: bool) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("secours par les gardiens non disponible sur cette station"))?;
        (b.rescue_ask)(card, send).await.map_err(ApiError::bad_request)
    }

    async fn backup_set(&self, who: &Principal, enabled: bool, host: Option<String>) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("sauvegarde privée non activée"))?;
        (b.set)(who.hex.clone(), enabled, host).await.map_err(ApiError::bad_request)
    }

    async fn backup_respond(&self, who: &Principal, owner: String, accept: bool, quota_bytes: u64) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("sauvegarde privée non activée"))?;
        // Volume maximal : ce qui reste du quota de l'hébergeur, sauvegarde déjà hébergée comprise dans son occupation.
        let account = astro_dav::auth::lookup_by_hex(&self.services.db, &who.hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        let quota = self.dav.config.limits_for(&account).quota_total;
        let hex = who.hex.clone();
        let used = self.services.db.readers().read(move |c| astro_dav::vfs::used_bytes(c, &hex).map_err(|_| astro_db::DbError::Aborted)).await.map_err(db_err)?;
        (b.respond)(who.hex.clone(), owner, accept, quota_bytes, quota.saturating_sub(used)).await.map_err(ApiError::bad_request)
    }

    async fn ucloud_send(&self, who: &Principal, paths: Vec<String>, friends: Vec<String>) -> Result<Value, ApiError> {
        let f = self.send_fn.as_ref().ok_or_else(|| ApiError::unavailable("envoi à des proches non activé"))?;
        f(who.hex.clone(), paths, friends).await.map_err(ApiError::bad_request)
    }

    async fn replication(&self, who: &Principal) -> Result<Value, ApiError> {
        let hex = who.hex.clone();
        let (mine, crawl, routes) = self
            .services
            .db
            .readers()
            .read(move |c| {
                let get = |scope: &str, key: &str| -> Option<Value> {
                    rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [scope, key], |r| r.get::<_, String>(0))).ok().flatten().and_then(|t| serde_json::from_str(&t).ok())
                };
                Ok((get("replication", &hex), get("crawl_status", "last"), get("crawl_status", "routes")))
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "mine": mine, "crawl": crawl, "routes": routes.unwrap_or_else(|| json!([])) }))
    }

    async fn constellation(&self, _who: &Principal) -> Result<Value, ApiError> {
        let svc = &self.services;
        let origin = match svc.origin {
            Some(o) => Some(o),
            None => svc
                .db
                .readers()
                .read(|c| Ok(c.query_row("SELECT lat, lon FROM identities WHERE lat IS NOT NULL AND lon IS NOT NULL ORDER BY (role = 'node') DESC, hosted DESC LIMIT 1", [], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))).ok()))
                .await
                .map_err(db_err)?,
        };
        // Les autres stations viennent de l'annuaire des balises (comme pour `astroportd route`), plus de `12345.json`.
        let rows: Vec<Value> = match &self.stations {
            Some(s) => s.list(u64::try_from(crate::private_backup::now()).unwrap_or(0)).await.map_err(ApiError::internal)?["stations"].as_array().cloned().unwrap_or_default(),
            None => Vec::new(),
        };
        let connected: Vec<String> = svc.p2p.as_ref().map(|h| h.connected_peers().iter().map(ToString::to_string).collect()).unwrap_or_default();
        let me = svc.info.peer_id.clone().unwrap_or_default();
        let others: Vec<&Value> = rows.iter().filter(|r| r["station"].as_str().is_some_and(|id| id != me)).collect();
        // Ce que chaque station connectée **nous** ouvre (sa politique de tunnels répond) : jamais ce qu'elle ne nous destine pas.
        let asked: Vec<Option<Vec<String>>> = match &svc.p2p {
            Some(h) => {
                // 12 stations connectées au plus (les ids sont déjà dans l'ordre de l'annuaire, la plus récente d'abord).
                let mut budget = 12;
                let ids: Vec<Option<String>> = others
                    .iter()
                    .map(|r| {
                        let id = r["station"].as_str().filter(|id| connected.iter().any(|c| c == id) && budget > 0)?;
                        budget -= 1;
                        Some(id.to_owned())
                    })
                    .collect();
                futures_util::future::join_all(ids.iter().map(|id| async move {
                    match id {
                        Some(id) => crate::tunnel_policy::ask_services(h, id).await.ok(),
                        None => None,
                    }
                }))
                .await
            }
            None => vec![None; others.len()],
        };
        let describe = |node_id: &str, power: f64, pos: Option<(f64, f64)>, services: Vec<String>, is_self: bool| {
            let distance = match (origin, pos) {
                (Some(o), Some(p)) => Some((haversine_km(o, p) * 10.0).round() / 10.0),
                _ => None,
            };
            json!({
                "node_id": node_id, "self": is_self, "power_score": (power * 10.0).round() / 10.0, "tier": astro_p2p::routing::Tier::of(power).label(),
                "distance_km": distance, "lat": pos.map(|p| p.0), "lon": pos.map(|p| p.1),
                "status": if is_self { "local" } else if connected.iter().any(|c| c == node_id) { "connected" } else { "known" },
                "services": services,
            })
        };
        let mut nodes = vec![describe(&me, svc.hardware.score(), origin, svc.exposed.clone(), true)];
        let mut rest: Vec<(&Value, Vec<String>)> = others.into_iter().zip(asked).map(|(r, a)| (r, a.unwrap_or_default())).collect();
        rest.sort_by(|a, b| b.0["power"].as_f64().unwrap_or(0.0).total_cmp(&a.0["power"].as_f64().unwrap_or(0.0)).then_with(|| a.0["station"].as_str().cmp(&b.0["station"].as_str())));
        nodes.extend(rest.iter().map(|(r, services)| describe(r["station"].as_str().unwrap_or_default(), r["power"].as_f64().unwrap_or(0.0), r["lat"].as_f64().zip(r["lon"].as_f64()), services.clone(), false)));
        Ok(json!({ "origin": origin.map(|o| json!({ "lat": o.0, "lon": o.1 })), "nodes": nodes }))
    }

    async fn domain_info(&self, _who: &Principal) -> Result<Value, ApiError> {
        use crate::station_domain as sd;
        let conf = self.conf_path.as_ref().ok_or_else(|| ApiError::unavailable("station.conf inconnu : réglage du domaine indisponible"))?;
        let saved = crate::init::read_conf(conf).into_iter().find(|(k, _)| k == "domain").map(|(_, v)| v).filter(|v| !v.is_empty());
        let saved_proxy = sd::proxy_from_conf(conf);
        let running = self.services.info.domain.clone();
        let mode = self.web_mode();
        // Les gestes réseau de la configuration enregistrée (celle qui vaudra après redémarrage), sinon de l'actuelle.
        let proxy = if saved.is_some() { saved_proxy } else { mode == "proxy" };
        let domain = saved.clone().or_else(|| running.clone());
        Ok(json!({
            "domain": running, "saved": saved, "mode": mode, "proxy": proxy,
            "pending": saved.is_some() && (saved != running || (mode != "local" && proxy != (mode == "proxy"))),
            "steps": sd::network_steps(proxy, domain.as_deref(), self.p2p_port(), self.visio_port(proxy, domain.is_some())),
            "restart": sd::restart_available(), "restart_command": "systemctl --user restart astroportd",
        }))
    }

    async fn domain_set(&self, _who: &Principal, domain: String, proxy: Option<bool>) -> Result<Value, ApiError> {
        use crate::station_domain as sd;
        let conf = self.conf_path.clone().ok_or_else(|| ApiError::unavailable("station.conf inconnu : réglage du domaine indisponible"))?;
        let domain = sd::normalize_domain(&domain).map_err(ApiError::bad_request)?;
        // Le DNS d'abord : sans lui, ni la station ni un proxy n'obtiennent de certificat.
        let lookup = tokio::time::timeout(Duration::from_secs(8), tokio::net::lookup_host((domain.as_str(), 443))).await;
        let ips: Vec<String> = match lookup {
            Ok(Ok(addrs)) => {
                let mut v: Vec<String> = addrs.map(|a| a.ip().to_string()).collect();
                v.sort();
                v.dedup();
                v
            }
            _ => Vec::new(),
        };
        if ips.is_empty() {
            return Err(ApiError::bad_request(format!("{domain} ne pointe encore nulle part : chez votre fournisseur de domaine, créez un enregistrement A vers l'adresse publique de votre box, puis réessayez (la propagation prend parfois une heure)")));
        }
        let mut warnings = Vec::new();
        let mode = self.web_mode();
        let proxy = match proxy {
            Some(p) => p,
            None if mode == "https" => false,
            None if mode == "proxy" => true,
            None => {
                // Station en HTTP local : qui tient les ports 80/443 ici, et qui répond au nom de domaine ?
                let (p80, p443) = (sd::port_state(80), sd::port_state(443));
                if p80 == "busy" || p443 == "busy" {
                    warnings.push("les ports 80/443 de cette machine sont tenus par un autre programme : un reverse proxy (Nginx Proxy Manager…), qui servira ce domaine".to_owned());
                    true
                } else if p80 == "free" {
                    let d = domain.clone();
                    match tokio::task::spawn_blocking(move || sd::who_answers(&d)).await.unwrap_or(sd::Answer::Unknown) {
                        sd::Answer::Other(server) => {
                            warnings.push(format!("http://{domain}/ arrive sur un autre serveur web ({server}), pas sur cette machine : un reverse proxy d'une autre machine sert ce domaine"));
                            true
                        }
                        _ => sd::proxy_from_conf(&conf),
                    }
                } else {
                    sd::proxy_from_conf(&conf)
                }
            }
        };
        if !proxy && mode != "https" {
            // La station prendra elle-même les ports 80 et 443 au redémarrage.
            let (p80, p443) = (sd::port_state(80), sd::port_state(443));
            if p80 == "busy" || p443 == "busy" {
                return Err(ApiError::conflict("les ports 80/443 de cette machine sont déjà pris (Nginx Proxy Manager ?) : cochez « Un reverse proxy sert ce domaine »"));
            }
            if p80 == "privileged" || p443 == "privileged" {
                let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "astroportd".into());
                warnings.push(format!("la station n'a pas le droit d'ouvrir les ports 80 et 443 : sur cette machine, lancez une fois « sudo setcap 'cap_net_bind_service=+ep' {exe} » avant de redémarrer"));
            }
        }
        sd::set_conf_value(&conf, "domain", &domain).map_err(|e| ApiError::internal(format!("{} : {e}", conf.display())))?;
        sd::set_conf_value(&conf, sd::WEB_KEY, if proxy { "proxy" } else { "" }).map_err(|e| ApiError::internal(format!("{} : {e}", conf.display())))?;
        Ok(json!({
            "domain": domain, "ips": ips, "proxy": proxy, "warnings": warnings,
            "steps": sd::network_steps(proxy, Some(&domain), self.p2p_port(), self.visio_port(proxy, true)),
            "restart": sd::restart_available(), "restart_command": "systemctl --user restart astroportd", "url": format!("https://{domain}/"),
        }))
    }

    async fn restart(&self, _who: &Principal) -> Result<Value, ApiError> {
        if !crate::station_domain::restart_available() {
            return Err(ApiError::unavailable("la station ne tourne pas sous systemd : relancez-la vous-même (systemctl --user restart astroportd, ou la commande de démarrage)"));
        }
        self.services.activity.publish_to(astro_core::Audience::Captain, "info", "station", "redémarrage demandé par le Capitaine".to_owned(), None);
        crate::station_domain::request_restart(Duration::from_millis(500));
        Ok(json!({ "restarting": true }))
    }

    async fn join_take(&self, id: String) -> Result<Value, ApiError> {
        match crate::sponsor::take(&self.services.db, &id).await.map_err(ApiError::internal)? {
            Some(sealed) => {
                self.services.activity.publish_to(astro_core::Audience::Captain, "info", "parrainage", "une station filleule a retiré son offre de parrainage".to_owned(), None);
                Ok(json!({ "sealed": sealed }))
            }
            None => Err(ApiError::not_found("offre de parrainage introuvable, déjà utilisée ou expirée : demandez un nouveau jeton à la station marraine")),
        }
    }

    async fn welcome_list(&self, who: &Principal) -> Result<Value, ApiError> {
        Ok(json!({ "links": self.welcome_service()?.list(&who.hex).await.map_err(invite_error)? }))
    }

    async fn welcome_create(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let n = |k: &str| body[k].as_u64().map(|v| u32::try_from(v).unwrap_or(u32::MAX));
        self.welcome_service()?.create(&who.hex, body["label"].as_str().unwrap_or_default(), n("max_uses"), n("days")).await.map_err(invite_error)
    }

    async fn welcome_revoke(&self, who: &Principal, token: String) -> Result<Value, ApiError> {
        self.welcome_service()?.revoke(&who.hex, &token).await.map_err(invite_error)?;
        Ok(json!({ "revoked": true }))
    }

    async fn welcome_preview(&self, token: String) -> Result<Value, ApiError> {
        self.welcome_service()?.preview(&token).await.map_err(invite_error)
    }

    async fn welcome_arrive(&self, ip: Option<std::net::IpAddr>, token: String, email: Option<String>, alias: Option<String>, lang: String) -> Result<Value, ApiError> {
        let lang = if lang.is_empty() { "fr".to_owned() } else { lang };
        let a = self.welcome_service()?.arrive(&token, ip, email.as_deref(), alias.as_deref(), &lang).await.map_err(invite_error)?;
        let mut out = registered_json(&a.registered);
        out["chat_peer"] = json!(a.host);
        if !a.warnings.is_empty() {
            out["warnings"] = json!(a.warnings);
        }
        Ok(out)
    }

    async fn energy(&self, _who: &Principal) -> Result<Value, ApiError> {
        let home = self.conf_path.as_deref().and_then(std::path::Path::parent).ok_or_else(|| ApiError::unavailable("dossier de la station inconnu"))?;
        Ok(crate::energy::view(&crate::energy::read(home, u64::try_from(crate::private_backup::now()).unwrap_or(0))))
    }

    async fn fuel(&self, _who: &Principal) -> Result<Value, ApiError> {
        Ok(match crate::primal_init::last_status(&self.services.db).await {
            Some(mut st) => {
                st["enabled"] = json!(true);
                st
            }
            None => json!({ "enabled": false }),
        })
    }

    async fn economy(&self, _who: &Principal) -> Result<Value, ApiError> {
        let (latest, love, recent) = self
            .services
            .db
            .readers()
            .read(|c| {
                let latest: Option<(String, i64, String)> = c
                    .query_row("SELECT stream, at, data FROM journal WHERE stream IN ('economy.3x13', 'economy.drawdown', 'economy.volunteer') ORDER BY at DESC, id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .ok();
                let love: (i64, i64) = c.query_row(astro_economy::store::love_totals(), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut stmt = c.prepare("SELECT at, kind, amount_cents, detail FROM love_ledger ORDER BY at DESC, id DESC LIMIT 8")?;
                let recent = stmt.query_map([], |r| Ok(json!({ "at": r.get::<_, i64>(0)?, "kind": r.get::<_, Option<String>>(1)?, "amount_cents": r.get::<_, i64>(2)?, "detail": r.get::<_, Option<String>>(3)?.and_then(|d| serde_json::from_str::<Value>(&d).ok()) })))?.collect::<Result<Vec<_>, _>>()?;
                Ok((latest, love, recent))
            })
            .await
            .map_err(db_err)?;
        let cents = |v: &Value, k: &str| v[k].as_str().and_then(|s| s.parse::<Amount>().ok()).map(|a| a.0);
        let (kind, at, week, allocation, shares, gap) = match latest {
            Some((stream, at, data)) => {
                let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                match stream.as_str() {
                    "economy.3x13" => {
                        let (cash, rnd, assets) = (cents(&v, "cash").unwrap_or(0), cents(&v, "rnd").unwrap_or(0), cents(&v, "assets").unwrap_or(0));
                        let total = (cash + rnd + assets).max(1);
                        let share = |name: &str, c: u64| json!({ "name": name, "cents": c, "pct": (c as f64 * 1000.0 / total as f64).round() / 10.0 });
                        let alloc = json!({ "surplus_cents": cents(&v, "surplus"), "is_provision_cents": cents(&v, "is_provision"), "net_cents": cents(&v, "net"), "captain_bonus_cents": cents(&v, "captain_bonus") });
                        ("split", Some(at), v["week"].as_str().map(str::to_owned), Some(alloc), vec![share("Trésorerie", cash), share("R&D", rnd), share("Actifs", assets)], None)
                    }
                    "economy.drawdown" => ("drawdown", Some(at), v["week"].as_str().map(str::to_owned), None, Vec::new(), cents(&v, "gap")),
                    _ => ("volunteer", Some(at), v["week"].as_str().map(str::to_owned), None, Vec::new(), cents(&v, "deficit")),
                }
            }
            None => ("none", None, None, None, Vec::new(), None),
        };
        Ok(json!({
            "kind": kind, "at": at, "week": week, "allocation": allocation, "shares": shares, "gap_cents": gap,
            "love": { "total_cents": love.0, "weeks": love.1, "recent": recent },
        }))
    }

    async fn accounts(&self, _who: &Principal) -> Result<Value, ApiError> {
        let rows: Vec<AccountRow> = self
            .services
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT i.hex, i.role, i.email, i.contract, (SELECT value FROM kv WHERE scope = 'lifecycle' AND key = i.hex),                             COALESCE((SELECT SUM(size) FROM ucloud_entries e WHERE e.owner_hex = i.hex AND e.type = 'file'), 0),                             (SELECT COUNT(*) FROM ucloud_entries e WHERE e.owner_hex = i.hex AND e.type = 'file')                      FROM identities i WHERE i.hosted = 1 AND i.role IN ('multipass', 'captain', 'zencard') ORDER BY i.email, i.hex",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        for (hex, role, email, contract, state, used, files) in rows {
            let (quota, unlimited) = match astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await {
                Some(a) => {
                    let q = self.dav.config.limits_for(&a).quota_total;
                    (if q == u64::MAX { 0 } else { q }, q == u64::MAX)
                }
                None => (0, false),
            };
            let life: Value = state.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
            let status = life["status"].as_str().map(str::to_owned).or_else(|| contract.clone());
            let grace_ends = (status.as_deref() == Some("grace_period")).then(|| life["status_since"].as_i64().map(|s| s + astro_lifecycle::state::GRACE_SECS as i64)).flatten();
            let pass_locked_until = match &self.account {
                Some(a) => a.pass_locked_until(&hex).await,
                None => None,
            };
            let h = hex.clone();
            let (own_quota, own_price, price, fee): (Option<i64>, Option<u64>, u64, Option<Value>) = self
                .services
                .db
                .readers()
                .read(move |c| {
                    let own_quota = c.query_row("SELECT quota_bytes FROM identities WHERE hex = ?1", [&h], |r| r.get::<_, Option<i64>>(0))?;
                    let own_price = c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![crate::hosting_fee::PRICE_SCOPE, h], |r| r.get::<_, String>(0)).ok().and_then(|v| v.trim().parse().ok());
                    let price = crate::hosting_fee::weekly_price(c, &h)?;
                    let fee = crate::hosting_fee::lifecycle_summary(c, &h);
                    Ok((own_quota, own_price, price, fee))
                })
                .await
                .map_err(db_err)?;
            let issuer: bool = self.services.db.readers().read({ let h = hex.clone(); move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT 1 FROM kv WHERE scope = 'billet_issuer' AND key = ?1", [&h], |_| Ok(())))?.is_some()) }).await.unwrap_or(false);
            let onboarding: Value = self.services.db.readers().read({ let h = hex.clone(); move |c| Ok(crate::member_primo::onboarding(c, &h)?) }).await.ok().flatten().map_or(Value::Null, |o| o.to_json(now_secs()));
            out.push(json!({
                "onboarding": onboarding, "billet_issuer": issuer, "own_quota_bytes": own_quota, "own_weekly_zen": own_price, "weekly_zen": price, "fee": fee,
                "pass_locked_until": pass_locked_until,
                "hex": hex, "role": role, "email": email, "contract": contract, "status": status, "tracked": !life.is_null(),
                "last_ok_at": life["last_ok_at"], "status_since": life["status_since"], "grace_ends_at": grace_ends,
                "used_bytes": used, "files": files, "quota_bytes": quota, "unlimited": unlimited,
            }));
        }
        Ok(json!({ "accounts": out }))
    }

    async fn account_quota(&self, who: &Principal, hex: String, quota_bytes: Option<u64>) -> Result<Value, ApiError> {
        let account = astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        if account.tier == astro_dav::auth::Tier::Captain && quota_bytes.is_some() {
            return Err(ApiError::bad_request("le Capitaine n'a pas de quota : seul le disque de la station le borne"));
        }
        let value: rusqlite::types::Value = quota_bytes.map_or(rusqlite::types::Value::Null, |q| rusqlite::types::Value::Integer(i64::try_from(q).unwrap_or(i64::MAX)));
        self.services.db.writer().execute("UPDATE identities SET quota_bytes = ?2 WHERE hex = ?1 AND hosted = 1", vec![hex.clone().into(), value]).await.map_err(db_err)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "quota", "quota", format!("Quota de {} : {}", hex.get(..8).unwrap_or(&hex), quota_bytes.map_or_else(|| "celui de sa formule".to_owned(), |q| format!("{} Go", q >> 30))), None);
        let _ = who;
        Ok(json!({ "hex": hex, "quota_bytes": quota_bytes }))
    }

    async fn account_hosting(&self, _who: &Principal, hex: String, quota_bytes: Option<u64>, weekly_zen: Option<u64>) -> Result<Value, ApiError> {
        let account = astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        if account.tier == astro_dav::auth::Tier::Captain {
            return Err(ApiError::bad_request("le Capitaine n'a ni quota ni prix : seul le disque de la station le borne"));
        }
        if weekly_zen.is_some_and(|z| z > crate::hosting_fee::MAX_WEEKLY_ZEN) {
            return Err(ApiError::bad_request(format!("prix hebdomadaire au plus {} ẑ", crate::hosting_fee::MAX_WEEKLY_ZEN)));
        }
        let value: rusqlite::types::Value = quota_bytes.map_or(rusqlite::types::Value::Null, |q| rusqlite::types::Value::Integer(i64::try_from(q).unwrap_or(i64::MAX)));
        let w = self.services.db.writer();
        w.execute("UPDATE identities SET quota_bytes = ?2 WHERE hex = ?1 AND hosted = 1", vec![hex.clone().into(), value]).await.map_err(db_err)?;
        match weekly_zen {
            Some(z) => w.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![crate::hosting_fee::PRICE_SCOPE.to_owned().into(), hex.clone().into(), z.to_string().into(), now_secs().into()]).await,
            None => w.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![crate::hosting_fee::PRICE_SCOPE.to_owned().into(), hex.clone().into()]).await,
        }
        .map_err(db_err)?;
        let label = |q: Option<u64>, z: Option<u64>| format!("{} · {}", q.map_or_else(|| "quota par défaut".to_owned(), |q| format!("{} Go", q >> 30)), z.map_or_else(|| "prix par défaut".to_owned(), |z| format!("{z} ẑ/semaine")));
        self.services.activity.publish_to(astro_core::Audience::Captain, "quota", "quota", format!("Hébergement de {} : {}", hex.get(..8).unwrap_or(&hex), label(quota_bytes, weekly_zen)), None);
        Ok(json!({ "hex": hex, "quota_bytes": quota_bytes, "weekly_zen": weekly_zen }))
    }

    async fn hosting_defaults(&self, _who: &Principal) -> Result<Value, ApiError> {
        self.services
            .db
            .readers()
            .read(|c| {
                let get = |key: &str| -> rusqlite::Result<Option<u64>> { Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![crate::hosting_fee::DEFAULT_SCOPE, key], |r| r.get::<_, String>(0)).ok().and_then(|v| v.trim().parse().ok())) };
                Ok(json!({ "default_quota_bytes": get("default_quota")?, "default_weekly_zen": get("default_price")? }))
            })
            .await
            .map_err(db_err)
    }

    async fn set_hosting_defaults(&self, _who: &Principal, quota_bytes: Option<u64>, weekly_zen: Option<u64>) -> Result<Value, ApiError> {
        if weekly_zen.is_some_and(|z| z > crate::hosting_fee::MAX_WEEKLY_ZEN) {
            return Err(ApiError::bad_request(format!("prix hebdomadaire au plus {} ẑ", crate::hosting_fee::MAX_WEEKLY_ZEN)));
        }
        let w = self.services.db.writer();
        for (key, value) in [("default_quota", quota_bytes), ("default_price", weekly_zen)] {
            match value {
                Some(v) => w.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![crate::hosting_fee::DEFAULT_SCOPE.to_owned().into(), key.to_owned().into(), v.to_string().into(), now_secs().into()]).await,
                None => w.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![crate::hosting_fee::DEFAULT_SCOPE.to_owned().into(), key.to_owned().into()]).await,
            }
            .map_err(db_err)?;
        }
        self.services.activity.publish_to(astro_core::Audience::Captain, "quota", "quota", format!("Palier par défaut : {} · {}", quota_bytes.map_or_else(|| "quota de la formule".to_owned(), |q| format!("{} Go", q >> 30)), weekly_zen.map_or_else(|| "gratuit".to_owned(), |z| format!("{z} ẑ/semaine"))), None);
        Ok(json!({ "default_quota_bytes": quota_bytes, "default_weekly_zen": weekly_zen }))
    }

    async fn account_token(&self, who: &Principal, hex: String) -> Result<Value, ApiError> {
        let account = astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        let login: Option<String> = self.services.db.readers().read({
            let hex = hex.clone();
            move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1", [hex], |r| r.get::<_, Option<String>>(0)).ok().flatten())
        }).await.map_err(db_err)?;
        let token = astro_dav::auth::account_token(&self.services.db, &self.dav.config.station_secret, &account.owner).await;
        // Trace : un jeton remis par le Capitaine est une opération sensible.
        let audit = json!({ "by": who.hex, "for": hex }).to_string();
        let _ = self.services.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('audit.token', ?1, ?2)", vec![now_secs().into(), audit.into()]).await;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:token", format!("jeton de secours remis pour {}", login.as_deref().unwrap_or(&hex[..8])), None);
        Ok(json!({ "login": login.unwrap_or_else(|| hex.clone()), "token": token }))
    }

    /// Console de BRO : `#commande` exécute la commande (comme par message privé) ; un autre texte est noté dans le
    /// journal (emplacement 15) après avoir cherché les souvenirs proches. Pas de génération de réponse par un LLM.
    async fn bro_say(&self, who: &Principal, text: String) -> Result<Value, ApiError> {
        if let Some(command) = bus::parse(&text) {
            let out = bus::execute(self.services.as_ref(), &command, &who.hex).await;
            return Ok(json!({ "reply": out.unwrap_or_else(|e| format!("⚠️ {e}")) }));
        }
        let Some(memory) = &self.memory else { return Err(ApiError::unavailable("La mémoire de BRO n'est pas disponible : cette station n'a pas de moteur d'IA (Ollama).")) };
        let related = memory.recall(&who.hex, &text, 3, None).await.map_err(|e| ApiError::unavailable(format!("mémoire indisponible : {e}")))?;
        let meta = Meta { source: "console".into(), ..Meta::default() };
        let mut reply = match memory.remember(&who.hex, Slot::Journal, &text, meta).await {
            Ok(r) if r.mem_id.is_some() => "📝 Noté dans votre journal.".to_owned(),
            Ok(_) => "📝 Je le savais déjà.".to_owned(),
            Err(e @ astro_bro::BroError::Unstorable(_)) => format!("🔒 {e} : je ne le garde pas."),
            Err(e) => return Err(ApiError::unavailable(format!("mémoire indisponible : {e}"))),
        };
        if !related.is_empty() {
            reply.push_str("\nJe me souviens que :");
            for r in related {
                reply.push_str(&format!("\n• {}", r.memory.content));
            }
        }
        Ok(json!({ "reply": reply }))
    }

    async fn health(&self) -> Vec<Subsystem> {
        let svc = &self.services;
        let mut out = Vec::new();
        out.push(match svc.db.readers().read(|c| Ok((c.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?, c.query_row("SELECT COUNT(*), COALESCE(MAX(created_at), 0) FROM nostr_events", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?))).await {
            Ok((mode, _)) if mode != "wal" => Subsystem::new("db", Status::Degraded, format!("journal {mode} (WAL attendu)")),
            Ok(_) => Subsystem::new("db", Status::Ok, "SQLite WAL"),
            Err(e) => Subsystem::new("db", Status::Down, e.to_string()),
        });
        out.push(Subsystem::new("relay", Status::Ok, "relais Nostr à l'écoute"));
        out.push(match &svc.p2p {
            None => Subsystem::new("p2p", Status::Disabled, "maillage non activé"),
            Some(h) => {
                let n = h.connected_peers().len();
                if n == 0 { Subsystem::new("p2p", Status::Degraded, "aucun pair connecté") } else { Subsystem::new("p2p", Status::Ok, format!("{n} pair(s)")) }
            }
        });
        out.push(match (&svc.chain, svc.last_chain_call()) {
            (None, _) => Subsystem::new("duniter", Status::Disabled, "non reliée à la Ğ1"),
            (Some(_), None) => Subsystem::new("duniter", Status::Ok, "configuré, pas encore sollicité"),
            (Some(_), Some((age, true))) => Subsystem::new("duniter", Status::Ok, format!("dernière lecture réussie il y a {} s", age.as_secs())),
            (Some(_), Some((age, false))) => Subsystem::new("duniter", Status::Degraded, format!("dernière lecture en échec il y a {} s", age.as_secs())),
        });
        out.push(match self.ollama_probe().await {
            None => Subsystem::new("bro", Status::Disabled, "mémoire désactivée : pas de moteur d'IA (Ollama)"),
            Some(Ok(())) => Subsystem::new("bro", Status::Ok, "Ollama répond"),
            Some(Err(e)) => Subsystem::new("bro", Status::Degraded, e),
        });
        out.push(if self.faces_enabled { Subsystem::new("facecloud", Status::Ok, "actif") } else { Subsystem::new("facecloud", Status::Disabled, "non activé") });
        out.push(match (svc.tls_status(), crate::station_domain::refused()) {
            (None, Some(why)) => Subsystem::new("https", Status::Down, why.to_owned()),
            (tls, _) => tls_subsystem(tls, now_secs()),
        });
        out
    }
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

impl<B: BlobStore> StationUi<B> {
    /// Écran MULTIPASS : redevances courantes d'usage (**Opex**) distinctes des parts d'investissement matériel
    /// (**Capex**, ZenCard). Les parts de matériel sont comptées à part des loyers.
    async fn opex_capex(&self, owner: &str, account: &astro_dav::auth::Account) -> Result<(Value, Value), ApiError> {
        let (owner, now) = (owner.to_owned(), now_secs());
        let tier = format!("{:?}", account.tier);
        let weekly_rent = self.services.weekly_rent_cents;
        let (opex, capex) = self
            .services
            .db
            .readers()
            .read(move |c| {
                let since = now - 28 * 86_400;
                let (rent_28, rent_total): (i64, i64) = c.query_row(
                    "SELECT COALESCE(SUM(CASE WHEN at >= ?2 THEN amount_cents END), 0), COALESCE(SUM(amount_cents), 0) FROM rents WHERE owner_hex = ?1",
                    rusqlite::params![owner, since],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let (contract, email): (Option<String>, Option<String>) = c.query_row("SELECT contract, email FROM identities WHERE hex = ?1", [&owner], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut assets = Vec::new();
                let (mut value, mut amortized) = (0i64, 0i64);
                if let Some(email) = email {
                    let mut stmt = c.prepare("SELECT id, label, asset_type, COALESCE(value_zen, 0), COALESCE(amortized_zen, 0), status FROM capital_assets WHERE lower(email) = lower(?1) ORDER BY date, id")?;
                    for row in stmt.query_map([&email], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?, r.get::<_, Option<String>>(5)?)))? {
                        let (id, label, kind, v, a, status) = row?;
                        let (v, a) = ((v * 100.0).round() as i64, (a * 100.0).round() as i64);
                        value += v;
                        amortized += a;
                        assets.push(json!({ "id": id, "label": label, "asset_type": kind, "value_cents": v, "amortized_cents": a, "net_cents": (v - a).max(0), "status": status }));
                    }
                }
                let opex = json!({
                    "weekly_rent_cents": weekly_rent, "rent_28d_cents": rent_28, "rent_total_cents": rent_total, "total_28d_cents": rent_28,
                });
                Ok((opex, contract, assets, value, amortized))
            })
            .await
            .map_err(db_err)
            .map(|(opex, contract, assets, value, amortized)| {
                let zencard = tier == "ZenCard";
                (opex, json!({ "zencard": zencard, "tier": tier, "contract": contract, "assets": assets, "value_cents": value, "amortized_cents": amortized, "net_cents": (value - amortized).max(0) }))
            })?;
        Ok((opex, capex))
    }
}

impl<B: BlobStore> StationUi<B> {
    fn contacts_service(&self) -> Result<&Arc<crate::identity_links::IdentityLinks<B>>, ApiError> {
        self.contacts.as_ref().ok_or_else(|| ApiError::unavailable("l'annuaire n'est pas disponible sur cette station"))
    }

    fn account_service(&self) -> Result<&Arc<crate::account::AccountService>, ApiError> {
        self.account.as_ref().ok_or_else(|| ApiError::unavailable("Mon compte n'est pas disponible sur cette station"))
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

impl<B: BlobStore> StationUi<B> {
    /// Clé qui a signé la requête (NIP-98, corps lié à la signature, anti-rejeu). L'URL attendue est celle que la station annonce
    /// publiquement (`public_base_url`), à défaut celle de l'en-tête `Host`.
    async fn nostr_caller(&self, headers: &HeaderMap, method: &str, uri: &Uri, body: &[u8]) -> Result<String, ApiError> {
        let value = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).ok_or_else(|| ApiError::denied("signature NIP-98 attendue"))?.to_owned();
        let base = match &self.dav.config.public_base_url {
            Some(b) => b.trim_end_matches('/').to_owned(),
            None => format!("http://{}", headers.get(axum::http::header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost")),
        };
        let url = format!("{base}{}", uri.path_and_query().map_or("/", |p| p.as_str()));
        let (method, body, store) = (method.to_owned(), body.to_vec(), self.dav.replay.clone());
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        tokio::task::spawn_blocking(move || astro_crypto::nip98::verify_authorization(&value, &astro_crypto::nip98::Nip98Request { method: &method, url: &url, body: Some(&body) }, now, store.as_ref()))
            .await
            .map_err(|_| ApiError::internal("vérification interrompue"))?
            .map(|v| v.pubkey)
            .map_err(|e| ApiError::denied(format!("NIP-98 refusé : {e}")))
    }

    async fn is_hosted(&self, hex: &str) -> bool {
        use rusqlite::OptionalExtension;
        let h = hex.to_owned();
        self.services.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [h], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false)
    }
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

impl<B: BlobStore> StationUi<B> {
    /// Ajoute à une fiche `keys` : les clés de la personne dans chaque UPlanet (emails de la fiche, clé déclarée, liens prouvés).
    async fn add_keys(&self, card: &mut Value) {
        let Some(people) = &self.people else { return };
        let emails: Vec<String> = card["fields"]["emails"].as_array().map(|a| a.iter().filter_map(|e| e["value"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let declared = card["pubkey"].as_str().map(str::to_owned);
        card["keys"] = json!(people.keys_for(&emails, declared.as_deref()).await);
        card["plane"] = json!(people.plane());
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
