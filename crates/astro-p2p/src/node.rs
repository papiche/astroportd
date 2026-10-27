//! Nœud libp2p : transport, comportements (relais, DCUtR, identify, flux) et boucle d'événements.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::core::upgrade::Version;
use libp2p::identity::Keypair;
use libp2p::pnet::{PnetConfig, PreSharedKey};
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{ConnectionId, NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, SwarmBuilder, Transport, dcutr, identify, noise, ping, relay, tcp, yamux};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::P2pError;
use crate::identity::keypair_from_seed;

/// Protocole d'identification annoncé aux pairs.
const IDENTIFY_PROTOCOL: &str = "/astro/id/1.0.0";

/// Réglages du nœud.
#[derive(Clone)]
pub struct P2pConfig {
    /// Graine ed25519 (clé IPFS de la station).
    pub seed: [u8; 32],
    /// Clé de réseau privé. Avec elle, seul TCP est utilisé (le chiffrement PSK ne s'applique pas à QUIC) ;
    /// sans elle, TCP et QUIC (hole punching UDP) sont actifs.
    pub psk: Option<PreSharedKey>,
    /// Adresses d'écoute (`/ip4/0.0.0.0/tcp/4101`…).
    pub listen: Vec<Multiaddr>,
    /// Pairs à joindre au démarrage (avec `/p2p/<PeerID>`).
    pub bootstrap: Vec<Multiaddr>,
    /// Adresses publiques connues, annoncées aux pairs (nécessaires à un serveur de relais).
    pub external: Vec<Multiaddr>,
    /// Rendre service de relais (circuit relay v2) aux stations derrière une box.
    pub relay_server: bool,
}

impl P2pConfig {
    /// Configuration minimale : écoute TCP sur un port libre de la boucle locale.
    pub fn local(seed: [u8; 32]) -> Self {
        Self { seed, psk: None, listen: vec![Multiaddr::from(std::net::Ipv4Addr::LOCALHOST).with(Protocol::Tcp(0))], bootstrap: Vec::new(), external: Vec::new(), relay_server: false }
    }
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay_client: relay::client::Behaviour,
    relay_server: Toggle<relay::Behaviour>,
    dcutr: dcutr::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    stream: libp2p_stream::Behaviour,
}

fn behaviour(key: &Keypair, relay_client: relay::client::Behaviour, relay_server: bool) -> Behaviour {
    let peer = key.public().to_peer_id();
    let relay_config = relay::Config { max_circuit_duration: Duration::from_secs(3600), max_circuit_bytes: 0, ..relay::Config::default() };
    Behaviour {
        relay_client,
        relay_server: relay_server.then(|| relay::Behaviour::new(peer, relay_config)).into(),
        dcutr: dcutr::Behaviour::new(peer),
        identify: identify::Behaviour::new(identify::Config::new(IDENTIFY_PROTOCOL.to_owned(), key.public())),
        ping: ping::Behaviour::default(),
        stream: libp2p_stream::Behaviour::new(),
    }
}

fn build_swarm(cfg: &P2pConfig, keypair: Keypair) -> Result<Swarm<Behaviour>, P2pError> {
    let relay_server = cfg.relay_server;
    let build_err = |e: &dyn std::fmt::Display| P2pError::Build(e.to_string());
    macro_rules! finish {
        ($builder:expr) => {
            $builder
                .with_relay_client(noise::Config::new, yamux::Config::default)
                .map_err(|e| build_err(&e))?
                .with_behaviour(|key, relay_client| behaviour(key, relay_client, relay_server))
                .map_err(|e| build_err(&e))?
                .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
                .build()
        };
    }
    Ok(match cfg.psk {
        Some(psk) => finish!(
            SwarmBuilder::with_existing_identity(keypair)
                .with_tokio()
                .with_other_transport(move |key| {
                    let noise_config = noise::Config::new(key)?;
                    let base = tcp::tokio::Transport::new(tcp::Config::default().nodelay(true));
                    let private = base.and_then(move |socket, _| async move { PnetConfig::new(psk).handshake(socket).await });
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(private.upgrade(Version::V1Lazy).authenticate(noise_config).multiplex(yamux::Config::default()))
                })
                .map_err(|e| build_err(&e))?
        ),
        None => finish!(
            SwarmBuilder::with_existing_identity(keypair)
                .with_tokio()
                .with_tcp(tcp::Config::default().nodelay(true), noise::Config::new, yamux::Config::default)
                .map_err(|e| build_err(&e))?
                .with_quic()
        ),
    })
}

#[derive(Default)]
struct Shared {
    listen_addrs: Vec<Multiaddr>,
    connected: HashMap<PeerId, usize>,
}

enum Command {
    Listen(Multiaddr, oneshot::Sender<Result<Multiaddr, P2pError>>),
    Dial(Multiaddr, oneshot::Sender<Result<PeerId, P2pError>>),
    AddExternal(Multiaddr),
    Shutdown,
}

/// Nœud démarré : tâche de fond du `Swarm`.
pub struct P2pNode {
    /// Poignée d'utilisation.
    pub handle: P2pHandle,
    task: JoinHandle<()>,
}

/// Poignée clonable vers le nœud.
#[derive(Clone)]
pub struct P2pHandle {
    peer_id: PeerId,
    cmd: mpsc::Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    control: libp2p_stream::Control,
}

impl P2pNode {
    /// Démarre le nœud : écoute, joint les pairs d'amorçage.
    pub async fn spawn(cfg: P2pConfig) -> Result<Self, P2pError> {
        let keypair = keypair_from_seed(&cfg.seed)?;
        let peer_id = keypair.public().to_peer_id();
        let mut swarm = build_swarm(&cfg, keypair)?;
        let control = swarm.behaviour().stream.new_control();
        for addr in &cfg.external {
            swarm.add_external_address(addr.clone());
        }
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (cmd, rx) = mpsc::channel(64);
        let handle = P2pHandle { peer_id, cmd, shared: shared.clone(), control };
        let task = tokio::spawn(run(swarm, rx, shared));
        for addr in &cfg.listen {
            handle.listen(addr.clone()).await?;
        }
        for addr in &cfg.bootstrap {
            // Un pair d'amorçage injoignable n'empêche pas le démarrage : le maillage se reforme plus tard.
            let _ = handle.dial(addr.clone()).await;
        }
        Ok(Self { handle, task })
    }

    /// Arrête le nœud.
    pub async fn shutdown(self) {
        let _ = self.handle.cmd.send(Command::Shutdown).await;
        let _ = self.task.await;
    }
}

impl P2pHandle {
    /// `PeerID` du nœud (celui de la clé IPFS de la station).
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Contrôle des flux applicatifs (`accept` / `open_stream`).
    pub fn control(&self) -> libp2p_stream::Control {
        self.control.clone()
    }

    /// Écoute sur `addr` ; rend l'adresse effectivement ouverte (port 0 résolu).
    pub async fn listen(&self, addr: Multiaddr) -> Result<Multiaddr, P2pError> {
        let (tx, rx) = oneshot::channel();
        self.cmd.send(Command::Listen(addr, tx)).await.map_err(|_| P2pError::Closed)?;
        tokio::time::timeout(Duration::from_secs(10), rx).await.map_err(|_| P2pError::Timeout)?.map_err(|_| P2pError::Closed)?
    }

    /// Joint un pair (`/ip4/…/tcp/…/p2p/<PeerID>`, ou une adresse de circuit relais) et attend la connexion.
    pub async fn dial(&self, addr: Multiaddr) -> Result<PeerId, P2pError> {
        let (tx, rx) = oneshot::channel();
        self.cmd.send(Command::Dial(addr, tx)).await.map_err(|_| P2pError::Closed)?;
        tokio::time::timeout(Duration::from_secs(15), rx).await.map_err(|_| P2pError::Timeout)?.map_err(|_| P2pError::Closed)?
    }

    /// Réserve une place de relais chez `relay` (`/ip4/…/tcp/…/p2p/<relais>`) : la station devient joignable
    /// derrière sa box à l'adresse de circuit retournée (`…/p2p/<relais>/p2p-circuit`).
    pub async fn listen_via_relay(&self, relay: Multiaddr) -> Result<Multiaddr, P2pError> {
        self.dial(relay.clone()).await?;
        self.listen(relay.with(Protocol::P2pCircuit)).await
    }

    /// Annonce une adresse publique.
    pub async fn add_external_address(&self, addr: Multiaddr) {
        let _ = self.cmd.send(Command::AddExternal(addr)).await;
    }

    /// Adresses d'écoute actuelles.
    pub fn listen_addrs(&self) -> Vec<Multiaddr> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).listen_addrs.clone()
    }

    /// Pairs actuellement connectés.
    pub fn connected_peers(&self) -> Vec<PeerId> {
        let mut v: Vec<PeerId> = self.shared.lock().unwrap_or_else(|e| e.into_inner()).connected.keys().copied().collect();
        v.sort();
        v
    }
}

async fn run(mut swarm: Swarm<Behaviour>, mut rx: mpsc::Receiver<Command>, shared: Arc<Mutex<Shared>>) {
    let mut pending_listen: HashMap<libp2p::core::transport::ListenerId, oneshot::Sender<Result<Multiaddr, P2pError>>> = HashMap::new();
    let mut pending_dial: HashMap<ConnectionId, oneshot::Sender<Result<PeerId, P2pError>>> = HashMap::new();
    let mut circuit_listeners: HashSet<libp2p::core::transport::ListenerId> = HashSet::new();
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None | Some(Command::Shutdown) => return,
                Some(Command::Listen(addr, reply)) => {
                    let is_circuit = addr.iter().any(|p| matches!(p, Protocol::P2pCircuit));
                    match swarm.listen_on(addr) {
                        Ok(id) => {
                            if is_circuit {
                                circuit_listeners.insert(id);
                            }
                            pending_listen.insert(id, reply);
                        }
                        Err(e) => { let _ = reply.send(Err(P2pError::Listen(e.to_string()))); }
                    }
                }
                Some(Command::Dial(addr, reply)) => {
                    let opts = DialOpts::unknown_peer_id().address(addr).build();
                    let id = opts.connection_id();
                    match swarm.dial(opts) {
                        Ok(()) => { pending_dial.insert(id, reply); }
                        Err(e) => { let _ = reply.send(Err(P2pError::Dial(e.to_string()))); }
                    }
                }
                Some(Command::AddExternal(addr)) => { swarm.add_external_address(addr); }
            },
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { listener_id, address } => {
                    shared.lock().unwrap_or_else(|e| e.into_inner()).listen_addrs.push(address.clone());
                    if let Some(reply) = pending_listen.remove(&listener_id) {
                        let _ = reply.send(Ok(address));
                    }
                }
                SwarmEvent::ListenerClosed { listener_id, reason, .. } => {
                    if let Some(reply) = pending_listen.remove(&listener_id) {
                        let _ = reply.send(Err(P2pError::Listen(format!("{reason:?}"))));
                    }
                    circuit_listeners.remove(&listener_id);
                }
                SwarmEvent::ConnectionEstablished { peer_id, connection_id, .. } => {
                    *shared.lock().unwrap_or_else(|e| e.into_inner()).connected.entry(peer_id).or_default() += 1;
                    if let Some(reply) = pending_dial.remove(&connection_id) {
                        let _ = reply.send(Ok(peer_id));
                    }
                }
                SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
                    if num_established == 0 {
                        shared.lock().unwrap_or_else(|e| e.into_inner()).connected.remove(&peer_id);
                    }
                }
                SwarmEvent::OutgoingConnectionError { connection_id, error, .. } => {
                    if let Some(reply) = pending_dial.remove(&connection_id) {
                        let _ = reply.send(Err(P2pError::Dial(error.to_string())));
                    }
                }
                _ => {}
            },
        }
    }
}
