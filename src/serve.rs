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
use astroportd::lease_gate::{LeaseGate, resource_for};
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
    /// URL du moteur d'inférence faciale : active FaceCloud.
    #[arg(long)]
    face_engine: Option<String>,
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
    /// Laisse ouverts à tout le maillage les services GPU (`comfyui`, `ollama`) : sans cette option, un flux n'est
    /// accepté que d'un pair qui détient un bail `gpu_1_24` valide (voir `astroportd tariff`).
    #[arg(long)]
    p2p_free_gpu: bool,
    /// Réserver une place de relais chez ce pair (station derrière une box).
    #[arg(long = "p2p-use-relay")]
    p2p_use_relay: Vec<Multiaddr>,
    /// Exposer un service local dans le maillage : `nom=hôte:port` (ex. `ssh=127.0.0.1:22`). Répétable.
    #[arg(long = "p2p-expose")]
    p2p_expose: Vec<String>,
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
    /// Nœud RPC Duniter v2s (`wss://…`) pour les soldes ; répétable (bascule automatique).
    #[arg(long = "duniter-rpc")]
    duniter_rpc: Vec<String>,
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
    /// Active le courtier autonome : une station **sans GPU** dont la file GPU s'allonge (attente estimée > 30 s)
    /// réserve 1 h de GPU 1/24 chez le Brain le plus proche par un virement Ẑen signé. Dépense réelle : opt-in.
    #[arg(long)]
    auto_lease: bool,
    /// Prix maximal accepté par le courtier autonome, en centimes de Ẑen par semaine.
    #[arg(long, default_value_t = 500)]
    auto_lease_max_price: u64,
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

/// Publication des virements du courtier : relai local puis relais de la constellation ; réussi dès qu'un relai accepte.
pub(crate) struct LeasePublisher {
    relay: Arc<Relay>,
    client: WsClient,
    relays: Vec<String>,
}

impl astro_bro::broker::Publisher for LeasePublisher {
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
    let local = Arc::new(AutoBlobStore::new(a.kubo.as_deref().map(KuboBlobStore::new), FsBlobStore::open(&a.blobs)?));

    let node_hex = match (&station.discovered, &station.node_secret) {
        (Some(d), _) => d.node.nostr_hex.clone(),
        (None, Some(secret)) => pubkey_of(secret)?,
        (None, None) => String::new(),
    };
    let activity_for_gate = activity.clone();

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
        for spec in &a.p2p_expose {
            let (name, target) = spec.split_once('=').ok_or("--p2p-expose attend nom=hôte:port")?;
            // Les services GPU ne s'ouvrent qu'aux détenteurs d'un bail valide (sauf `--p2p-free-gpu`).
            let mut opts = ExposeOpts::default();
            if resource_for(name).is_some() && !a.p2p_free_gpu {
                if node_hex.is_empty() {
                    return Err(format!("« {name} » exige un bail : la clé Nostr du NODE est introuvable (--game-dir) ; ou --p2p-free-gpu").into());
                }
                opts.gate = Some(Arc::new(LeaseGate::new(db.clone(), node_hex.clone(), Some(activity_for_gate.clone()))));
                println!("P2P : {name} réservé aux détenteurs d'un bail gpu_1_24");
            }
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
    let relay = Relay::new(db.clone(), circle, RelayConfig { projection: Some(Arc::new(Projector::plan_lossy)), ..RelayConfig::default() });
    let mut dav_config = DavConfig::new(station_secret);
    dav_config.lease_node = (!node_hex.is_empty()).then(|| node_hex.clone()); // un bail de disque relève le quota
    if let Some(domain) = a.domains.first() {
        dav_config.public_base_url = Some(format!("https://{domain}")); // liaison NIP-98 derrière notre propre TLS
    }
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let stop_watch = stop_rx.clone();
    let mut worker = None;
    let mut on_upload = None;
    if let Some(engine_url) = &a.face_engine {
        let mut client = WsClient::new(Duration::from_secs(30));
        if let Some(s) = signer.clone() {
            client = client.with_signer(s);
        }
        let face = Arc::new(FaceCloud::new(
            db.clone(),
            blobs.clone(),
            Arc::new(HttpFaceEngine::new(engine_url, Duration::from_secs(120))),
            Arc::new(RelayOutbox::new(client, a.relays.clone())),
            keys,
            Some(publisher.clone()),
            FaceConfig::default(),
        ));
        on_upload = Some(face.upload_hook());
        worker = Some(tokio::spawn(face.run_worker(Duration::from_secs(5), stop_rx)));
        println!("FaceCloud actif (moteur {engine_url})");
    }
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay, publisher: Some(publisher.clone()), config: dav_config, on_upload });

    // BRO LifeOS : mémoire sémantique (Ollama), services de la station, bus de commandes par messages privés.
    let ollama = a.ollama.as_deref().map(|url| Arc::new(OllamaClient::new(url, &a.embed_model, &a.chat_model, Duration::from_secs(60))));
    let memory = ollama.as_ref().map(|o| Arc::new(MemoryStore::new(db.clone(), o.clone(), o.clone(), Some(activity.clone()))));
    let hardware = tokio::task::spawn_blocking(Hardware::detect).await?;
    let chain = (!a.duniter_rpc.is_empty()).then(|| Arc::new(RpcPool::new(SubxtConnector, PoolConfig { urls: a.duniter_rpc.clone(), cooldown: Duration::from_secs(60), reconnect_delay: Duration::from_secs(2) })));
    let peer_id = p2p_handle.as_ref().map(|h| h.peer_id().to_string()).or_else(|| station.discovered.as_ref().map(|d| d.node.peer_id.clone()));
    let services_peer = peer_id.clone();
    let info = StationInfo { node_hex: node_hex.clone(), peer_id, domain: a.domains.first().cloned(), started: std::time::Instant::now() };
    let services = Arc::new(StationServices::new(StationDeps { db: db.clone(), info, nostr_dir: station.nostr_dir.clone(), chain, hardware, p2p: p2p_handle.clone(), weekly_rent_cents: a.weekly_rent, activity: activity.clone(), swarm_dir: a.swarm_dir.clone().or_else(default_swarm_dir), origin: a.lat.zip(a.lon), exposed: a.p2p_expose.iter().filter_map(|s| s.split_once('=').map(|(n, _)| n.to_owned())).collect() }));
    println!("matériel : {}", station::hardware_text(&hardware, None));
    let mut background: Vec<tokio::task::JoinHandle<()>> = vec![tokio::spawn(watch_relay(relay.subscribe(), activity.clone(), stop_watch.clone()))];
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
    // Publication des événements signés (courtier, santé économique, vidéos) : relai local puis relais.
    let event_publisher = {
        let mut client = WsClient::new(Duration::from_secs(30));
        if let Some(s) = signer.clone() {
            client = client.with_signer(s);
        }
        Arc::new(LeasePublisher { relay: relay.clone(), client, relays: a.relays.clone() })
    };
    let node_publisher = station.node_secret.as_ref().map(|_| event_publisher.clone());
    // Courtier autonome (opt-in) : réserve du GPU quand la file d'une station sans GPU s'allonge.
    if a.auto_lease {
        match (&station.node_secret, &node_publisher) {
            (Some(secret), Some(publisher)) => {
                let config = astro_bro::broker::BrokerConfig { max_unit_price_cents: a.auto_lease_max_price, ..Default::default() };
                let broker = Arc::new(astro_bro::broker::Broker::new(db.clone(), **secret, hardware.vram_gb > 0.0, config, publisher.clone(), Some(activity.clone()))?);
                let (dir, origin) = (a.swarm_dir.clone().or_else(default_swarm_dir), a.lat.zip(a.lon));
                let swarm: astro_bro::broker::SwarmSource = Arc::new(move || (dir.as_deref().map(astro_p2p::routing::read_swarm_dir).unwrap_or_default(), origin));
                println!("BRO : courtier autonome actif (GPU {}, plafond {} centimes/sem.)", if hardware.vram_gb > 0.0 { "local : inactif" } else { "absent : achat si file critique" }, a.auto_lease_max_price);
                background.push(tokio::spawn(broker.run(Duration::from_secs(15), swarm, stop_watch.clone())));
            }
            _ => return Err("--auto-lease exige la clé Nostr du NODE (--game-dir)".into()),
        }
    }
    // Documents demandés par `#upload` : rapatriés, chiffrés et déposés dans /Uploads (le demandeur est prévenu).
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
    let ui = Arc::new(StationUi::new(services, dav.clone(), memory, ollama, a.face_engine.is_some()).with_consent(consent_fn));

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
