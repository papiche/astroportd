//! `astroportd serve` : le démon complet — relai Nostr, WebDAV uCloud, FaceCloud, reverse proxy, HTTPS automatique
//! (Let's Encrypt / ZeroSSL, sans Nginx) et maillage P2P (tunnels DRAGON, échange de blobs).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use astro_bro::bus::{CommandBus, Reply};
use astro_bro::{MemoryStore, OllamaClient};
use astro_core::ActivityBus;
use astro_crypto::{FileKeySource, KeySource};
use astro_duniter::{PoolConfig, RpcPool, SubxtConnector};
use astro_dav::{AutoBlobStore, DavConfig, DavState, FsBlobStore, KuboBlobStore, ManifestPublisher};
use astro_db::{Db, SqliteReplayStore};
use astro_facecloud::{FaceCloud, FaceConfig, HttpFaceEngine, Outbox};
use astro_p2p::blob::{P2pBlobStore, ServeOpts, serve_blobs};
use astro_p2p::tunnel::{ExposeOpts, expose, forward};
use astro_p2p::routing::Hardware;
use astro_p2p::{Multiaddr, P2pConfig, P2pNode, PeerId, parse_swarm_key};
use astro_relay::{DbCircle, Relay, RelayConfig};
use astro_sync::{Projector, RelayOutbox, WsClient};
use astro_ui::acme::{LETS_ENCRYPT, LETS_ENCRYPT_STAGING};
use astro_ui::{AcmeConfig, AcmeManager, Frontend, Gate, KvCertStore, ProxyRoute, proxy_router};
use astro_nostr_proto::{Event, pubkey_of};
use base64::Engine;
use clap::Args;

use crate::identity;
use astroportd::station::{self, StationDeps, StationInfo, StationServices};
use astroportd::tunnel_policy::{PolicyGate, TunnelPolicy};
use astroportd::ui_backend::StationUi;
use astroportd::upload_worker::{Notify, UploadWorker};

type Failure = Box<dyn std::error::Error>;

/// Options de `astroportd serve`.
#[derive(Args)]
pub struct ServeArgs {
    /// Base `uplanet.db`.
    #[arg(long)]
    db: PathBuf,
    /// Dossier des blobs chiffrés (repli local d'IPFS).
    #[arg(long)]
    blobs: PathBuf,
    /// Dossier des comptes (`<email>/.secret.nostr`) ; défaut : `<game-dir>/nostr`.
    #[arg(long)]
    secrets: Option<PathBuf>,
    /// Dossier `~/.zen/game` des identités locales (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
    #[arg(long)]
    game_dir: Option<PathBuf>,
    /// Nom de domaine de la station ; active HTTPS automatique. Répétable (le premier est le principal).
    #[arg(long = "domain")]
    domains: Vec<String>,
    /// Email de contact du compte ACME.
    #[arg(long)]
    acme_email: Option<String>,
    /// Annuaire ACME (Let's Encrypt par défaut ; ZeroSSL : https://acme.zerossl.com/v2/DV90).
    #[arg(long)]
    acme_directory: Option<String>,
    /// Utiliser l'annuaire de test de Let's Encrypt (certificats non reconnus, quotas larges).
    #[arg(long)]
    acme_staging: bool,
    /// Liaison de compte externe ACME : identifiant de clé (ZeroSSL).
    #[arg(long, requires = "acme_eab_hmac")]
    acme_eab_kid: Option<String>,
    /// Liaison de compte externe ACME : clé HMAC en base64url (ZeroSSL).
    #[arg(long, requires = "acme_eab_kid")]
    acme_eab_hmac: Option<String>,
    /// Écoute HTTP (défi ACME + redirection vers HTTPS avec `--domain`, sinon service en clair).
    #[arg(long)]
    http_listen: Option<SocketAddr>,
    /// Écoute HTTPS (avec `--domain`).
    #[arg(long, default_value = "0.0.0.0:443")]
    https_listen: SocketAddr,
    /// Reverse proxy : `/préfixe=http://hôte:port` (répétable). Authentification de la station exigée.
    #[arg(long = "proxy")]
    proxies: Vec<String>,
    /// Même chose sans authentification (service déjà public uniquement).
    #[arg(long = "proxy-open")]
    open_proxies: Vec<String>,
    /// API RPC d'IPFS Kubo ; sans elle les blobs restent sur disque.
    #[arg(long)]
    kubo: Option<String>,
    /// URL du moteur d'inférence faciale (Brain GPU) : active FaceCloud.
    #[arg(long, conflicts_with = "face_models")]
    face_engine: Option<String>,
    /// Dossier des modèles ONNX d'InsightFace (`det_10g.onnx` et `w600k_r50.onnx`) : active FaceCloud avec une
    /// inférence **locale** (binaire compilé avec `--features onnx`, bibliothèque `libonnxruntime` via `ORT_DYLIB_PATH`).
    #[arg(long)]
    face_models: Option<PathBuf>,
    /// Fils de calcul par modèle pour l'inférence locale (0 = au choix d'ONNX Runtime).
    #[arg(long, default_value_t = 0)]
    face_threads: usize,
    /// Relais où publier les partages FaceCloud (répétable).
    #[arg(long = "relay")]
    relays: Vec<String>,
    /// Fichier `swarm.key` du réseau privé P2P.
    #[arg(long)]
    swarm_key: Option<PathBuf>,
    /// Écoute P2P (`/ip4/0.0.0.0/tcp/4101`). Active le maillage ; la graine vient du NODE (`secret.NODE.dunikey`).
    #[arg(long = "p2p-listen")]
    p2p_listen: Vec<Multiaddr>,
    /// Pairs à joindre au démarrage (`…/p2p/<PeerID>`).
    #[arg(long = "p2p-bootstrap")]
    p2p_bootstrap: Vec<Multiaddr>,
    /// Adresses publiques annoncées aux pairs.
    #[arg(long = "p2p-external")]
    p2p_external: Vec<Multiaddr>,
    /// Servir de relais (circuit relay v2) aux stations derrière une box.
    #[arg(long)]
    p2p_relay_server: bool,
    /// Réserver une place de relais chez ce pair (station derrière une box).
    #[arg(long = "p2p-use-relay")]
    p2p_use_relay: Vec<Multiaddr>,
    /// Exposer un service local dans le maillage : `nom=hôte:port` (ex. `ssh=127.0.0.1:22`). Répétable.
    #[arg(long = "p2p-expose")]
    p2p_expose: Vec<String>,
    /// Service SSH offert au maillage, derrière la politique de tunnels (fermé par défaut en ẐEN, ouvert aux Capitaines amis en
    /// ORIGIN, réglable dans l'écran du Capitaine) : `hôte:port`, ou `none` pour ne pas l'offrir.
    #[arg(long = "p2p-ssh", default_value = "127.0.0.1:22")]
    p2p_ssh: String,
    /// Service ComfyUI (images) offert aux Capitaines autorisés : `hôte:port`, ou `none`.
    #[arg(long = "p2p-comfyui", default_value = "127.0.0.1:8188")]
    p2p_comfyui: String,
    /// Service Ollama (IA) offert aux Capitaines autorisés : `hôte:port`, ou `none`.
    #[arg(long = "p2p-ollama", default_value = "127.0.0.1:11434")]
    p2p_ollama: String,
    /// Dossier SSH dont `authorized_keys` suit la politique de `ssh` (défaut : `~/.ssh`).
    #[arg(long = "ssh-dir")]
    ssh_dir: Option<PathBuf>,
    /// Joindre le service d'un pair depuis un port local : `bind=PeerID/nom` (ex. `127.0.0.1:2222=12D3Koo…/ssh`).
    #[arg(long = "p2p-forward")]
    p2p_forward: Vec<String>,
    /// Délai de regroupement des publications de manifeste uCloud, en secondes.
    #[arg(long, default_value_t = 2)]
    debounce_secs: u64,
    /// API d'Ollama (`http://127.0.0.1:11434`) : active la mémoire sémantique de BRO (1024D) et le cycle RÊVE.
    #[arg(long)]
    ollama: Option<String>,
    /// Modèle d'embedding de la mémoire de BRO (1024 dimensions).
    #[arg(long, default_value = "bge-m3")]
    embed_model: String,
    /// Modèle de résumé du cycle RÊVE.
    #[arg(long, default_value = "llama3.2")]
    chat_model: String,
    /// Nœud RPC Duniter v2s (`wss://…`) pour les soldes ; répétable (bascule automatique). Sans lui, les nœuds sont
    /// **découverts automatiquement** (amorces, pairs, fichier réseau officiel, annonces on-chain ; sondés puis triés par latence).
    #[arg(long = "duniter-rpc")]
    duniter_rpc: Vec<String>,
    /// Ne pas joindre la Ğ1 : ni soldes, ni écoute des virements, ni découverte de nœuds (station sans ẐEN).
    #[arg(long)]
    no_duniter: bool,
    /// Indexeur squid (GraphQL, `https://…/v1/graphql`) d'un nœud Duniter v2s : sert au contrôle primal des virements reçus
    /// (un virement n'est compté en Ẑen que s'il vient de cette UPlanet). Sans lui, il est découvert automatiquement.
    #[arg(long = "squid-url")]
    squid_url: Option<String>,
    /// Ne pas transformer les réactions Nostr `+N` en virements de Ẑen (par défaut, la réaction d'un MULTIPASS hébergé ici
    /// envoie N Ẑen au MULTIPASS de l'auteur, si son portefeuille vient de cette UPlanet).
    #[arg(long)]
    no_zen_reactions: bool,
    /// Ẑen au plus par réaction `+N`.
    #[arg(long, default_value_t = 100)]
    zen_per_reaction: u64,
    /// Ẑen au plus par donateur et par jour.
    #[arg(long, default_value_t = 500)]
    zen_per_day: u64,
    /// Désactive la synchronisation continue N¹/N² : sans elle, la station ne réplique plus les profils et événements
    /// de ses amis et de leurs amis (relais des stations de la constellation, joints par le maillage P2P).
    #[arg(long)]
    no_crawl: bool,
    /// Période de la synchronisation N¹/N², en secondes.
    #[arg(long, default_value_t = 900)]
    crawl_every: u64,
    /// Loyer hebdomadaire d'un MULTIPASS en centimes de Ẑen (jauge d'autonomie du tableau de bord).
    #[arg(long, default_value_t = 100)]
    weekly_rent: u64,
    /// Dossier des `12345.json` des pairs (défaut : `$HOME/.zen/tmp/swarm`) : alimente la vue Constellation.
    #[arg(long)]
    swarm_dir: Option<PathBuf>,
    /// Latitude de la station (distances de la vue Constellation) ; sinon celle de son NODE en base.
    #[arg(long, allow_hyphen_values = true, requires = "lon")]
    lat: Option<f64>,
    /// Longitude de la station.
    #[arg(long, allow_hyphen_values = true, requires = "lat")]
    lon: Option<f64>,
    /// Ferme l'inscription web d'un MULTIPASS (`/register`), ouverte par défaut.
    #[arg(long)]
    no_register: bool,
    /// Nombre maximal de MULTIPASS hébergés par cette station (0 = illimité).
    #[arg(long, default_value_t = 500)]
    max_accounts: u64,
    /// Inscriptions acceptées par adresse IP et par heure (0 = illimité).
    #[arg(long, default_value_t = 5)]
    register_per_hour: usize,
    /// Désactive l'orchestrateur de nuit (20h12 solaire : snapshot, purges, RÊVE, clôture hebdomadaire, santé 30850).
    #[arg(long)]
    no_nightly: bool,
    /// Dossier des snapshots nocturnes de `uplanet.db` (défaut : `snapshots/` à côté de la base).
    #[arg(long)]
    snapshot_dir: Option<PathBuf>,
    /// Charges hebdomadaires de la station (PAF), en centimes de Ẑen, déduites des recettes à la clôture de semaine.
    #[arg(long, default_value_t = 0)]
    weekly_costs: u64,
    /// Période de lecture des messages privés adressés à la station (bus de commandes de BRO), en secondes.
    #[arg(long, default_value_t = 5)]
    bus_poll_secs: u64,
}

fn default_swarm_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zen/tmp/swarm"))
}

/// Publication des événements signés de la station : relai local puis relais de la constellation ; réussi dès qu'un relai accepte.
pub(crate) struct EventPublisher {
    relay: Arc<Relay>,
    client: WsClient,
    relays: Vec<String>,
}

impl astro_bro::publisher::Publisher for EventPublisher {
    async fn publish(&self, event: Event) -> Result<(), String> {
        let (mut accepted, mut notes) = (false, Vec::new());
        let (ok, msg) = self.relay.publish_local(&event).await;
        accepted |= ok;
        if !ok {
            notes.push(format!("local : {msg}"));
        }
        for url in &self.relays {
            match self.client.publish(url, &event).await {
                Ok((true, _)) => accepted = true,
                Ok((false, why)) => notes.push(format!("{url} : {why}")),
                Err(e) => notes.push(format!("{url} : {e}")),
            }
        }
        if accepted { Ok(()) } else { Err(notes.join(" ; ")) }
    }
}

/// Réponses du bus de commandes : stockées sur le relai local (le client du titulaire les y lit) et publiées sur les
/// relais `--relay`. Réussi dès que l'un des deux accepte.
struct BroReply {
    relay: Arc<Relay>,
    outbox: Option<RelayOutbox>,
}

impl Reply for BroReply {
    async fn send(&self, recipient_hex: &str, wrap: Event) -> Result<(), String> {
        let (accepted, message) = self.relay.publish_local(&wrap).await;
        let remote = match &self.outbox {
            Some(o) => Some(o.send(recipient_hex, wrap).await.map_err(|e| e.0)),
            None => None,
        };
        match (accepted, remote) {
            (true, _) | (_, Some(Ok(()))) => Ok(()),
            (false, None) => Err(message),
            (false, Some(Err(e))) => Err(format!("{message} ; {e}")),
        }
    }
}

/// Publie sur le bus d'activité les événements acceptés par le relai, regroupés par nature toutes les 2 s
/// (un lot de 500 événements ne doit pas inonder l'interface).
async fn watch_relay(mut live: tokio::sync::broadcast::Receiver<Arc<Event>>, activity: ActivityBus, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut counts: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            ev = live.recv() => match ev {
                Ok(e) => *counts.entry(e.kind).or_default() += 1,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    activity.publish("relay", "relay", format!("{n} événement(s) non comptés (relai très sollicité)"), None);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
            _ = tick.tick() => {
                if counts.is_empty() {
                    continue;
                }
                let transfers = counts.get(&30_852).copied().unwrap_or(0);
                let total: u64 = counts.values().sum();
                let detail = counts.iter().map(|(k, n)| format!("kind {k} ×{n}")).collect::<Vec<_>>().join(", ");
                counts.clear();
                activity.publish("relay", "relay", format!("{total} événement(s) reçu(s) : {detail}"), None);
                if transfers > 0 {
                    activity.publish("economy", "relay", format!("{transfers} virement(s) Ẑen constaté(s) dans le registre Ğ1-N²"), None);
                }
            }
            _ = stop.changed() => return,
        }
    }
}

/// Lance le démon complet.
pub async fn run(a: ServeArgs) -> Result<(), Failure> {
    let station = identity::resolve(a.game_dir.as_deref(), true)?;
    let station_secret = station.secret.clone();
    let signer = station.signer.clone();
    let secrets_dir = a.secrets.clone().or(station.nostr_dir.clone()).ok_or("dossier des comptes introuvable : indiquer --secrets")?;
    let replay = Arc::new(SqliteReplayStore::open(&a.db)?);
    let db = Arc::new(Db::open(&a.db, 4)?);
    if let Some(found) = &station.discovered {
        db.writer().batch(astro_lifecycle::store::identity_statements(found)).await?;
    }
    let activity = ActivityBus::new();
    activity.publish("system", "astroportd", format!("station démarrée (v{})", env!("CARGO_PKG_VERSION")), None);
    // Politique des tunnels : qui ouvre quel service (SSH fermé par défaut, sauf en ORIGIN), réglée par le Capitaine.
    let ssh_managed = a.p2p_ssh != "none";
    let ssh_dir = ssh_managed.then(|| a.ssh_dir.clone().or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh")))).flatten();
    let tunnel_policy = TunnelPolicy::load(db.clone(), a.swarm_key.is_none(), ssh_dir, activity.clone()).await;
    let mut exposed_names: Vec<String> = a.p2p_expose.iter().filter_map(|s| s.split_once('=').map(|(n, _)| n.to_owned())).collect();
    let local = Arc::new(AutoBlobStore::new(a.kubo.as_deref().map(KuboBlobStore::new), FsBlobStore::open(&a.blobs)?));

    let node_hex = match (&station.discovered, &station.node_secret) {
        (Some(d), _) => d.node.nostr_hex.clone(),
        (None, Some(secret)) => pubkey_of(secret)?,
        (None, None) => String::new(),
    };

    // Maillage P2P (facultatif).
    let mut p2p_guards: Vec<Box<dyn std::any::Any + Send>> = Vec::new();
    let mut p2p_handle = None;
    if !a.p2p_listen.is_empty() || !a.p2p_bootstrap.is_empty() || a.p2p_relay_server || !a.p2p_use_relay.is_empty() {
        let seed = station.p2p_seed.ok_or("graine P2P introuvable : NODE absent de --game-dir (secret.NODE.dunikey) et ASTRO_P2P_SEED non défini")?;
        let mut cfg = P2pConfig::local(seed);
        if !a.p2p_listen.is_empty() {
            cfg.listen = a.p2p_listen.clone();
        }
        cfg.bootstrap = a.p2p_bootstrap.clone();
        cfg.external = a.p2p_external.clone();
        cfg.relay_server = a.p2p_relay_server;
        if let Some(path) = &a.swarm_key {
            cfg.psk = Some(parse_swarm_key(&std::fs::read_to_string(path)?)?);
        }
        let node = P2pNode::spawn(cfg).await?;
        let handle = node.handle.clone();
        println!("P2P : PeerID {} ({})", handle.peer_id(), if a.swarm_key.is_some() { "réseau privé" } else { "réseau ouvert" });
        for relay in &a.p2p_use_relay {
            match handle.listen_via_relay(relay.clone()).await {
                Ok(addr) => println!("P2P : joignable derrière la box via {addr}"),
                Err(e) => eprintln!("P2P : relais {relay} indisponible : {e}"),
            }
        }
        p2p_guards.push(Box::new(serve_blobs(&handle, local.clone(), ServeOpts::default())?));
        let mut specs: Vec<String> = a.p2p_expose.clone();
        // Les trois services partagés entre Capitaines : SSH, ComfyUI, Ollama (chacun derrière la politique des tunnels).
        for (name, target) in [("ssh", &a.p2p_ssh), ("comfyui", &a.p2p_comfyui), ("ollama", &a.p2p_ollama)] {
            if target != "none" && !specs.iter().any(|s| s.split_once('=').is_some_and(|(n, _)| n == name)) {
                specs.push(format!("{name}={target}"));
                exposed_names.push(name.to_owned());
            }
        }
        for spec in &specs {
            let (name, target) = spec.split_once('=').ok_or("--p2p-expose attend nom=hôte:port")?;
            // Chaque flux passe par la politique du Capitaine (voir l'écran Options › Tunnels) : rien n'est ouvert sans elle.
            let opts = ExposeOpts { gate: Some(Arc::new(PolicyGate::new(tunnel_policy.clone()))), ..ExposeOpts::default() };
            let mode = tunnel_policy.rule(name).mode;
            println!("P2P : {name} exposé, accès « {mode:?} » (réglable dans l'interface du Capitaine)");
            p2p_guards.push(Box::new(expose(&handle, name, target.to_owned(), opts)?));
        }
        for spec in &a.p2p_forward {
            let (bind, rest) = spec.split_once('=').ok_or("--p2p-forward attend bind=PeerID/nom")?;
            let (peer, name) = rest.split_once('/').ok_or("--p2p-forward attend bind=PeerID/nom")?;
            let fwd = forward(&handle, bind.parse()?, peer.parse::<PeerId>()?, name).await?;
            println!("P2P : {} → {name} de {peer}", fwd.local_addr);
            p2p_guards.push(Box::new(fwd));
        }
        p2p_guards.push(Box::new(node));
        p2p_handle = Some(handle);
    }
    let blobs = Arc::new(match &p2p_handle {
        Some(h) => P2pBlobStore::new(local.clone(), h.clone(), vec![]),
        None => P2pBlobStore::local_only(local.clone()),
    });

    // Services.
    let keys: Arc<dyn KeySource> = Arc::new(FileKeySource::new(&secrets_dir));
    let media_keys = keys.clone();
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys.clone(), Duration::from_secs(a.debounce_secs));
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await?;
    let _refresher = circle.spawn_refresher(db.clone(), Duration::from_secs(15));
    let conf_path = a.db.parent().map_or_else(|| PathBuf::from("station.conf"), |d| d.join("station.conf"));
    let policy = astroportd::station_admin::KindsControl::initial_policy(&db, &conf_path).await;
    let relay = Relay::new(db.clone(), circle, RelayConfig { projection: Some(Arc::new(Projector::plan_lossy)), policy, ..RelayConfig::default() });
    let kinds_control = Arc::new(astroportd::station_admin::KindsControl::new(db.clone(), &conf_path, relay.clone()));
    // Le relai est aussi offert au maillage par une écoute locale (`/x/strfry-<PeerID>`, le nom de DRAGON) : c'est le transport
    // que les autres stations, même derrière une box, utilisent pour le lire, sans port public ni domaine. Ce n'est pas un
    // tunnel réglable : la politique de lecture (pare-feu social NIP-42) reste celle du relai.
    if let Some(handle) = &p2p_handle {
        if a.p2p_expose.iter().any(|s| s.split_once('=').is_some_and(|(n, _)| n == astro_sync::peers::RELAY_SERVICE)) {
            println!("P2P : le service « {} » est déjà exposé par --p2p-expose", astro_sync::peers::RELAY_SERVICE);
        } else {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let local = listener.local_addr()?;
            tokio::spawn(astro_relay::serve(listener, relay.clone()));
            p2p_guards.push(Box::new(expose(handle, astro_sync::peers::RELAY_SERVICE, local.to_string(), ExposeOpts::default())?));
            println!("P2P : relai Nostr offert au maillage (/x/{}-{})", astro_sync::peers::RELAY_SERVICE, handle.peer_id());
        }
    }
    let mut dav_config = DavConfig::new(station_secret);
    if let Some(domain) = a.domains.first() {
        dav_config.public_base_url = Some(format!("https://{domain}")); // liaison NIP-98 derrière notre propre TLS
    }
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let stop_watch = stop_rx.clone();
    let worker;
    let on_upload;
    let share_fn: Option<astroportd::identity_links::ShareFn>;
    let contacts_cell: astroportd::identity_links::HookCell = Arc::default();
    let thumbs_fn: Option<astroportd::ui_backend::ThumbFn>;
    // Client de relais commun (lecture, publication) : le maillage P2P d'abord, l'adresse publique en repli. Les enveloppes
    // d'envoi à un ami partent vers la station d'accueil de l'ami (MeshOutbox), puis vers les relais de `--relay`.
    let relay_client = {
        let mut ws = WsClient::new(Duration::from_secs(30));
        if let Some(s) = signer.clone() {
            ws = ws.with_signer(s);
        }
        Arc::new(astroportd::p2p_relay::P2pRelayClient::new(ws, p2p_handle.clone()))
    };
    let mesh_outbox = Arc::new(astroportd::mesh_outbox::MeshOutbox::new(db.clone(), relay_client.clone(), a.relays.clone(), a.swarm_dir.clone().or_else(default_swarm_dir)));
    let sharing_keys = keys.clone();
    let account_keys = keys.clone();
    let invites_keys = keys.clone();
    let inbox_keys = keys.clone();
    let backup_keys = keys.clone();
    let assist_keys = keys.clone();
    // Sans option : si les deux modèles sont aux emplacements habituels (et le binaire compilé avec `onnx`), l'inférence locale
    // s'active toute seule ; `astroportd face-check` dit ce qui manque.
    // Moteur FaceID : une option de ligne de commande l'impose (réglage figé dans l'interface) ; sinon le choix du Capitaine
    // (enregistré) l'emporte ; sinon, si les deux modèles sont aux emplacements habituels (binaire compilé avec `onnx`),
    // l'inférence locale s'active toute seule ; `astroportd face-check` dit ce qui manque. Le Capitaine peut ensuite
    // basculer à chaud (ONNX local, Brain distant, arrêt) sans redémarrer.
    use astroportd::station_admin::{FaceControl, FaceMode, load_local_engine, local_models_found};
    let auto_models = if a.face_engine.is_none() && a.face_models.is_none() { local_models_found(&a.db) } else { None };
    let (initial_mode, locked) = match (&a.face_engine, &a.face_models) {
        (Some(url), _) => (FaceMode::Remote(url.clone()), Some("--face-engine".to_owned())),
        (None, Some(_)) => (FaceMode::Local, Some("--face-models".to_owned())),
        (None, None) => match FaceControl::stored(&db).await {
            Some(mode) => (mode, None),
            None if auto_models.is_some() => {
                println!("FaceCloud : modèles trouvés : inférence locale activée automatiquement");
                (FaceMode::Local, None)
            }
            None => (FaceMode::Off, None),
        },
    };
    let initial_engine: Option<(astro_facecloud::AnyFaceEngine, String)> = match &initial_mode {
        FaceMode::Off => None,
        FaceMode::Remote(url) => Some((astro_facecloud::AnyFaceEngine::Http(Box::new(HttpFaceEngine::new(url, Duration::from_secs(120)))), format!("moteur {url}"))),
        FaceMode::Local => match a.face_models.clone().or(auto_models) {
            Some(dir) => match load_local_engine(&dir, a.face_threads).await {
                Ok(engine) => Some((engine, format!("inférence locale ONNX ({})", dir.display()))),
                Err(e) if locked.is_some() => return Err(e.into()),
                Err(e) => {
                    eprintln!("FaceCloud : {e} : moteur arrêté (réglable dans l'interface du Capitaine)");
                    None
                }
            },
            None => {
                eprintln!("FaceCloud : inférence locale choisie mais modèles introuvables : moteur arrêté");
                None
            }
        },
    };
    let face_label = initial_engine.as_ref().map(|(_, l)| l.clone());
    let face_switch = Arc::new(astro_facecloud::SwitchableEngine::new(initial_engine));
    let face_control = Arc::new(FaceControl::new(db.clone(), face_switch.clone(), a.face_threads, a.db.parent().map(PathBuf::from).unwrap_or_default(), locked, initial_mode));
    {
        let face = Arc::new(FaceCloud::new(
            db.clone(),
            blobs.clone(),
            face_switch,
            mesh_outbox.clone(),
            keys,
            Some(publisher.clone()),
            FaceConfig::default(),
        ));
        on_upload = Some(astroportd::identity_links::upload_hook(contacts_cell.clone(), Some(face.upload_hook())));
        let for_share = face.clone();
        share_fn = Some(Arc::new(move |owner: String, friend: String| {
            let face = for_share.clone();
            Box::pin(async move {
                match face.share_backlog(&owner, &friend).await {
                    Ok((shared, skipped)) => println!("annuaire : {shared} photo(s) partagée(s) avec la personne reliée ({skipped} écartée(s) : amitié non réciproque, déjà partagé…)"),
                    Err(e) => eprintln!("annuaire : partage impossible : {e}"),
                }
            })
        }));
        let for_thumbs = face.clone();
        thumbs_fn = Some(Arc::new(move |owner: String, ids: Vec<i64>, size: u32| {
            let face = for_thumbs.clone();
            Box::pin(async move { face.face_thumbs(&owner, &ids, size).await.map(|b| (b.thumbs, b.failed)).map_err(|e| e.to_string()) })
        }));
        worker = Some(tokio::spawn(face.run_worker(Duration::from_secs(5), stop_rx)));
        println!("FaceCloud actif ({})", face_label.as_deref().unwrap_or("moteur arrêté : réglable par le Capitaine"));
    }
    let inbox_blobs = blobs.clone();
    let backup_blobs = blobs.clone();
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay, publisher: Some(publisher.clone()), config: dav_config, on_upload });
    // Annuaire : fiches vCard de /Contacts ↔ visages ↔ MULTIPASS (CardDAV est servi par le routeur WebDAV).
    let links = Arc::new(astroportd::identity_links::IdentityLinks::new(db.clone(), dav.clone(), share_fn, Some(activity.clone())));
    let _ = contacts_cell.set(links.clone());


    // BRO LifeOS : mémoire sémantique (Ollama), services de la station, bus de commandes par messages privés.
    let ollama = a.ollama.as_deref().map(|url| Arc::new(OllamaClient::new(url, &a.embed_model, &a.chat_model, Duration::from_secs(60))));
    let memory = ollama.as_ref().map(|o| Arc::new(MemoryStore::new(db.clone(), o.clone(), o.clone(), Some(activity.clone()))));
    let hardware = tokio::task::spawn_blocking(Hardware::detect).await?;
    // Constellation = secret partagé de la swarm.key (ẐEN) ou 64 zéros (ORIGIN).
    let uplanet_name = match &a.swarm_key {
        Some(path) => std::fs::read_to_string(path)?.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_owned).ok_or("swarm.key vide")?,
        None => astroportd::init::ORIGIN_UPLANETNAME.to_owned(),
    };
    // Nœuds Duniter : ceux donnés, sinon découverts (cache d'une heure dans la base ; échec = amorces, dit au démarrage).
    let discovered = if a.no_duniter || (!a.duniter_rpc.is_empty() && a.squid_url.is_some()) {
        None
    } else {
        let found = tokio::time::timeout(Duration::from_secs(40), astroportd::duniter_nodes::nodes(&db, Arc::new(astroportd::duniter_nodes::RealHttp::default()))).await.ok();
        match &found {
            Some(n) if n.degraded => eprintln!("Ğ1 : découverte des nœuds sans réponse : amorces utilisées sans sonde ({} RPC, {} squid)", n.rpc.len(), n.squid.len()),
            Some(n) => println!("Ğ1 : {} nœud(s) RPC et {} indexeur(s) découverts (le plus rapide : {} en {} ms)", n.rpc.len(), n.squid.len(), n.rpc.first().map_or("—", |r| r.url.as_str()), n.rpc.first().map_or(0, |r| r.latency_ms)),
            None => eprintln!("Ğ1 : découverte des nœuds trop lente (40 s) : amorces utilisées sans sonde"),
        }
        found.or_else(|| Some(astroportd::duniter_nodes::Nodes { rpc: astroportd::duniter_nodes::BOOTSTRAP_RPC.iter().map(|u| astroportd::duniter_nodes::Scored { url: (*u).to_owned(), latency_ms: 9999, height: 0 }).collect(), squid: astroportd::duniter_nodes::BOOTSTRAP_SQUID.iter().map(|u| astroportd::duniter_nodes::Scored { url: (*u).to_owned(), latency_ms: 9999, height: 0 }).collect(), degraded: true }))
    };
    let rpc_urls: Vec<String> = if !a.duniter_rpc.is_empty() { a.duniter_rpc.clone() } else { discovered.as_ref().map(astroportd::duniter_nodes::Nodes::rpc_urls).unwrap_or_default() };
    let squid_urls: Vec<String> = match &a.squid_url {
        Some(u) => vec![u.clone()],
        None => discovered.as_ref().map(astroportd::duniter_nodes::Nodes::squid_urls).unwrap_or_default(),
    };
    let chain = (!a.no_duniter && !rpc_urls.is_empty()).then(|| Arc::new(RpcPool::new(SubxtConnector, PoolConfig { urls: rpc_urls, cooldown: Duration::from_secs(60), reconnect_delay: Duration::from_secs(2) })));
    let funds_chain = chain.clone();
    let peer_id = p2p_handle.as_ref().map(|h| h.peer_id().to_string()).or_else(|| station.discovered.as_ref().map(|d| d.node.peer_id.clone()));
    let services_peer = peer_id.clone();
    let info = StationInfo { node_hex: node_hex.clone(), peer_id, domain: a.domains.first().cloned(), started: std::time::Instant::now() };
    let services = Arc::new(StationServices::new(StationDeps { db: db.clone(), info, nostr_dir: station.nostr_dir.clone(), chain, hardware, p2p: p2p_handle.clone(), weekly_rent_cents: a.weekly_rent, activity: activity.clone(), swarm_dir: a.swarm_dir.clone().or_else(default_swarm_dir), origin: a.lat.zip(a.lon), exposed: exposed_names.clone() }));
    println!("matériel : {}", station::hardware_text(&hardware, None));
    let mut background: Vec<tokio::task::JoinHandle<()>> = vec![tokio::spawn(watch_relay(relay.subscribe(), activity.clone(), stop_watch.clone()))];
    {
        let (links, mut stop) = (links.clone(), stop_watch.clone());
        background.push(tokio::spawn(async move {
            loop {
                let n = links.sync_all().await;
                if n > 0 {
                    println!("annuaire : {n} carnet(s) relu(s)");
                }
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(900)) => {}
                    _ = stop.changed() => return,
                }
            }
        }));
    }
    let mut notify: Option<Notify> = None;
    if let Some(secret) = &station.node_secret {
        let mut client = WsClient::new(Duration::from_secs(30));
        if let Some(s) = signer.clone() {
            client = client.with_signer(s);
        }
        let outbox = (!a.relays.is_empty()).then(|| RelayOutbox::new(client, a.relays.clone()));
        let reply = Arc::new(BroReply { relay: relay.clone(), outbox });
        let bus = Arc::new(CommandBus::new(db.clone(), **secret, services.clone(), reply, Some(activity.clone()))?);
        println!("BRO : bus de commandes à l'écoute des messages privés de la station ({}…)", &bus.station_hex()[..8]);
        let for_notify = bus.clone();
        notify = Some(Arc::new(move |owner: String, text: String| {
            let bus = for_notify.clone();
            Box::pin(async move {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                if let Err(e) = bus.notify(&owner, &text, now).await {
                    eprintln!("BRO : message à {}… non envoyé : {e}", &owner[..8.min(owner.len())]);
                }
            })
        }));
        background.push(tokio::spawn(bus.run(Duration::from_secs(a.bus_poll_secs.max(1)), stop_watch.clone())));
    }
    // Publication des événements signés (santé économique, vidéos) : relai local puis relais.
    let event_publisher = {
        let mut client = WsClient::new(Duration::from_secs(30));
        if let Some(s) = signer.clone() {
            client = client.with_signer(s);
        }
        Arc::new(EventPublisher { relay: relay.clone(), client, relays: a.relays.clone() })
    };
    let node_publisher = station.node_secret.as_ref().map(|_| event_publisher.clone());
    // Attestation de build : empreinte du binaire, publiée par le NODE pour que les autres stations la comparent à la leur.
    match tokio::task::spawn_blocking(astroportd::build_attest::BuildInfo::current).await {
        Ok(Ok(info)) => {
            println!("Build : {} {} ({})", info.version, &info.sha256[..16], info.target);
            services.set_build(info.clone());
            if let Some(secret) = station.node_secret.as_ref() {
                background.push(tokio::spawn(astroportd::build_attest::run(info, services_peer.clone(), **secret, event_publisher.clone(), stop_watch.clone())));
            }
        }
        Ok(Err(e)) => eprintln!("Build : empreinte du binaire illisible ({e}) : attestation désactivée"),
        Err(e) => eprintln!("Build : empreinte du binaire non calculée : {e}"),
    }
    // Envoi de fichiers aux proches et réception de ceux qu'ils envoient depuis d'autres stations (blob rapatrié par le maillage).
    let sharing = Arc::new(astroportd::sharing::Sharing::new(db.clone(), sharing_keys, mesh_outbox.clone(), Some(publisher.clone()), services_peer.clone()));
    {
        let mut inbox = astroportd::share_inbox::ShareInbox::new(db.clone(), inbox_blobs, inbox_keys, Some(publisher.clone()), activity.clone(), notify.clone());
        if let Some(handle) = p2p_handle.clone() {
            let local = local.clone();
            inbox = inbox.with_prefetch(Arc::new(move |cid: String, peer: String| {
                let (handle, local) = (handle.clone(), local.clone());
                Box::pin(async move {
                    let peer: astro_p2p::PeerId = peer.parse().map_err(|e| format!("PeerID illisible : {e}"))?;
                    astro_p2p::blob::fetch_blob(&handle, &[peer], &cid, local.as_ref(), 1 << 30, Duration::from_secs(120)).await.map(|_| ()).map_err(|e| e.to_string())
                })
            }));
        }
        background.push(tokio::spawn(Arc::new(inbox).run(Duration::from_secs(20), stop_watch.clone())));
        println!("partage : fichiers des proches reçus par le maillage (un passage toutes les 20 s)");
    }
    // Sauvegarde privée chez un ami : le proche doit accepter ; le blob part chiffré, sans sa clé.
    let backup = {
        let mut b = astroportd::private_backup::PrivateBackup::new(db.clone(), backup_keys, mesh_outbox.clone(), backup_blobs, sharing.clone(), activity.clone()).with_station(Some(publisher.clone()), services_peer.clone()).with_notify(notify.clone());
        if let Some(handle) = p2p_handle.clone() {
            let local = local.clone();
            b = b.with_prefetch(Arc::new(move |cid: String, peer: String| {
                let (handle, local) = (handle.clone(), local.clone());
                Box::pin(async move {
                    let peer: astro_p2p::PeerId = peer.parse().map_err(|e| format!("PeerID illisible : {e}"))?;
                    astro_p2p::blob::fetch_blob(&handle, &[peer], &cid, local.as_ref(), 1 << 30, Duration::from_secs(120)).await.map(|_| ()).map_err(|e| e.to_string())
                })
            }));
        }
        Arc::new(b)
    };
    // Assistance entre Capitaines amis : demande, réponse ; l'accès n'est ouvert que lorsqu'un ami accepte.
    let assist = Arc::new(astroportd::assist::Assist::new(db.clone(), assist_keys, mesh_outbox.clone(), tunnel_policy.clone(), exposed_names.clone(), services_peer.clone(), activity.clone()).with_notify(notify.clone()));
    background.push(tokio::spawn(assist.clone().run(Duration::from_secs(30), stop_watch.clone())));
    // Les accès temporaires expirent, les amis changent : `authorized_keys` suit la politique toutes les 5 minutes.
    {
        let (policy, mut stop) = (tunnel_policy.clone(), stop_watch.clone());
        background.push(tokio::spawn(async move {
            loop {
                let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
                if let Err(e) = policy.sync_ssh(at).await {
                    eprintln!("tunnels : authorized_keys non mis à jour : {e}");
                }
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(300)) => {}
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() {
                            return;
                        }
                    }
                }
            }
        }));
    }
    background.push(tokio::spawn(backup.clone().run(Duration::from_secs(60), stop_watch.clone())));
    // Rattrapage du GPS et de la date de prise de vue des photos reçues avant que la station ne les lise : 25 fichiers par
    // compte et par passage, chacun lu une seule fois ; le rythme s'accélère tant qu'il en reste.
    {
        let (dav, db, bus, mut stop) = (dav.clone(), db.clone(), activity.clone(), stop_watch.clone());
        background.push(tokio::spawn(async move {
            let mut wait = Duration::from_secs(30);
            loop {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    _ = stop.changed() => return,
                }
                let owners: Vec<String> = db.readers().read(|c| Ok(c.prepare("SELECT hex FROM identities WHERE hosted = 1 AND role IN ('multipass','captain','zencard')")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap_or_default();
                let (mut scanned, mut found, mut remaining) = (0, 0, 0);
                for owner in owners {
                    match astro_dav::thumb::backfill_exif(&dav, &owner, 25).await {
                        Ok(s) => (scanned, found, remaining) = (scanned + s.scanned, found + s.found, remaining + s.remaining),
                        Err(e) => {
                            bus.publish_to(astro_core::Audience::Captain, "alert", "exif", format!("rattrapage EXIF impossible : {e}"), None);
                        }
                    }
                }
                if scanned > 0 {
                    bus.publish_to(astro_core::Audience::Captain, "system", "exif", format!("rattrapage EXIF : {scanned} photo(s) lue(s), {found} avec position ou date, {remaining} restante(s)"), None);
                }
                wait = if remaining > 0 { Duration::from_secs(20) } else { Duration::from_secs(900) };
            }
        }));
    }
    // Documents demandés par `#upload` : rapatriés, chiffrés et déposés dans /Uploads (le demandeur est prévenu).
    // Synchronisation continue N¹/N² : les profils et événements des amis et de leurs amis, depuis les relais des stations
    // de la constellation (joints par le maillage P2P, adresse publique en repli).
    if !a.no_crawl {
        let client = astroportd::p2p_relay::SharedRelayClient(relay_client.clone());
        let swarm = a.swarm_dir.clone().or_else(default_swarm_dir);
        let own = services_peer.clone();
        let source: astro_sync::RelaySource = Arc::new(move || {
            let peers = swarm.as_deref().map(astro_sync::read_peers).unwrap_or_default();
            peers.iter().filter(|p| Some(&p.node_id) != own.as_ref()).map(astro_sync::Peer::target).collect()
        });
        let (hook_db, hook_bus, hook_client) = (db.clone(), activity.clone(), client.clone());
        let hook: astro_sync::PassHook = Arc::new(move |r| {
            match &r.result {
                Ok(s) => {
                    if s.inserted > 0 {
                        hook_bus.publish("relay", "crawl", format!("synchronisation N² : {} nouvel(s) événement(s) répliqué(s) ({} membres, {} relais)", s.inserted, s.keys, r.relays), None);
                    }
                    if let Some((relay, key, why)) = s.errors.first() {
                        hook_bus.publish_to(astro_core::Audience::Captain, "alert", "crawl", format!("synchronisation N² : {} échec(s), par exemple {relay} pour …{} : {why}", s.errors.len(), key.chars().take(8).collect::<String>()), None);
                    }
                }
                Err(e) => {
                    hook_bus.publish_to(astro_core::Audience::Captain, "alert", "crawl", format!("synchronisation N² interrompue : {e}"), None);
                }
            }
            let routes = serde_json::Value::Array(hook_client.0.routes().iter().map(astroportd::p2p_relay::RouteState::to_json).collect());
            let db = hook_db.clone();
            tokio::spawn(async move {
                let _ = db
                    .writer()
                    .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('crawl_status', 'routes', ?1, strftime('%s','now')) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![routes.to_string().into()])
                    .await;
            });
        });
        let config = astro_sync::CrawlConfig { relays: a.relays.clone(), ..astro_sync::CrawlConfig::default() };
        let coop_keys: Vec<String> = astroportd::coop_config::coop_keys(&uplanet_name).map(|(_, hex)| vec![hex]).unwrap_or_default();
        let crawler = Arc::new(astro_sync::Crawler::new(db.clone(), client.clone(), config).with_relay_source(source).with_hook(hook).with_extra_keys(coop_keys));
        // Mesure de la réplication des données des membres hébergés : où et par qui (événements ; fichiers si kubo est joint).
        {
            let swarm = a.swarm_dir.clone().or_else(default_swarm_dir);
            let peers_fn: astro_sync::replication::PeersFn = Arc::new(move || swarm.as_deref().map(astro_sync::read_peers).unwrap_or_default());
            // Qui détient un fichier : le maillage interne d'abord (sonde `HAS` du protocole de blobs, sans kubo), kubo pour les
            // CID qu'il ne sait pas échanger (`bafybei…` : fichiers en plusieurs blocs) quand `--kubo` est donné.
            let kubo = a.kubo.as_deref().map(|url| Arc::new(astro_dav::kubo::KuboBlobStore::new(url)));
            let mesh = p2p_handle.clone();
            let providers: Option<astro_sync::replication::ProviderFn> = (mesh.is_some() || kubo.is_some()).then(|| {
                let swarm = a.swarm_dir.clone().or_else(default_swarm_dir);
                let f: astro_sync::replication::ProviderFn = Arc::new(move |cid: String| {
                    let (kubo, mesh, swarm) = (kubo.clone(), mesh.clone(), swarm.clone());
                    Box::pin(async move {
                        if let (Some(handle), true) = (&mesh, cid.starts_with("bafkrei")) {
                            let peers = swarm.as_deref().map(astro_sync::read_peers).unwrap_or_default();
                            let probes = peers.into_iter().filter_map(|p| p.node_id.parse::<astro_p2p::PeerId>().ok().map(|id| (p.node_id, id))).map(|(name, id)| {
                                let (handle, cid) = (handle.clone(), cid.clone());
                                async move { (name, astro_p2p::blob::has_blob(&handle, id, &cid, Duration::from_secs(4)).await) }
                            });
                            let mut found = Vec::new();
                            for (name, r) in futures_util::future::join_all(probes).await {
                                if matches!(r, Ok(Some(_))) {
                                    found.push(name);
                                }
                            }
                            return Ok(found);
                        }
                        match &kubo {
                            Some(k) => k.providers(&cid, 20, Duration::from_secs(8)).await.map_err(|e| e.to_string()),
                            None => Err("CID en plusieurs blocs : non échangeable par le maillage, et kubo n'est pas joint (--kubo)".to_owned()),
                        }
                    })
                });
                f
            });
            let measured = providers.is_some();
            let config = astro_sync::ReplicationConfig { own_node_id: services_peer.clone(), ..astro_sync::ReplicationConfig::default() };
            let replicator = Arc::new(astro_sync::Replicator::new(db.clone(), client.clone(), peers_fn, providers, config));
            let alert_bus = activity.clone();
            let on_pass: Arc<dyn Fn(Result<usize, String>) + Send + Sync> = Arc::new(move |r| {
                if let Err(e) = r {
                    alert_bus.publish_to(astro_core::Audience::Captain, "alert", "replication", format!("mesure de la réplication impossible : {e}"), None);
                }
            });
            println!("réplication : mesure par membre toutes les {} s ({})", (a.crawl_every * 2).max(60), if measured { "événements et fichiers" } else { "événements seulement : --kubo absent" });
            background.push(tokio::spawn(replicator.run_forever(Duration::from_secs(120), Duration::from_secs((a.crawl_every * 2).max(60)), on_pass, stop_watch.clone())));
        }
        println!("synchronisation N¹/N² : un passage toutes les {} s (premier dans 60 s)", a.crawl_every);
        background.push(tokio::spawn(crawler.run_forever_after(Duration::from_secs(60), Duration::from_secs(a.crawl_every.max(30)), stop_watch.clone())));
    }
    // Virements Ğ1 reçus : le titulaire est prévenu dès le bloc (flux personnel + message privé de BRO).
    if let Some(chain) = funds_chain {
        match astroportd::funds::master_wallet(&uplanet_name) {
            Ok(master) => {
                let indexer = (!squid_urls.is_empty()).then(|| Arc::new(astroportd::funds::SquidPrimal::new(squid_urls.clone())));
                // Les listes issues de la découverte se renouvellent toutes les heures : nœuds RPC (le pool garde sa connexion qui
                // marche et suit la nouvelle liste à la prochaine bascule) et indexeurs squid. Une liste imposée par option ne bouge pas.
                let (auto_rpc, auto_squid) = (a.duniter_rpc.is_empty(), a.squid_url.is_none());
                if !a.no_duniter && (auto_rpc || (auto_squid && indexer.is_some())) {
                    let (pool, handle, db, mut stop) = (chain.clone(), indexer.as_ref().map(|ix| ix.urls_handle()), db.clone(), stop_watch.clone());
                    background.push(tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                () = tokio::time::sleep(Duration::from_secs(astroportd::duniter_nodes::CACHE_TTL_SECS.unsigned_abs() + 60)) => {}
                                _ = stop.changed() => return,
                            }
                            let fresh = astroportd::duniter_nodes::nodes(&db, Arc::new(astroportd::duniter_nodes::RealHttp::default())).await;
                            if fresh.degraded {
                                continue; // aucune réponse : on garde les listes qui marchent
                            }
                            if auto_rpc {
                                pool.set_urls(fresh.rpc_urls());
                            }
                            if let (true, Some(h)) = (auto_squid, &handle) {
                                *h.write().unwrap_or_else(|e| e.into_inner()) = fresh.squid_urls();
                            }
                        }
                    }));
                }
                // Réactions `+N` : le MULTIPASS hébergé ici qui réagit envoie N Ẑen (virement Ğ1) à l'auteur, sans toucher au relai.
                if !a.no_zen_reactions {
                    let zen = Arc::new(astroportd::zen_react::ZenReactions::new(
                        db.clone(),
                        chain.clone(),
                        secrets_dir.clone(),
                        indexer.clone(),
                        master.clone(),
                        activity.clone(),
                        notify.clone(),
                        astroportd::zen_react::ZenLimits { per_reaction: a.zen_per_reaction, per_day: a.zen_per_day },
                    ));
                    background.push(tokio::spawn(zen.run(relay.subscribe(), stop_watch.clone())));
                    println!("Ẑen : les réactions +N des MULTIPASS hébergés ici envoient N Ẑen (plafonds : {} par réaction, {} par jour)", a.zen_per_reaction, a.zen_per_day);
                }
                let watch = Arc::new(astroportd::funds::FundsWatch::new(db.clone(), activity.clone(), master, indexer, notify.clone(), station.nostr_dir.clone()));
                background.push(tokio::spawn(watch.run(chain, stop_watch.clone())));
            }
            Err(e) => eprintln!("Ğ1 : portefeuille maître de la constellation introuvable ({e}) : écoute des virements inactive"),
        }
    }
    background.push(tokio::spawn(Arc::new(UploadWorker::new(dav.clone(), activity.clone(), notify)).run(Duration::from_secs(10), stop_watch.clone())));
    println!("BRO : traitement des #upload actif (dépôt dans /Uploads)");
    if let Some(m) = &memory {
        println!("BRO : mémoire sémantique active (Ollama, {}) ; cycle RÊVE à {} souvenirs", a.embed_model, astro_bro::REVE_THRESHOLD);
        let (m, mut stop) = (m.clone(), stop_watch.clone());
        background.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30 * 60));
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = stop.changed() => return,
                }
                if let Err(e) = m.sweep().await {
                    eprintln!("BRO : balayage RÊVE impossible : {e}");
                }
            }
        }));
    }
    // Orchestrateur de nuit : 20h12 solaire vrai de la station.
    if !a.no_nightly {
        let origin = match a.lat.zip(a.lon) {
            Some(c) => Some(c),
            None => {
                let hex = node_hex.clone();
                db.readers().read(move |c| Ok(c.query_row("SELECT lat, lon FROM identities WHERE hex = ?1 AND lat IS NOT NULL AND lon IS NOT NULL", [hex], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))).ok())).await?
            }
        };
        match origin.map(|(lat, lon)| astro_core::solar::Coordinates::new(lat, lon)) {
            Some(Ok(coords)) => {
                let dream: Option<astroportd::solar_cron::Hook> = memory.clone().map(|m| -> astroportd::solar_cron::Hook {
                    Arc::new(move |_| {
                        let m = m.clone();
                        Box::pin(async move { m.sweep().await.map(|done| format!("{} emplacement(s) examiné(s)", done.len())).map_err(|e| e.to_string()) })
                    })
                });
                let purge_shares: astroportd::solar_cron::Hook = {
                    let dav = dav.clone();
                    Arc::new(move |now| {
                        let dav = dav.clone();
                        Box::pin(async move { astro_dav::share::purge_expired(&dav.db, &*dav.blobs, i64::try_from(now).unwrap_or(0)).await.map(|n| format!("{n} lien(s) d'invité expiré(s) supprimé(s)")).map_err(|e| e.to_string()) })
                    })
                };
                let backup_night: astroportd::solar_cron::Hook = {
                    let backup = backup.clone();
                    Arc::new(move |_| {
                        let backup = backup.clone();
                        Box::pin(async move { backup.sync_night().await })
                    })
                };
                let integrity_night: astroportd::solar_cron::Hook = {
                    let integrity = Arc::new(astroportd::integrity::Integrity::new(db.clone(), local.clone(), Some(activity.clone())));
                    let dav = dav.clone();
                    Arc::new(move |now| {
                        let (integrity, dav) = (integrity.clone(), dav.clone());
                        Box::pin(async move {
                            // Rattrapage des empreintes visuelles des photos déjà là (doublons), puis contrôle des blobs.
                            let hashed = astro_dav::thumb::backfill_phash(&dav.db, &*dav.blobs, 500).await.map_err(|e| format!("empreintes visuelles : {e}"))?;
                            let report = integrity.run(now).await?;
                            Ok(if hashed > 0 { format!("{report} ; {hashed} empreinte(s) visuelle(s) calculée(s)") } else { report })
                        })
                    })
                };
                let snapshot_dir = a.snapshot_dir.clone().unwrap_or_else(|| a.db.parent().map_or_else(|| PathBuf::from("snapshots"), |p| p.join("snapshots")));
                let nightly = Arc::new(astroportd::solar_cron::Nightly {
                    db: db.clone(),
                    snapshot_dir,
                    node_hex: node_hex.clone(),
                    node_secret: station.node_secret.as_ref().map(|s| **s),
                    peer_id: services_peer.clone(),
                    weekly_costs: astro_core::Amount(a.weekly_costs),
                    split: astro_economy::split::SplitConfig::default(),
                    purge_shares: Some(purge_shares),
                    dream,
                    backup: Some(backup_night),
                    integrity: Some(integrity_night),
                    publisher: node_publisher.clone(),
                    activity: Some(activity.clone()),
                });
                let next = astro_core::solar::system_now_checked().ok().and_then(|n| astro_core::solar::next_20h12(n, &coords).ok());
                println!("20h12 solaire : orchestrateur de nuit actif (prochaine échéance : {})", next.map_or_else(|| "horloge invalide".to_owned(), |t| format!("{} {:02}:{:02} UTC", astro_core::week::date_string(t), t % 86_400 / 3_600, t % 3_600 / 60)));
                // Rattrapage des nuits manquées pendant l'arrêt, en tâche de fond : le démarrage du serveur n'attend pas.
                {
                    let (n, c) = (nightly.clone(), coords);
                    background.push(tokio::spawn(async move {
                        let Ok(now) = astro_core::solar::system_now_checked() else { return };
                        for r in n.catch_up(now, &c).await {
                            println!("20h12 solaire : rattrapage de la nuit du {} ({} étapes, {})", r.date, r.steps.len(), if r.all_ok() { "ok" } else { "avec échec" });
                        }
                    }));
                }
                background.push(tokio::spawn(nightly.run(coords, stop_watch.clone())));
            }
            Some(Err(e)) => eprintln!("20h12 solaire : {e} : orchestrateur de nuit inactif"),
            None => eprintln!("20h12 solaire : coordonnées de la station inconnues (--lat/--lon, ou position du NODE en base) : orchestrateur de nuit inactif"),
        }
    }
    // Dépôt de films et de séries : attestation de copie privée depuis l'interface (kind 31903 signé du titulaire).
    let ffmpeg_found = tokio::process::Command::new("ffmpeg").arg("-version").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().await.is_ok_and(|s| s.success());
    let media_pipeline = Arc::new(astroportd::media::MediaPipeline {
        dav: dav.clone(),
        keys: media_keys,
        publisher: Some(event_publisher.clone()),
        activity: Some(activity.clone()),
        tools: astroportd::media::Tools::default(),
        mode: if ffmpeg_found { astroportd::media::Transcode::Auto } else { astroportd::media::Transcode::Notify },
        work_dir: a.db.parent().map_or_else(|| PathBuf::from("work"), |p| p.join("work")),
    });
    println!("médias : dépôt de films et séries (copie privée attestée, plafond 1024 Mo, encodage {})", if ffmpeg_found { "H.264/AAC automatique" } else { "recommandé seulement : ffmpeg introuvable" });
    let consent_fn: astroportd::ui_backend::ConsentFn = {
        let p = media_pipeline.clone();
        Arc::new(move |owner| {
            let p = p.clone();
            Box::pin(async move { p.record_consent(&owner).await.map(|_| ()).map_err(|e| e.to_string()) })
        })
    };
    // Inscription d'un MULTIPASS : `uplanet_name` (voir plus haut) désigne la constellation.
    let registrar = {
        let secret = station.secret.clone();
        let publisher = event_publisher.clone();
        let publish: astroportd::register::PublishFn = Arc::new(move |e| {
            let p = publisher.clone();
            Box::pin(async move { astro_bro::publisher::Publisher::publish(&*p, e).await })
        });
        let captain = station.discovered.as_ref().and_then(|d| d.captain()).map(|m| m.hex.clone());
        let limits = astroportd::register::Limits { open: !a.no_register, max_accounts: a.max_accounts, per_ip_per_hour: a.register_per_hour };
        Arc::new(
            astroportd::register::Registrar::new(db.clone(), secrets_dir.clone(), uplanet_name.clone(), Arc::new(move |hex| astro_dav::auth::derive_dav_token(&secret, hex)), limits)
                .with_captain(captain)
                .with_station_domain(a.domains.first().cloned())
                .with_relays(a.domains.first().map(|d| format!("wss://{d}")).into_iter().collect())
                .with_publisher(publish, Some(activity.clone())),
        )
    };
    println!("inscription web des MULTIPASS : {} (constellation {}, max {} comptes, {} par heure et par adresse) → /register", if a.no_register { "fermée" } else { "ouverte" }, registrar.constellation(), a.max_accounts, a.register_per_hour);
    // Invitations : le lien `/i/<jeton>` crée le MULTIPASS du destinataire, l'abonne à l'expéditeur et range les photos.
    let invites = {
        let relays = event_publisher.clone();
        let publish: astroportd::register::PublishFn = Arc::new(move |e| {
            let p = relays.clone();
            Box::pin(async move { astro_bro::publisher::Publisher::publish(&*p, e).await })
        });
        Arc::new(astroportd::invites::Invites::new(db.clone(), dav.blobs.clone(), invites_keys, registrar.clone(), publish).with_links(links.clone()).with_publisher(publisher.clone()).with_activity(activity.clone()).with_remote(sharing.clone()))
    };
    let account_publish: astroportd::register::PublishFn = {
        let publisher = event_publisher.clone();
        Arc::new(move |e| {
            let p = publisher.clone();
            Box::pin(async move { astro_bro::publisher::Publisher::publish(&*p, e).await })
        })
    };
    let forwards = Arc::new(astroportd::forwards::Forwards::new(p2p_handle.clone()));
    let ssh_home = a.ssh_dir.clone().or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh")));
    let identity = ssh_home.as_ref().map(|d| d.join("id_ed25519")).filter(|p| p.exists());
    let known_hosts = a.db.parent().map_or_else(|| PathBuf::from("console_known_hosts"), |p| p.join("console_known_hosts"));
    let consoles = Arc::new(astroportd::console::Consoles::new(identity, known_hosts, Some(activity.clone())));
    let consoles_ui = consoles.clone();
    let assist_admin: astroportd::ui_backend::AdminFn = {
        let assist = assist.clone();
        Arc::new(move |_section, body| {
            let (assist, forwards, consoles) = (assist.clone(), forwards.clone(), consoles.clone());
            Box::pin(async move {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
                let with_local = |mut view: serde_json::Value| {
                    let open = forwards.list(now);
                    if let Some(rows) = view["incoming"].as_array_mut() {
                        for r in rows {
                            r["local"] = open.iter().find(|o| r["id"].as_str() == Some(o.0.as_str())).map_or(serde_json::Value::Null, |o| serde_json::json!({ "addr": o.1.to_string(), "until": o.2 }));
                        }
                    }
                    view
                };
                let Some(b) = body else { return assist.view().await.map(with_local) };
                match b["action"].as_str() {
                    Some("connect") => {
                        let id = b["id"].as_str().unwrap_or("");
                        let (station, service, hours) = assist.accepted_incoming(id).await.ok_or("demande introuvable ou pas encore acceptée")?;
                        forwards.open(id, &station, &service, now + hours * 3600, now).await?;
                        assist.view().await.map(with_local)
                    }
                    Some("console") => {
                        let id = b["id"].as_str().unwrap_or("");
                        let (station, _, _) = assist.accepted_incoming(id).await.ok_or("demande introuvable ou pas encore acceptée")?;
                        let (_, addr, until) = forwards.list(now).into_iter().find(|o| o.0 == id).ok_or("ouvrez d'abord l'accès local (Se connecter)")?;
                        let ticket = consoles.ticket(addr, b["user"].as_str().unwrap_or(""), &station, until)?;
                        Ok(serde_json::json!({ "ticket": ticket }))
                    }
                    Some("disconnect") => {
                        forwards.close(b["id"].as_str().unwrap_or(""));
                        assist.view().await.map(with_local)
                    }
                    Some("request") => {
                        let sent = assist.request(b["service"].as_str().unwrap_or(""), b["hours"].as_i64().unwrap_or(0), b["note"].as_str().unwrap_or(""), b["to"].as_str().filter(|t| !t.is_empty()), b["exclusive"].as_bool().unwrap_or(false)).await?;
                        let mut view = assist.view().await?;
                        view["sent"] = sent["sent"].clone();
                        view["error"] = sent["error"].clone();
                        Ok(view)
                    }
                    Some("respond") => assist.respond(b["id"].as_str().unwrap_or(""), b["other"].as_str().unwrap_or(""), b["accept"].as_bool().unwrap_or(false)).await,
                    Some("cancel") => assist.cancel(b["id"].as_str().unwrap_or("")).await,
                    _ => Err("action attendue : request, respond, cancel, connect ou disconnect".to_owned()),
                }
            })
        })
    };
    let account_origin = a.domains.first().map(|d| format!("https://{d}"));
    let mut ui = StationUi::new(services, dav.clone(), memory, ollama, true).with_console(consoles_ui).with_consent(consent_fn).with_registrar(registrar.clone()).with_invites(invites).with_contacts(links.clone()).with_account(Arc::new(astroportd::account::AccountService::new(db.clone(), secrets_dir.clone(), account_keys, station.secret.clone(), account_publish, account_origin).with_activity(activity.clone())))
        .with_admin(Arc::new(astroportd::station_admin::Admin { face: face_control, kinds: kinds_control, tunnels: Some((tunnel_policy.clone(), exposed_names.clone())), assist: Some(assist_admin), tasks: Some(db.clone()) }).handler());
    if let Some(f) = thumbs_fn {
        ui = ui.with_face_thumbs(f);
    }
    {
        let (for_coop_get, for_coop_set) = (db.clone(), (db.clone(), event_publisher.clone()));
        let coop_name = uplanet_name.clone();
        let coop_name2 = uplanet_name.clone();
        let coop_get: astroportd::ui_backend::CoopGetFn = Arc::new(move || {
            let (db, name) = (for_coop_get.clone(), coop_name.clone());
            Box::pin(async move {
                let (_, hex) = astroportd::coop_config::coop_keys(&name)?;
                Ok(astroportd::coop_config::view(astroportd::coop_config::load(&db, &hex).await.as_ref(), &name, &hex))
            })
        });
        let coop_set: astroportd::ui_backend::CoopSetFn = Arc::new(move |patch| {
            let ((db, publisher), name) = (for_coop_set.clone(), coop_name2.clone());
            Box::pin(async move { astroportd::coop_config::save(&db, &publisher, &name, &patch).await })
        });
        ui = ui.with_coop(coop_get, coop_set);
        let (for_friends, for_send) = (sharing.clone(), sharing.clone());
        let friends: astroportd::ui_backend::FriendsFn = Arc::new(move |owner: String| {
            let sharing = for_friends.clone();
            Box::pin(async move { sharing.friends(&owner).await.map(|f| serde_json::json!({ "friends": f.iter().map(|x| serde_json::json!({ "hex": x.hex, "label": x.label, "hosted": x.hosted })).collect::<Vec<_>>() })) })
        });
        let send: astroportd::ui_backend::SendFn = Arc::new(move |owner: String, paths: Vec<String>, to: Vec<String>| {
            let sharing = for_send.clone();
            Box::pin(async move { sharing.send(&owner, &paths, &to).await })
        });
        ui = ui.with_sharing(friends, send);
        let (b1, b2, b3) = (backup.clone(), backup.clone(), backup.clone());
        ui = ui.with_backup(astroportd::ui_backend::BackupFns {
            status: Arc::new(move |owner: String| {
                let b = b1.clone();
                Box::pin(async move { b.status(&owner).await })
            }),
            set: Arc::new(move |owner: String, enabled: bool, host: Option<String>| {
                let b = b2.clone();
                Box::pin(async move { b.set(&owner, enabled, host.as_deref()).await })
            }),
            respond: Arc::new(move |host: String, owner: String, accept: bool, quota: u64, max: u64| {
                let b = b3.clone();
                Box::pin(async move { b.respond(&host, &owner, accept, quota, max).await })
            }),
        });
    }
    let ui = Arc::new(ui);

    // Reverse proxy : les routes gardées exigent un compte de la station (Basic ou NIP-98).
    let gate: Gate = {
        let dav = dav.clone();
        Arc::new(move |headers, method, uri| {
            let dav = dav.clone();
            Box::pin(async move { dav.is_authorized(&headers, method.as_str(), &uri).await })
        })
    };
    let mut routes = Vec::new();
    for spec in &a.proxies {
        routes.push(ProxyRoute::parse(spec, Some(gate.clone()))?);
    }
    for spec in &a.open_proxies {
        routes.push(ProxyRoute::parse(spec, None)?);
    }
    let public_origin = a.domains.first().map(|d| format!("https://{d}"));
    let mut app = astro_dav::router(dav.clone())
        .merge(relay.clone().router())
        .merge(astro_ui::web::static_router())
        .merge(astro_ui::api::router(ui, activity.clone(), public_origin));
    if !routes.is_empty() {
        app = app.merge(proxy_router(routes, Duration::from_secs(120)));
    }
    // `/` : application web pour un navigateur, NIP-11 et WebSocket pour les clients Nostr.
    let app = app.layer(axum::middleware::from_fn(astro_ui::web::negotiate));

    // Écoute : HTTPS automatique avec un domaine, sinon HTTP en clair.
    let frontend = if a.domains.is_empty() {
        let addr = a.http_listen.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8080)));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("astroportd à l'écoute sur http://{} (sans domaine : pas de TLS)", listener.local_addr()?);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });
        None
    } else {
        let mut acme = AcmeConfig::new(a.domains.clone(), a.acme_email.clone());
        acme.directory_url = a.acme_directory.clone().unwrap_or_else(|| (if a.acme_staging { LETS_ENCRYPT_STAGING } else { LETS_ENCRYPT }).to_owned());
        if let (Some(kid), Some(hmac)) = (&a.acme_eab_kid, &a.acme_eab_hmac) {
            acme.eab = Some((kid.clone(), base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(hmac.trim_end_matches('='))?));
        }
        let manager = AcmeManager::new(acme, KvCertStore::new(db.clone()));
        let http = a.http_listen.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 80)));
        let front = Frontend::start(manager, app, http, a.https_listen).await?;
        println!("astroportd : HTTP {} (défi ACME + redirection) et HTTPS {} pour {}", front.http_addr, front.https_addr, a.domains.join(", "));
        Some(front)
    };

    tokio::signal::ctrl_c().await?;
    let _ = stop.send(true);
    for t in background {
        let _ = t.await;
    }
    if let Some(f) = frontend {
        f.shutdown();
    }
    if let Some(w) = worker {
        let _ = w.await;
    }
    for (owner, reason) in publisher.flush().await {
        eprintln!("manifeste non publié pour {owner} : {reason}");
    }
    drop(p2p_guards);
    db.writer().shutdown().await?;
    Ok(())
}
