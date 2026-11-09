//! `astroportd serve` : le démon complet — relai Nostr, WebDAV uCloud, FaceCloud, reverse proxy, HTTPS automatique
//! (Let's Encrypt / ZeroSSL, sans Nginx) et maillage P2P (tunnels DRAGON, échange de blobs).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use astro_bro::bus::{CommandBus, Reply};
use astro_bro::{AnyEmbedder, MemoryStore, OllamaClient};
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
use clap::Args;

use crate::identity;
use astroportd::station::{self, StationDeps, StationInfo, StationServices};
use astroportd::tunnel_policy::{PolicyGate, TunnelPolicy};
use astroportd::ui_backend::StationUi;
use astroportd::upload_worker::{Notify, UploadWorker};

mod admin;
mod face;
mod p2p;
mod tasks;
mod ui;

type Failure = Box<dyn std::error::Error>;
/// WebDAV de la station : blobs locaux, maillage P2P en repli.
type Dav = DavState<P2pBlobStore<AutoBlobStore>>;
/// Mémoire sémantique de BRO (Ollama pour les embeddings et pour les résumés).
type Memory = MemoryStore<AnyEmbedder, OllamaClient>;
/// Sauvegarde privée chez un ami.
type Backup = astroportd::private_backup::PrivateBackup<astroportd::mesh_outbox::MeshOutbox, P2pBlobStore<AutoBlobStore>>;

/// Options de `astroportd serve`.
#[derive(Args)]
pub struct ServeArgs {
    /// Base `uplanet.db`.
    #[arg(long)]
    db: PathBuf,
    /// Dossier des blobs chiffrés (repli local d'IPFS).
    #[arg(long)]
    blobs: PathBuf,
    /// Bases `uplanet.db` des **autres** astroportd (plans ORIGIN / ZEN*) qui partagent ce même dossier `--blobs` : un blob
    /// qu'elles référencent n'est jamais supprimé d'ici. Répétable, ou séparées par `:`.
    #[arg(long = "pool-peer", value_delimiter = ':')]
    pool_peers: Vec<PathBuf>,
    /// Dossier des comptes (`<email>/.secret.nostr`) ; défaut : `<game-dir>/nostr`.
    #[arg(long)]
    secrets: Option<PathBuf>,
    /// Dossier `~/.zen/game` des identités locales (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
    #[arg(long)]
    game_dir: Option<PathBuf>,
    /// Nom de domaine de la station ; active HTTPS automatique. Répétable (le premier est le principal).
    #[arg(long = "domain")]
    domains: Vec<String>,
    /// Domaine public d'une station **derrière un reverse proxy** (Nginx Proxy Manager…) qui tient déjà les ports 80/443
    /// et renvoie ce domaine vers `--http-listen` (défaut `0.0.0.0:8090`) : la station ne demande pas de certificat, mais se
    /// présente sous `https://<domaine>` (balise 30850, liens, NIP-98, relai annoncé), lit l'IP des visiteurs dans
    /// `X-Forwarded-For` (posé par un proxy de la machine ou du réseau local) et sert la visio (STUN) en UDP sur le même port.
    /// Repris de `station.conf` si absent (`domain`, avec `web = proxy`).
    #[arg(long, conflicts_with = "domains")]
    public_domain: Option<String>,
    /// Email de contact du compte ACME.
    #[arg(long)]
    acme_email: Option<String>,
    /// Utiliser l'annuaire de test de Let's Encrypt (certificats non reconnus, quotas larges).
    #[arg(long)]
    acme_staging: bool,
    /// Écoute HTTP (défi ACME + redirection vers HTTPS avec `--domain`, sinon service en clair ; défaut sans domaine : 127.0.0.1:8090, derrière un reverse proxy : 0.0.0.0:8090).
    #[arg(long)]
    http_listen: Option<SocketAddr>,
    /// Écoute HTTPS (avec `--domain`).
    #[arg(long, default_value = "0.0.0.0:443")]
    https_listen: SocketAddr,
    /// Visio : écoute STUN (UDP) pour que deux navigateurs derrière leur box s'appellent en direct. Défaut : le même port que
    /// l'interface (`--https-listen` avec un domaine, sinon `--http-listen`) : une seule ouverture de port sur la box.
    #[arg(long)]
    stun_listen: Option<SocketAddr>,
    /// Visio : pas de serveur STUN sur cette station (ni de relais d'appel).
    #[arg(long, conflicts_with = "stun_listen")]
    no_stun: bool,
    /// Visio : STUN seulement, sans relais d'appel (TURN). Sans relais, un appel entre deux réseaux très fermés échoue.
    #[arg(long)]
    no_turn: bool,
    /// Visio : serveur STUN ou TURN supplémentaire donné aux navigateurs (`stun:hôte:port`, `turn:hôte:port`), répétable.
    #[arg(long = "ice-server")]
    ice_servers: Vec<String>,
    /// Reverse proxy : `/préfixe=http://hôte:port` (répétable). Authentification de la station exigée.
    #[arg(long = "proxy")]
    proxies: Vec<String>,
    /// Même chose sans authentification (service déjà public uniquement).
    #[arg(long = "proxy-open")]
    open_proxies: Vec<String>,
    /// API RPC d'IPFS Kubo ; sans elle les blobs restent sur disque.
    #[arg(long)]
    kubo: Option<String>,
    /// Passerelle de compatibilité `GET /ipfs/<cid>` pour les clients historiques (zelkova, coracle) : lecture seule des
    /// blobs chiffrés d'un lien de partage actif, et des anciens `Qm…` épinglés dans Kubo (ORIGIN). Désactivée par défaut.
    #[arg(long)]
    ipfs_gateway: bool,
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
    /// Écoute P2P. Par défaut le maillage est **actif** sur `/ip4/0.0.0.0/tcp/4101` (ou le premier port libre après) ; la
    /// graine vient du NODE (`secret.NODE.dunikey`). Répétable.
    #[arg(long = "p2p-listen")]
    p2p_listen: Vec<Multiaddr>,
    /// Couper le maillage P2P (ni tunnels, ni partage entre stations, ni services des Capitaines amis).
    #[arg(long)]
    no_p2p: bool,
    /// Pairs à joindre au démarrage (`…/p2p/<PeerID>`).
    #[arg(long = "p2p-bootstrap")]
    p2p_bootstrap: Vec<Multiaddr>,
    /// Adresses publiques annoncées aux pairs.
    #[arg(long = "p2p-external")]
    p2p_external: Vec<Multiaddr>,
    /// Servir de relais (circuit relay v2) aux stations derrière une box. Automatique quand la configuration coopérative
    /// (`P2P_RELAYS`) liste cette station.
    #[arg(long)]
    p2p_relay_server: bool,
    /// Ne pas chercher les stations du réseau local (mDNS).
    #[arg(long)]
    no_mdns: bool,
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
    /// Dossier des embeddings **locaux** de BRO (`bge-m3-int8.onnx` et `bge-m3-tokenizer.json`) : les vecteurs de la mémoire
    /// sont calculés dans le processus, sans Ollama (binaire compilé avec `--features onnx`). Sans option, les emplacements
    /// habituels sont essayés ; Ollama reste utilisé pour les résumés RÊVE et en l'absence des fichiers.
    #[arg(long)]
    embed_models: Option<PathBuf>,
    /// Fils de calcul des embeddings locaux (0 = au choix d'ONNX Runtime, soit tous les cœurs : sur une petite machine,
    /// laissez de la place au relai avec `astroportd init --embed-threads N`).
    #[arg(long, default_value_t = 0)]
    embed_threads: usize,
    /// Modèle d'embedding de la mémoire de BRO quand Ollama les calcule (1024 dimensions).
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
    /// Coupe les transactions primales (1 Ğ1 du portefeuille maître `UPLANETNAME_G1`). Par défaut, la station les envoie : aux
    /// portefeuilles coopératifs vides, au compte technique d'hébergement et au NODE (équivalent de `UPLANET.init.sh` — c'est ce
    /// qui rend une UPlanet **officielle**), puis à chaque nouveau MULTIPASS après sa première connexion.
    #[arg(long)]
    no_primal_init: bool,
    /// Période de lecture des messages privés adressés à la station (bus de commandes de BRO), en secondes.
    #[arg(long, default_value_t = 5)]
    bus_poll_secs: u64,
}

/// Moteur d'embedding de la mémoire de BRO : `bge-m3` local (ONNX Runtime) si ses fichiers sont là, sinon Ollama.
///
/// `--embed-models` **impose** le local : un dossier incomplet, une bibliothèque introuvable ou un binaire sans `onnx` est une
/// erreur. Sans l'option, des fichiers trouvés aux emplacements habituels mais inutilisables ne font pas tomber la station :
/// le problème est dit sur la sortie d'erreur et Ollama calcule les vecteurs (jamais un repli muet).
fn local_embedder(a: &ServeArgs, ollama: &Arc<OllamaClient>) -> Result<AnyEmbedder, Box<dyn std::error::Error>> {
    #[cfg(feature = "onnx")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let data = a.db.parent().map(PathBuf::from);
        let forced = a.embed_models.is_some();
        let dir = match &a.embed_models {
            Some(d) => Some(d.clone()),
            None => astro_bro::onnx::find_models(data.as_deref(), home.as_deref()),
        };
        if let Some(dir) = dir {
            let lib = astro_facecloud::onnx::find_library(std::env::var("ORT_DYLIB_PATH").ok().as_deref(), home.as_deref());
            let loaded = match &lib {
                Some(l) => astro_bro::onnx::OrtEmbedder::load(&dir, Some(l), a.embed_threads).map_err(|e| e.to_string()),
                None => Err("libonnxruntime introuvable (ORT_DYLIB_PATH, ou pip install onnxruntime)".to_owned()),
            };
            match loaded {
                Ok(engine) => {
                    println!("embeddings de BRO : bge-m3 local, ONNX Runtime ({})", dir.display());
                    return Ok(AnyEmbedder::Local(engine));
                }
                Err(e) if forced => return Err(format!("embeddings locaux ({}) : {e}", dir.display()).into()),
                Err(e) => eprintln!("embeddings locaux inutilisables ({}) : {e} — Ollama calcule les vecteurs (`astroportd embed-check` dit quoi corriger)", dir.display()),
            }
        }
    }
    #[cfg(not(feature = "onnx"))]
    if a.embed_models.is_some() {
        return Err("--embed-models demande la fonction `onnx` : cargo build --release --features onnx".into());
    }
    Ok(AnyEmbedder::Ollama(ollama.clone()))
}

/// Nom du plan servi par cette base et plans voisins de la station (`planes.json`, ORIGIN seul à défaut).
fn station_planes(own_db: &std::path::Path) -> (String, Vec<astroportd::person_keys::Sibling>) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return ("origin".into(), Vec::new()) };
    let Ok((file, _)) = astroportd::plane::load(&astroportd::plane::default_path(&home), &home) else { return ("origin".into(), Vec::new()) };
    let Ok(planes) = astroportd::plane::plan(&file, &home) else { return ("origin".into(), Vec::new()) };
    let same = |a: &std::path::Path, b: &std::path::Path| a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y);
    let name = planes.iter().find(|p| same(&p.db, own_db)).map_or_else(|| "origin".to_owned(), |p| p.name.clone());
    let siblings = planes.iter().filter(|p| !same(&p.db, own_db)).map(|p| astroportd::person_keys::Sibling { name: p.name.clone(), db: p.db.clone(), game: Some(p.game.clone()) }).collect();
    (name, siblings)
}

/// Domaine sous lequel la station se présente : celui de son HTTPS (`--domain`), sinon celui que sert son reverse proxy
/// (`--public-domain`).
pub(crate) fn public_domain(a: &ServeArgs) -> Option<&String> {
    a.domains.first().or(a.public_domain.as_ref())
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

/// Publication d'un événement signé par la station, sous la forme que les services attendent (`PublishFn`).
fn publish_fn(publisher: &Arc<EventPublisher>) -> astroportd::register::PublishFn {
    let publisher = publisher.clone();
    Arc::new(move |e| {
        let p = publisher.clone();
        Box::pin(async move { astro_bro::publisher::Publisher::publish(&*p, e).await })
    })
}

/// Rapatriement d'un blob depuis la station d'un ami (maillage P2P), pour le partage de fichiers et la sauvegarde privée.
fn blob_prefetch(handle: &astro_p2p::P2pHandle, local: &Arc<AutoBlobStore>) -> astroportd::share_inbox::Prefetch {
    blob_prefetch_max(handle, local, 1 << 30, Duration::from_secs(120))
}

/// Comme [`blob_prefetch`], avec un plafond et un délai : le pair qui annonce plus gros est refusé avant le moindre octet reçu.
fn blob_prefetch_max(handle: &astro_p2p::P2pHandle, local: &Arc<AutoBlobStore>, max_bytes: u64, timeout: Duration) -> astroportd::share_inbox::Prefetch {
    let (handle, local) = (handle.clone(), local.clone());
    Arc::new(move |cid: String, peer: String| {
        let (handle, local) = (handle.clone(), local.clone());
        Box::pin(async move {
            let peer: astro_p2p::PeerId = peer.parse().map_err(|e| format!("PeerID illisible : {e}"))?;
            astro_p2p::blob::fetch_blob(&handle, &[peer], &cid, local.as_ref(), max_bytes, timeout).await.map(|_| ()).map_err(|e| e.to_string())
        })
    })
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
    astroportd::station_domain::arm();
    run_until(a, async {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            () = astroportd::station_domain::RESTART.notified() => {}
        }
    })
    .await?;
    if astroportd::station_domain::restart_requested() {
        // Code d'échec voulu : systemd (Restart=on-failure) relance la station avec sa nouvelle configuration.
        return Err("redémarrage demandé depuis le cockpit : systemd relance la station".into());
    }
    Ok(())
}

/// Comme [`run`], mais s'arrête quand `shutdown` se termine : plusieurs plans d'un même processus partagent un seul signal d'arrêt.
pub async fn run_until(mut a: ServeArgs, shutdown: impl std::future::Future<Output = ()>) -> Result<(), Failure> {
    // Domaine réglé depuis le cockpit ou `init` : repris de station.conf quand la commande n'en donne pas.
    if a.domains.is_empty() {
        use astroportd::station_domain as sd;
        let conf = a.db.parent().map_or_else(|| PathBuf::from("station.conf"), |d| d.join("station.conf"));
        if let Some((domain, email)) = sd::domain_from_conf(&conf).filter(|_| a.public_domain.is_none()) {
            let ports = [80, a.https_listen.port()];
            let state = |p: u16| sd::port_state(p);
            if a.http_listen.is_some() || sd::proxy_from_conf(&conf) {
                // Le Capitaine a dit qu'un reverse proxy sert ce domaine (`web = proxy`, ou écoute imposée par la commande).
                println!("domaine public repris de {} : {domain} (servi par un reverse proxy)", conf.display());
                a.public_domain = Some(domain);
            } else if ports.iter().all(|p| state(*p) == "busy") {
                // 80 et 443 tenus par un autre programme de cette machine : très probablement un reverse proxy (NPM…).
                println!("domaine {domain} : les ports 80 et 443 de cette machine sont tenus par un autre programme (reverse proxy ?) : la station se présente sous https://{domain}");
                a.public_domain = Some(domain);
            } else if ports.iter().all(|p| state(*p) == "free") {
                // Ports libres ici, mais le domaine peut arriver sur un reverse proxy d'une autre machine : la station le
                // demande à son propre nom avant de réclamer un certificat qu'elle n'obtiendrait jamais.
                let d = domain.clone();
                match tokio::task::spawn_blocking(move || sd::who_answers(&d)).await.unwrap_or(sd::Answer::Unknown) {
                    sd::Answer::Other(server) => {
                        println!("domaine {domain} : http://{domain}/ arrive sur un autre serveur web ({server}), pas sur cette machine : un reverse proxy d'une autre machine sert ce domaine ; la station se présente sous https://{domain}");
                        a.public_domain = Some(domain);
                    }
                    _ => {
                        println!("domaine repris de {} : {domain}", conf.display());
                        a.domains = vec![domain];
                        a.acme_email = a.acme_email.take().or(email);
                    }
                }
            } else {
                // Filet : une station qui ne peut pas ouvrir 80/443 resterait en échec au démarrage, donc injoignable. Elle
                // garde son accès local et le cockpit dit pourquoi le domaine attend.
                let blocked: Vec<String> = ports.iter().filter(|p| state(**p) != "free").map(ToString::to_string).collect();
                let why = format!("domaine {domain} enregistré mais pas appliqué : le(s) port(s) {} de cette machine sont pris ou interdits à la station (droits : sudo setcap 'cap_net_bind_service=+ep' sur le binaire). La station reste en accès local ; redémarrez une fois le port libéré, ou indiquez dans le cockpit qu'un reverse proxy sert ce domaine.", blocked.join(" et "));
                eprintln!("{why}");
                sd::set_refused(why);
            }
        }
    }
    // Sans HTTPS par la station, l'interface écoute sur le port 8090 : en local pour un essai, sur toutes les interfaces
    // derrière un reverse proxy (NPM dans Docker ou sur une autre machine doit pouvoir la joindre).
    if a.domains.is_empty() && a.http_listen.is_none() {
        a.http_listen = Some(if a.public_domain.is_some() { astroportd::station_domain::proxy_listen() } else { astroportd::station_domain::local_listen() });
    }
    if let Some(d) = a.public_domain.take() {
        let d = astroportd::station_domain::normalize_domain(&d).map_err(|e| format!("--public-domain : {e}"))?;
        a.public_domain = Some(d);
    }
    // Étanchéité des constellations : les CID d'une UPlanet privée ne sortent jamais de son essaim (même règle que `planes`).
    if a.swarm_key.is_some() && a.kubo.is_some() {
        return Err("--kubo n'appartient qu'à ORIGIN : une UPlanet privée (--swarm-key) n'a jamais de Kubo (ses CID ne doivent pas sortir de son essaim)".into());
    }
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
    if a.kubo.is_none()
        && a.swarm_key.is_none()
        && let Some(n) = astroportd::legacy_ucloud::count_ipfs_only(&a.db).filter(|n| *n > 0)
    {
        let msg = format!("{n} fichier(s) de l'uCloud repris d'Astroport.ONE ne sont que dans IPFS : sans --kubo (ex. --kubo http://127.0.0.1:5001) ils restent illisibles (fiches, photos, miniatures)");
        eprintln!("ATTENTION : {msg}");
        activity.publish("system", "astroportd", msg, None);
    }
    let local = Arc::new(AutoBlobStore::new(a.kubo.as_deref().map(KuboBlobStore::new), FsBlobStore::open(&a.blobs)?).with_pool_peers(a.pool_peers.clone()));
    if !a.pool_peers.is_empty() {
        println!("bassin de blobs partagé : {} (pairs : {})", a.blobs.display(), a.pool_peers.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
    }

    let node_hex = match (&station.discovered, &station.node_secret) {
        (Some(d), _) => d.node.nostr_hex.clone(),
        (None, Some(secret)) => pubkey_of(secret)?,
        (None, None) => String::new(),
    };

    // Constellation = secret partagé de la swarm.key (ẐEN) ou 64 zéros (ORIGIN).
    let uplanet_name = match &a.swarm_key {
        Some(path) => std::fs::read_to_string(path)?.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_owned).ok_or("swarm.key vide")?,
        None => astroportd::init::ORIGIN_UPLANETNAME.to_owned(),
    };
    // Stations relais publiées par la coopérative : la station s'y trouve-t-elle ? Alors elle sert de relais.
    let coop_relays = astroportd::coop_config::p2p_relays(&db, &uplanet_name).await;
    let (mut p2p_guards, p2p_handle, p2p_role) = p2p::start(&a, station.p2p_seed, &coop_relays, &local, &tunnel_policy, &mut exposed_names).await?;
    let blobs = Arc::new(match &p2p_handle {
        Some(h) => P2pBlobStore::new(local.clone(), h.clone(), vec![]),
        None => P2pBlobStore::local_only(local.clone()),
    });

    // Services.
    let keys: Arc<dyn KeySource> = Arc::new(FileKeySource::new(&secrets_dir));
    let ui_keys = keys.clone();
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys.clone(), Duration::from_secs(a.debounce_secs));
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await?;
    let _refresher = circle.spawn_refresher(db.clone(), Duration::from_secs(15));
    let conf_path = a.db.parent().map_or_else(|| PathBuf::from("station.conf"), |d| d.join("station.conf"));
    let policy = astroportd::station_admin::KindsControl::initial_policy(&db, &conf_path).await;
    let relay = Relay::new(db.clone(), circle.clone(), RelayConfig { projection: Some(Arc::new(Projector::plan_lossy)), policy, ..RelayConfig::default() });
    let kinds_control = Arc::new(astroportd::station_admin::KindsControl::new(db.clone(), &conf_path, relay.clone()));
    if let Some(handle) = &p2p_handle {
        p2p::offer_relay(&a, handle, &relay, &mut p2p_guards).await?;
    }
    let mut dav_config = DavConfig::new(station_secret);
    // Un fichier qui ne se déchiffre plus pendant son envoi (blob altéré ou illisible) est dit au Capitaine, jamais tu.
    let read_bus = activity.clone();
    dav_config.on_read_failure = Some(Arc::new(move |f: &astro_dav::ReadFailure| {
        let what = if f.integrity { "fichier altéré (ne se déchiffre plus)" } else { "fichier illisible sur le disque" };
        read_bus.publish_to(astro_core::Audience::Captain, "alert", "ucloud", format!("{what} : {} ({}) — {}", f.path, f.cid.chars().take(16).collect::<String>(), f.reason), None);
    }));
    // Première connexion authentifiée d'un MULTIPASS créé ici = preuve qu'il a reçu ses clés (crate::member_primo).
    let proofs = astroportd::member_primo::Proofs::new(db.clone());
    dav_config.on_login = Some(Arc::new(move |hex: &str| proofs.note(hex)));
    dav_config.ipfs_gateway = a.ipfs_gateway;
    if let Some(domain) = public_domain(&a) {
        dav_config.public_base_url = Some(format!("https://{domain}")); // liaison NIP-98 derrière notre propre TLS
    }
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let stop_watch = stop_rx.clone();
    let worker;
    let on_upload;
    let share_fn: Option<astroportd::identity_links::ShareFn>;
    let contacts_cell: astroportd::identity_links::HookCell = Arc::default();
    let thumbs_fn: Option<astroportd::ui_backend::ThumbFn>;
    let pending_fn: Option<astroportd::ui_backend::PendingShareFn>;
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
    let chat_keys = keys.clone();
    let people_keys = keys.clone();
    let account_keys = keys.clone();
    let agent_keys = keys.clone();
    let agent_keys2 = keys.clone();
    let pull_keys = keys.clone();
    let invites_keys = keys.clone();
    let inbox_keys = keys.clone();
    let backup_keys = keys.clone();
    let assist_keys = keys.clone();
    let captains_keys = keys.clone();
    // Sans option : si les deux modèles sont aux emplacements habituels (et le binaire compilé avec `onnx`), l'inférence locale
    // s'active toute seule ; `astroportd face-check` dit ce qui manque.
    // Moteur FaceID : une option de ligne de commande l'impose (réglage figé dans l'interface) ; sinon le choix du Capitaine
    // (enregistré) l'emporte ; sinon, si les deux modèles sont aux emplacements habituels (binaire compilé avec `onnx`),
    // l'inférence locale s'active toute seule ; `astroportd face-check` dit ce qui manque. Le Capitaine peut ensuite
    // basculer à chaud (ONNX local, Brain distant, arrêt) sans redémarrer.
    use astroportd::station_admin::FaceControl;
    let (initial_mode, locked, initial_engine) = face::initial(&a, &db).await?;
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
        let for_pending = face.clone();
        pending_fn = Some(Arc::new(move |owner: String, friend: String, accept: bool| {
            let face = for_pending.clone();
            Box::pin(async move { face.resolve_pending(&owner, &friend, accept).await.map_err(|e| e.to_string()) })
        }));
        worker = Some(tokio::spawn(face.run_worker(Duration::from_secs(5), stop_rx)));
        println!("FaceCloud actif ({})", face_label.as_deref().unwrap_or("moteur arrêté : réglable par le Capitaine"));
    }
    let inbox_blobs = blobs.clone();
    let backup_blobs = blobs.clone();
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay, publisher: Some(publisher.clone()), config: dav_config, on_upload });
    // Reprise d'un dossier d'un autre serveur WebDAV (Nextcloud…), lancée depuis l'interface (onglet « Sources » du uCloud).
    let pull = astroportd::webdav_pull::PullService::new(dav.clone(), pull_keys, Some(activity.clone()));
    // Rangement du uCloud par type (Documents, Images, Musique, Vidéos, Archives), lancé depuis l'onglet « Ranger ».
    let organize = astroportd::organize::OrganizeService::new(dav.clone(), Some(activity.clone()));
    // Annuaire : fiches vCard de /Contacts ↔ visages ↔ MULTIPASS (CardDAV est servi par le routeur WebDAV).
    let links = Arc::new(astroportd::identity_links::IdentityLinks::new(db.clone(), dav.clone(), share_fn, Some(activity.clone())));
    let _ = contacts_cell.set(links.clone());


    // BRO LifeOS : mémoire sémantique (Ollama), services de la station, bus de commandes par messages privés.
    let ollama = a.ollama.as_deref().map(|url| Arc::new(OllamaClient::new(url, &a.embed_model, &a.chat_model, Duration::from_secs(60))));
    let embedder = ollama.as_ref().map(|o| local_embedder(&a, o)).transpose()?;
    let memory = ollama.as_ref().zip(embedder).map(|(o, e)| Arc::new(MemoryStore::new(db.clone(), Arc::new(e), o.clone(), Some(activity.clone()))));
    let hardware = tokio::task::spawn_blocking(Hardware::detect).await?;
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
    let ui_chain = chain.clone();
    let billet_chain = chain.clone();
    let peer_id = p2p_handle.as_ref().map(|h| h.peer_id().to_string()).or_else(|| station.discovered.as_ref().map(|d| d.node.peer_id.clone()));
    let services_peer = peer_id.clone();
    let info = StationInfo { node_hex: node_hex.clone(), peer_id, domain: public_domain(&a).cloned(), started: std::time::Instant::now() };
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
    // Envoi de fichiers aux proches et réception de ceux qu'ils envoient depuis d'autres stations (blob rapatrié par le maillage).
    let sharing = Arc::new(astroportd::sharing::Sharing::new(db.clone(), sharing_keys, mesh_outbox.clone(), Some(publisher.clone()), services_peer.clone()));
    {
        let mut inbox = astroportd::share_inbox::ShareInbox::new(db.clone(), inbox_blobs, inbox_keys, Some(publisher.clone()), activity.clone(), notify.clone());
        if let Some(handle) = &p2p_handle {
            inbox = inbox.with_prefetch(blob_prefetch(handle, &local));
        }
        background.push(tokio::spawn(Arc::new(inbox).run(Duration::from_secs(20), stop_watch.clone())));
        println!("partage : fichiers des proches reçus par le maillage (un passage toutes les 20 s)");
    }
    // Réveil du crawl : un module qui demande une clé (secours d'un compte) n'attend pas le passage suivant.
    let crawl_nudge = Arc::new(tokio::sync::Notify::new());
    let beacon_nudge = Arc::new(tokio::sync::Notify::new());
    // Sauvegarde privée chez un ami : le proche doit accepter ; le blob part chiffré, sans sa clé.
    let backup = {
        // Station d'accueil annoncée dans le DID d'un compte rétabli ici : `<PeerID>:<NODE_HEX>` (rien si l'un des deux manque).
        let home = services_peer.as_ref().filter(|_| astro_nostr_proto::is_hex64(&node_hex)).map(|peer| format!("{peer}:{node_hex}"));
        let mut b = astroportd::private_backup::PrivateBackup::new(db.clone(), backup_keys, mesh_outbox.clone(), backup_blobs, sharing.clone(), activity.clone())
            .with_station(Some(publisher.clone()), services_peer.clone())
            .with_home(home, Some(publish_fn(&event_publisher)))
            .with_accounts_dir(station.nostr_dir.clone())
            .with_notify(notify.clone())
            // Secours par les gardiens : la clé NODE demande et reçoit ; le gardien voit le domaine (sinon le nom de machine).
            .with_rescue(station.node_secret.as_ref().map(|s| **s), Some(public_domain(&a).cloned().unwrap_or_else(astroportd::init::hostname)), Some(crawl_nudge.clone()));
        if let Some(handle) = &p2p_handle {
            b = b.with_prefetch(blob_prefetch(handle, &local));
            // Copies de pièces jointes d'un ami : jamais plus que la taille d'une pièce jointe, annoncée avant tout octet reçu.
            b = b.with_attachment_prefetch(blob_prefetch_max(handle, &local, astro_dav::share::ATTACHMENT_MAX as u64, Duration::from_secs(60)));
        }
        Arc::new(b)
    };
    // Assistance entre Capitaines amis : demande, réponse ; l'accès n'est ouvert que lorsqu'un ami accepte.
    let assist = Arc::new(astroportd::assist::Assist::new(db.clone(), assist_keys, mesh_outbox.clone(), tunnel_policy.clone(), exposed_names.clone(), services_peer.clone(), activity.clone()).with_notify(notify.clone()));
    background.push(tokio::spawn(assist.clone().run(Duration::from_secs(30), stop_watch.clone())));
    let parts = tasks::Parts {
        a: &a,
        db: db.clone(),
        activity: activity.clone(),
        stop_watch: stop_watch.clone(),
        dav: dav.clone(),
        local: local.clone(),
        relay: relay.clone(),
        p2p_handle: p2p_handle.clone(),
        services_peer: services_peer.clone(),
        uplanet_name: uplanet_name.clone(),
        secrets_dir: secrets_dir.clone(),
        nostr_dir: station.nostr_dir.clone(),
        notify: notify.clone(),
        node_hex: node_hex.clone(),
        node_secret: station.node_secret.as_ref().map(|s| **s),
        node_publisher: node_publisher.clone(),
        crawl_nudge: crawl_nudge.clone(),
        beacon_nudge: beacon_nudge.clone(),
    };
    tasks::ssh_sync(&parts, &tunnel_policy, &mut background);
    // Adresses de la coopérative : la station et chaque MULTIPASS hébergé pointent vers leur espace IPNS (DNSLink OVH).
    let dnslink = Arc::new(astroportd::dnslink::Dnslink::new(db.clone(), uplanet_name.clone(), services_peer.clone(), astroportd::init::hostname(), activity.clone()));
    tasks::dnslink_sync(&parts, &dnslink, &mut background);
    background.push(tokio::spawn(backup.clone().run(Duration::from_secs(60), stop_watch.clone())));
    tasks::exif_backfill(&parts, &mut background);
    // Documents demandés par `#upload` : rapatriés, chiffrés et déposés dans /Uploads (le demandeur est prévenu).
    tasks::crawl(&parts, &relay_client, &mut background);
    tasks::funds(&parts, funds_chain, &squid_urls, &mut background);
    background.push(tokio::spawn(Arc::new(UploadWorker::new(dav.clone(), activity.clone(), notify)).run(Duration::from_secs(10), stop_watch.clone())));
    println!("BRO : traitement des #upload actif (dépôt dans /Uploads)");
    tasks::bro_sweep(&parts, &memory, &mut background);
    let ui_wallets = Arc::new(astroportd::coop_wallets::Wallets::new(uplanet_name.to_string(), db.clone()));
    // Ğ1Billets : vérifier la valeur d'un billet papier et la transférer vers un compte de l'UPlanet (page publique /scan).
    let billets = billet_chain.map(|chain| {
        let primal = (!squid_urls.is_empty()).then(|| Arc::new(astroportd::funds::SquidPrimal::new(squid_urls.clone())));
        let svc = astroportd::billet::Billets::new(db.clone(), chain, ui_wallets.clone(), primal, uplanet_name.to_string(), activity.clone());
        // Créer des billets exige les comptes (le portefeuille de l'émetteur finance), la clé du NODE (elle chiffre les parts gardées
        // pour le retour automatique) et de quoi publier le témoin.
        Arc::new(match (&station.nostr_dir, &station.node_secret) {
            (Some(dir), Some(secret)) => svc.with_issuing(astroportd::billet::Issuing { accounts_dir: dir.clone(), node_secret: zeroize::Zeroizing::new(**secret), public_base: public_domain(&a).map(|d| format!("https://{d}")), publish: publish_fn(&event_publisher) }),
            _ => svc,
        })
    });
    if let Some(b) = &billets {
        background.push(tokio::spawn(b.clone().run(stop_watch.clone())));
        println!("Ğ1Billets : vérification et encaissement actifs → /scan{}", if squid_urls.is_empty() { " (sans indexeur Ğ1 --squid-url : l'origine du billet et le destinataire ne peuvent pas être vérifiés, l'encaissement est refusé)" } else { "" });
    }
    tasks::nightly(&parts, &memory, &backup, &billets, &mut background).await?;
    // Inscription d'un MULTIPASS : `uplanet_name` (voir plus haut) désigne la constellation.
    let registrar = {
        let secret = station.secret.clone();
        let publish = publish_fn(&event_publisher);
        let captain = station.discovered.as_ref().and_then(|d| d.captain()).map(|m| m.hex.clone());
        let limits = astroportd::register::Limits { open: !a.no_register, max_accounts: a.max_accounts, per_ip_per_hour: a.register_per_hour };
        Arc::new(
            astroportd::register::Registrar::new(db.clone(), secrets_dir.clone(), uplanet_name.clone(), Arc::new(move |hex| astro_dav::auth::derive_dav_token(&secret, hex)), limits)
                .with_captain(captain)
                .require_captain()
                .with_player_file(station.nostr_dir.as_ref().and_then(|d| d.parent()).map(|g| g.join("players/.current/.player")))
                .with_station_domain(public_domain(&a).cloned())
                .with_relays(public_domain(&a).map(|d| format!("wss://{d}")).into_iter().collect())
                .with_publisher(publish, Some(activity.clone())),
        )
    };
    // Station sans Capitaine : un code d'installation, écrit dans le journal seulement, ouvre la page /welcome.
    let setup_code = if registrar.awaiting_captain().await {
        let url = public_domain(&a).map_or_else(|| format!("http://{}", a.http_listen.unwrap_or_else(astroportd::station_domain::local_listen)), |d| format!("https://{d}"));
        let code = astroportd::first_run::SetupCode::new(Box::new(move |code| println!("\n⚓ Station sans Capitaine : ouvrez {url}/welcome et saisissez le code d'installation {code}\n   (ou, sur cette machine : astroportd multipass create <votre@email> --captain)\n")))?;
        Some(Arc::new(code))
    } else {
        None
    };
    println!("inscription web des MULTIPASS : {} (constellation {}, max {} comptes, {} par heure et par adresse) → /register", if a.no_register { "fermée" } else { "ouverte" }, registrar.constellation(), a.max_accounts, a.register_per_hour);
    // Annuaire des stations de l'UPlanet et balise 30850 (places libres, position, matériel) : relai local, `--relay`
    // et relai de la coopérative `wss://relay.<zone OVH>`. Sans clé du NODE, pas de balise, l'annuaire reste lisible.
    let stations = Arc::new(astroportd::beacon::Stations::new(db.clone(), uplanet_name.to_string(), services_peer.clone()));
    // Le relai accepte les balises des stations sœurs (même `swarm_id`) : il sert d'annuaire s'il est le relai commun.
    {
        let (circle, stations) = (circle.clone(), stations.clone());
        background.push(tokio::spawn(async move {
            match stations.swarm_id().await {
                Ok(id) => circle.set_swarm_id(id),
                Err(e) => eprintln!("annuaire des stations : UPlanet non identifiée ({e}) : les balises des autres stations sont refusées"),
            }
        }));
    }
    if let Some(secret) = station.node_secret.as_ref().map(|s| **s) {
        let mut client = WsClient::new(Duration::from_secs(20));
        if let Some(s) = signer.clone() {
            client = client.with_signer(s);
        }
        let beacon = Arc::new(astroportd::beacon::Beacon {
            db: db.clone(),
            node_secret: secret,
            publisher: event_publisher.clone(),
            client,
            relays: a.relays.clone(),
            registrar: registrar.clone(),
            stations: stations.clone(),
            name: public_domain(&a).cloned().unwrap_or_else(astroportd::init::hostname),
            public_url: public_domain(&a).map(|d| format!("https://{d}")),
            origin: a.lat.zip(a.lon),
            hardware,
            weekly_rent: astro_core::Amount(a.weekly_rent),
            weekly_costs: astro_core::Amount(a.weekly_costs),
            station_home: a.db.parent().map(std::path::Path::to_path_buf),
            nudge: beacon_nudge.clone(),
            activity: Some(activity.clone()),
            p2p: p2p_handle.clone(),
            p2p_announce: p2p_role.announce.clone(),
        });
        background.push(tokio::spawn(beacon.run(stop_watch.clone())));
        println!("balise de station (30850) : publiée au démarrage, à chaque changement de places et toutes les 6 h{}", if a.lat.is_some() { ", position arrondie à la case UMAP" } else { ", sans position (--lat/--lon absents)" });
    }
    // Invitations : le lien `/i/<jeton>` crée le MULTIPASS du destinataire, l'abonne à l'expéditeur et range les photos.
    let invites = {
        let publish = publish_fn(&event_publisher);
        Arc::new(astroportd::invites::Invites::new(db.clone(), dav.blobs.clone(), invites_keys, registrar.clone(), publish).with_links(links.clone()).with_publisher(publisher.clone()).with_activity(activity.clone()).with_remote(sharing.clone()))
    };
    match invites.migrate_legacy().await {
        Ok(0) => {}
        Ok(n) => println!("invitations : {n} ancienne(s) invitation(s) migrée(s) vers le suivi du uCloud vivant"),
        Err(e) => eprintln!("invitations : migration de l'ancien format impossible : {e:?}"),
    }
    let welcome = Arc::new(astroportd::welcome::Welcome::new(db.clone(), invites.clone(), Some(activity.clone())));
    let account_publish = publish_fn(&event_publisher);
    // Agents du membre (kind 31903 `d = agents`) et serveur MCP : un agent demande, le membre accorde ou refuse.
    let agents = Arc::new(astroportd::agents::Agents::new(db.clone(), agent_keys, publish_fn(&event_publisher)).with_activity(activity.clone()));
    let member_ai = Arc::new(astroportd::member_ai::MemberAi::new(db.clone(), agent_keys2, publish_fn(&event_publisher)));
    let forwards = Arc::new(astroportd::forwards::Forwards::new(p2p_handle.clone()));
    let ssh_home = a.ssh_dir.clone().or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh")));
    let identity = ssh_home.as_ref().map(|d| d.join("id_ed25519")).filter(|p| p.exists());
    let known_hosts = a.db.parent().map_or_else(|| PathBuf::from("console_known_hosts"), |p| p.join("console_known_hosts"));
    let consoles = Arc::new(astroportd::console::Consoles::new(identity, known_hosts, Some(activity.clone())));
    let consoles_ui = consoles.clone();
    let assist_admin = admin::assist_admin(&assist, &forwards, &consoles);
    // Maillage (stations, bootstrap gardé) et Capitaines (suivre, services des amis, accès à chaud) pour l'écran du Capitaine.
    let mut mesh = astroportd::mesh_admin::Mesh::new(db.clone(), p2p_handle.clone(), a.p2p_bootstrap.clone(), station_planes(&a.db).0, activity.clone())
        .with_relays(a.p2p_use_relay.clone(), Some(uplanet_name.to_string()), p2p_role.relay_server)
        .with_directory(stations.clone(), beacon_nudge.clone());
    // Bail géographique : une seule station de l'UPlanet tient chaque quartier (UMAP, SECTOR, REGION), la plus proche.
    if let Some(handle) = &p2p_handle {
        let geo = astroportd::geo_lease::GeoLease::new(db.clone(), handle.peer_id().to_string(), a.lat.zip(a.lon), activity.clone());
        tokio::spawn(geo.clone().run(handle.clone(), stop_watch.clone()));
        mesh = mesh.with_geo(geo);
    }
    let mesh = Arc::new(mesh);
    background.push(tokio::spawn(mesh.clone().keep_alive(Duration::from_secs(300), stop_watch.clone())));
    // Cartes de station : chacune décrit la sienne par le maillage ; le dossier des `12345.json` des pairs se remplit des
    // stations connectées (synchronisation N¹/N², envoi des partages, réplication, routage), même sans Astroport.ONE.
    if let Some(h) = &p2p_handle {
        let card = Arc::new(astroportd::swarm_cards::Card {
            db: db.clone(),
            handle: h.clone(),
            hostname: astroportd::init::hostname(),
            node_hex: node_hex.clone(),
            my_relay: public_domain(&a).map(|d| format!("wss://{d}")),
            origin: a.lat.zip(a.lon),
        });
        match card.serve() {
            Ok(answers) => p2p_guards.push(Box::new(answers)),
            Err(e) => eprintln!("essaim : carte de station non servie : {e}"),
        }
        match a.swarm_dir.clone().or_else(default_swarm_dir) {
            Some(dir) => background.push(tokio::spawn(astroportd::swarm_cards::keep(h.clone(), dir, stop_watch.clone()))),
            None => eprintln!("essaim : HOME introuvable et --swarm-dir absent : les cartes des stations ne sont pas gardées"),
        }
    }
    let captains = Arc::new(astroportd::captains::Captains::new(db.clone(), captains_keys, Some(publish_fn(&event_publisher)), p2p_handle.clone(), forwards.clone(), Some(consoles.clone()), activity.clone()));
    let (mesh_admin, captains_admin) = admin::mesh_admins(&mesh, &captains);
    let account_origin = public_domain(&a).map(|d| format!("https://{d}"));
    let account_svc = Arc::new(astroportd::account::AccountService::new(db.clone(), secrets_dir.clone(), account_keys, station.secret.clone(), account_publish, account_origin).with_activity(activity.clone()));
    let address_book = Arc::new(astroportd::address_book::AddressBook::new(db.clone(), dav.clone(), links.clone(), Some(account_svc.clone())));
    // Une personne, plusieurs UPlanet : les plans voisins de cette station (planes.json), lus en lecture seule.
    let people = {
        let (plane_name, siblings) = station_planes(&a.db);
        Arc::new(astroportd::person_keys::PersonKeys::new(db.clone(), plane_name, siblings, people_keys, publish_fn(&event_publisher)))
    };
    // Chat privé 1 à 1 : un ami hébergé ici reçoit sur le relai local, les autres par le maillage ; la copie de l'expéditeur reste ici.
    // Visio : STUN et relais d'appel (TURN) en UDP sur le port de l'interface. Le relais ne sert qu'aux membres de la station et à
    // leurs amis N¹ (identifiants signés par `turn_secret`). Un échec n'arrête pas la station : les appels restent possibles sur
    // un même réseau ou en IPv6, et la raison est dite.
    let mut turn_secret = [0u8; 32];
    getrandom::fill(&mut turn_secret).map_err(|e| format!("aléa indisponible : {e}"))?;
    let stun_default = || {
        if !a.domains.is_empty() {
            return a.https_listen;
        }
        let local = a.http_listen.unwrap_or_else(astroportd::station_domain::local_listen);
        // Derrière un proxy, la visio doit rester joignable de dehors (même port, en UDP), même si l'interface écoute en local.
        if a.public_domain.is_some() && local.ip().is_loopback() { SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), local.port()) } else { local }
    };
    let stun_port = match (!a.no_stun).then(|| a.stun_listen.unwrap_or_else(stun_default)) {
        Some(addr) => match astroportd::turn::bind(addr).await {
            Ok(socks) => {
                let bound: Vec<String> = socks.iter().filter_map(|s| s.local_addr().ok()).map(|a| a.to_string()).collect();
                let port = socks.first().and_then(|s| s.local_addr().ok()).map_or(addr.port(), |a| a.port());
                let call_relay = astroportd::turn::Relay::new(turn_secret);
                if a.no_turn {
                    println!("visio : STUN sur udp/{} (relais d'appel coupé : --no-turn)", bound.join(", "));
                    background.push(tokio::spawn(astroportd::turn::serve_rebinding(socks, call_relay.stun_only(), stop_watch.clone(), astroportd::turn::Rebind::local(addr))));
                } else {
                    println!("visio : STUN et relais d'appel sur udp/{}, réservés aux membres et à leurs amis N¹ (ouvrez le port UDP {port} sur la box)", bound.join(", "));
                    background.push(tokio::spawn(astroportd::turn::serve_rebinding(socks, call_relay, stop_watch.clone(), astroportd::turn::Rebind::local(addr))));
                }
                Some(port)
            }
            Err(e) => {
                let msg = format!("visio : STUN impossible sur udp/{addr} ({e}) : appels limités au réseau local et à l'IPv6 (--stun-listen pour un autre port)");
                eprintln!("{msg}");
                activity.publish_to(astro_core::Audience::Captain, "alert", "visio", msg, None);
                None
            }
        },
        None => None,
    };
    let turn = stun_port.filter(|_| !a.no_turn).map(|p| (turn_secret, p));
    let chat = {
        let (local_pub, mesh, hosted_db) = (publish_fn(&event_publisher), mesh_outbox.clone(), db.clone());
        let publish_local: astroportd::chat::DeliverFn = {
            let p = local_pub.clone();
            Arc::new(move |_to, ev| p(ev))
        };
        let deliver: astroportd::chat::DeliverFn = Arc::new(move |to: String, ev: astro_nostr_proto::Event| {
            let (local_pub, mesh, db) = (local_pub.clone(), mesh.clone(), hosted_db.clone());
            Box::pin(async move {
                let peer = to.clone();
                let hosted = db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [peer], |r| r.get::<_, i64>(0)).is_ok())).await.unwrap_or(false);
                if hosted {
                    local_pub(ev).await
                } else {
                    astro_facecloud::Outbox::send(&*mesh, &to, ev).await.map_err(|e| e.to_string())
                }
            })
        });
        // Présence visio (sur option, amis mutuels) : même remise aux stations des amis que le chat.
        let presence = Arc::new(astroportd::presence::Presence::new(db.clone(), chat_keys.clone(), activity.clone(), deliver.clone(), publish_local.clone()));
        background.push(tokio::spawn(presence.clone().run(relay.subscribe(), stop_watch.clone())));
        let chat = Arc::new(astroportd::chat::Chat::new(db.clone(), chat_keys, deliver, publish_local, activity.clone()).with_people(people.clone()).with_turn_opt(turn));
        background.push(tokio::spawn(chat.clone().run(Duration::from_secs(3), relay.subscribe(), stop_watch.clone())));
        println!("chat : conversations privées 1 à 1 (NIP-17), réception dès l'arrivée (contrôle toutes les 3 s) ; présence visio sur option");
        (chat, presence)
    };
    let (chat, presence) = chat;
    let charter_accept = Arc::new(astroportd::charter_accept::CharterAccept::new(db.clone(), ui_keys, publish_fn(&event_publisher), uplanet_name.to_string()));
    let constellation = astroportd::constellation::Constellation::new(db.clone(), ui_chain, ui_wallets.clone(), uplanet_name.to_string(), squid_urls.clone()).with_acceptances(charter_accept.clone());
    let constellation = Arc::new(match &billets {
        Some(b) => constellation.with_billets(b.clone()),
        None => constellation,
    });
    background.push(tokio::spawn(constellation.clone().run(stop_watch.clone())));
    if let Some(bad) = a.ice_servers.iter().find(|u| !["stun:", "stuns:", "turn:", "turns:"].iter().any(|p| u.starts_with(p)) || u.len() > 200 || u.chars().any(char::is_whitespace)) {
        return Err(format!("--ice-server « {bad} » : stun:hôte:port ou turn:hôte:port attendu").into());
    }
    let admin_fn = Arc::new(astroportd::station_admin::Admin { face: face_control, kinds: kinds_control, tunnels: Some((tunnel_policy.clone(), exposed_names.clone())), targets: service_targets(&a), dnslink: Some(dnslink.clone()), assist: Some(assist_admin), tasks: Some(db.clone()), mesh: Some(mesh_admin), captains: Some(captains_admin) }).handler();
    // Prise de contrôle locale (`astroportd tunnel …`) : la politique des tunnels et le routage vers les services des autres stations.
    let swarm_route = Arc::new(astroportd::swarm_route::SwarmRoute::new(stations.clone(), p2p_handle.clone(), forwards.clone(), a.lat.zip(a.lon)));
    let mut control_sections: std::collections::BTreeMap<&'static str, astroportd::ui_backend::AdminFn> = std::collections::BTreeMap::new();
    control_sections.insert("tunnels", admin_fn.clone());
    control_sections.insert("tunnel", Arc::new(move |_section, body| {
        let route = swarm_route.clone();
        Box::pin(async move { route.act(&body.unwrap_or(serde_json::Value::Null)).await })
    }));
    let control_path = astroportd::control::socket_path(&a.db);
    match astroportd::control::serve(control_path.clone(), control_sections) {
        Ok(task) => {
            println!("contrôle local : {} (astroportd tunnel …)", control_path.display());
            p2p_guards.push(Box::new(task));
        }
        Err(e) => eprintln!("contrôle local indisponible : {e}"),
    }
    let mut ui = StationUi::new(services.clone(), dav.clone(), memory, ollama, true).with_setup(setup_code).with_conf_path(conf_path.clone()).with_ice(stun_port, turn.map(|t| t.0), a.ice_servers.clone()).with_presence(presence).with_chat(chat).with_people(people.clone()).with_console(consoles_ui).with_agents(agents).with_ai(member_ai).with_pull(pull).with_organize(organize).with_registrar(registrar.clone()).with_stations(stations.clone()).with_invites(invites).with_welcome(welcome).with_contacts(links.clone()).with_account(account_svc).with_book(address_book)
        .with_admin(admin_fn.clone());
    ui = ui.with_feedback(Arc::new(astroportd::feedback::Feedback::new(db.clone(), uplanet_name.to_string(), activity.clone()))).with_oc(Arc::new(astroportd::oc_member::OcMember::new(db.clone(), uplanet_name.to_string())));
    if let Some(f) = thumbs_fn {
        ui = ui.with_face_thumbs(f);
    }
    if let Some(f) = pending_fn {
        ui = ui.with_pending_shares(f);
    }
    if let Some(b) = billets {
        ui = ui.with_billet(Arc::new(move |ip, op, body| {
            let b = b.clone();
            Box::pin(async move { b.handle(ip, op, &body).await })
        }));
    }
    let site = Arc::new(astroportd::public_site::Site::new(dav.clone(), uplanet_name.to_string()));
    ui = ui.with_site(site.clone());
    let ui = Arc::new(ui::wire(ui, &db, &event_publisher, &uplanet_name, &sharing, &backup, &constellation));

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
    let public_origin = public_domain(&a).map(|d| format!("https://{d}"));
    let mut app = astro_dav::router(dav.clone())
        .merge(relay.clone().router())
        .merge(astro_ui::web::static_router())
        .merge(site.clone().router())
        .merge(astro_ui::api::router(ui, activity.clone(), public_origin));
    if !routes.is_empty() {
        app = app.merge(proxy_router(routes, Duration::from_secs(120)));
    }
    // `/` : application web pour un navigateur, NIP-11 et WebSocket pour les clients Nostr.
    let app = app.layer(axum::middleware::from_fn(astro_ui::web::negotiate));
    // Un nom d'hôte `<nom>.<zone>` reçoit le site public de ce compte (couche la plus externe : elle passe avant tout le reste).
    let app = app.layer(axum::middleware::from_fn_with_state(site, astroportd::public_site::host_layer));
    // Derrière un reverse proxy : l'adresse du visiteur (limites par IP de l'inscription, du relai…) est celle que le proxy
    // a vue, pas la sienne.
    let app = if a.domains.is_empty() && a.public_domain.is_some() { app.layer(axum::middleware::from_fn(astroportd::station_domain::client_ip_from_proxy)) } else { app };

    // Écoute : HTTPS automatique avec un domaine, sinon HTTP en clair.
    let frontend = if a.domains.is_empty() {
        let addr = a.http_listen.unwrap_or_else(astroportd::station_domain::local_listen);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        match &a.public_domain {
            Some(d) => println!("astroportd à l'écoute sur http://{} derrière un reverse proxy : se présente sous https://{d} (le proxy tient le TLS et renvoie ce domaine ici, WebSockets activés)", listener.local_addr()?),
            None => println!("astroportd à l'écoute sur http://{} (sans domaine : pas de TLS)", listener.local_addr()?),
        }
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });
        None
    } else {
        let mut acme = AcmeConfig::new(a.domains.clone(), a.acme_email.clone());
        acme.directory_url = (if a.acme_staging { LETS_ENCRYPT_STAGING } else { LETS_ENCRYPT }).to_owned();
        let manager = AcmeManager::new(acme, KvCertStore::new(db.clone()));
        services.set_tls(a.domains.clone(), manager.status.clone());
        let http = a.http_listen.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 80)));
        let front = Frontend::start(manager, app, http, a.https_listen).await?;
        println!("astroportd : HTTP {} (défi ACME + redirection) et HTTPS {} pour {}", front.http_addr, front.https_addr, a.domains.join(", "));
        Some(front)
    };

    shutdown.await;
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

/// Où écoute chaque service offert au maillage : SSH, ComfyUI et Ollama (sauf `none`), puis ceux de `--p2p-expose`.
fn service_targets(a: &ServeArgs) -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = a.p2p_expose.iter().filter_map(|s| s.split_once('=')).map(|(n, t)| (n.to_owned(), t.to_owned())).collect();
    for (name, target) in [("ssh", &a.p2p_ssh), ("comfyui", &a.p2p_comfyui), ("ollama", &a.p2p_ollama)] {
        if target != "none" && !all.iter().any(|(n, _)| n == name) {
            all.push((name.to_owned(), target.clone()));
        }
    }
    all
}
