//! Tunnels TCP « DRAGON » à travers le maillage : `/x/<service>-<PeerID>`.
//!
//! Remplace `DRAGON_p2p_ssh.sh` : une station **expose** un service local (relai Nostr, SSH, Brain GPU…) sous un nom ;
//! une autre le **joint** depuis un port local, sans qu'aucun port ne soit ouvert sur la box de l'hôte.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use libp2p::{PeerId, StreamProtocol};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::compat::FuturesAsyncReadCompatExt;

use crate::{P2pError, P2pHandle};

/// Nom de protocole d'un service : `/x/<service>-<PeerID de l'hôte>`.
///
/// `service` : 1 à 32 caractères parmi `a-z 0-9 . _ -`.
pub fn protocol_name(service: &str, host: &PeerId) -> Result<StreamProtocol, P2pError> {
    let valid = !service.is_empty() && service.len() <= 32 && service.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'));
    if !valid {
        return Err(P2pError::Stream(format!("nom de service invalide : {service:?}")));
    }
    StreamProtocol::try_from_owned(format!("/x/{service}-{host}")).map_err(|e| P2pError::Stream(e.to_string()))
}

/// Réponse (asynchrone) d'une [`StreamGate`].
pub type GateFuture = Pin<Box<dyn Future<Output = bool> + Send>>;

/// Décide, pour chaque flux entrant, s'il est ouvert. Sert aux services « lourds » (GPU, LLM) réservés à qui
/// y est autorisé : la décision dépend de l'instant présent, elle est donc prise **à chaque flux**, pas à l'exposition.
/// Un refus (ou une garde trop lente) referme le flux sans rien envoyer.
pub trait StreamGate: Send + Sync + 'static {
    /// Le pair `peer` peut-il ouvrir un flux vers `service` maintenant ?
    fn allow(&self, peer: PeerId, service: &str) -> GateFuture;

    /// Le flux déjà ouvert peut-il continuer ? Appelé périodiquement par le watchdog de session (par défaut : même
    /// décision qu'à l'ouverture). Une garde peut l'implémenter plus discrètement (pas de journal à chaque passage).
    fn still_allowed(&self, peer: PeerId, service: &str) -> GateFuture {
        self.allow(peer, service)
    }
}

/// Période par défaut du watchdog de session.
pub const DEFAULT_WATCH_EVERY: Duration = Duration::from_secs(60);

/// Temps accordé à une garde pour répondre ; au-delà, le flux est refusé (on ferme, on n'ouvre jamais par défaut).
const GATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Contrôle d'accès et limites d'un service exposé.
#[derive(Clone)]
pub struct ExposeOpts {
    /// Pairs autorisés ; `None` = tout membre du réseau privé.
    pub allow: Option<HashSet<PeerId>>,
    /// Tunnels simultanés au plus.
    pub max_streams: usize,
    /// Garde consultée pour chaque flux (politique de tunnels) ; `None` = pas de contrôle supplémentaire.
    pub gate: Option<Arc<dyn StreamGate>>,
    /// Période du watchdog : la garde est reconsultée pour chaque flux ouvert, et le flux est **coupé** dès qu'elle
    /// refuse (autorisation retirée en cours de session). 60 s par défaut ; ignoré sans garde.
    pub watch_every: Duration,
}

impl std::fmt::Debug for ExposeOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExposeOpts").field("allow", &self.allow).field("max_streams", &self.max_streams).field("gate", &self.gate.is_some()).field("watch_every", &self.watch_every).finish()
    }
}

impl Default for ExposeOpts {
    fn default() -> Self {
        Self { allow: None, max_streams: 64, gate: None, watch_every: DEFAULT_WATCH_EVERY }
    }
}

/// Service exposé ; s'arrête à la destruction.
pub struct Exposed {
    /// Protocole annoncé.
    pub protocol: String,
    task: JoinHandle<()>,
}

impl Drop for Exposed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Emplacement occupé par un pair : libéré à la destruction.
struct PeerSlot {
    map: Arc<Mutex<HashMap<PeerId, usize>>>,
    peer: PeerId,
}

impl PeerSlot {
    fn take(map: &Arc<Mutex<HashMap<PeerId, usize>>>, peer: PeerId, cap: usize) -> Option<Self> {
        let mut m = map.lock().unwrap_or_else(PoisonError::into_inner);
        let n = m.entry(peer).or_default();
        if *n >= cap {
            return None;
        }
        *n += 1;
        Some(Self { map: map.clone(), peer })
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut m = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = m.get_mut(&self.peer) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.peer);
            }
        }
    }
}

/// Expose `target` (`127.0.0.1:22`, `127.0.0.1:7777`…) sous le nom `service`.
pub fn expose(handle: &P2pHandle, service: &str, target: String, opts: ExposeOpts) -> Result<Exposed, P2pError> {
    let protocol = protocol_name(service, &handle.peer_id())?;
    let mut incoming = handle.control().accept(protocol.clone()).map_err(|e| P2pError::Stream(e.to_string()))?;
    let limit = Arc::new(Semaphore::new(opts.max_streams.max(1)));
    let per_peer: Arc<Mutex<HashMap<PeerId, usize>>> = Arc::default();
    let per_peer_cap = (opts.max_streams / 4).max(4).min(opts.max_streams.max(1));
    let (gate, service, watch_every) = (opts.gate.clone(), service.to_owned(), opts.watch_every.max(Duration::from_millis(10)));
    let task = tokio::spawn(async move {
        while let Some((peer, stream)) = incoming.next().await {
            if opts.allow.as_ref().is_some_and(|a| !a.contains(&peer)) {
                continue; // pair non autorisé : le flux est refermé
            }
            // Un même pair ne peut pas occuper tous les emplacements (même refusé par la garde, il en tient un
            // jusqu'à 5 s) : au plus le quart du total (4 au minimum), pour que les autres pairs passent toujours.
            let Some(_slot) = PeerSlot::take(&per_peer, peer, per_peer_cap) else { continue };
            let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
            let (target, gate, service) = (target.clone(), gate.clone(), service.clone());
            tokio::spawn(async move {
                let (_permit, _slot) = (permit, _slot);
                if let Some(gate) = &gate {
                    // Garde injoignable ou trop lente = refus.
                    if !tokio::time::timeout(GATE_TIMEOUT, gate.allow(peer, &service)).await.unwrap_or(false) {
                        return;
                    }
                }
                if let Ok(mut tcp) = TcpStream::connect(&target).await {
                    let mut tunnel = stream.compat();
                    let relay = copy_bidirectional(&mut tunnel, &mut tcp);
                    tokio::pin!(relay);
                    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + watch_every, watch_every);
                    loop {
                        tokio::select! {
                            _ = &mut relay => break,
                            _ = tick.tick() => {
                                // Watchdog : l'autorisation peut être retirée en cours de session. Refus, erreur ou lenteur de la
                                // garde coupent la liaison (les deux côtés sont refermés en quittant la boucle).
                                if let Some(g) = &gate {
                                    if !tokio::time::timeout(GATE_TIMEOUT, g.still_allowed(peer, &service)).await.unwrap_or(false) {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    Ok(Exposed { protocol: protocol.to_string(), task })
}

/// Tunnel entrant : un port local relié à un service d'un autre pair.
pub struct Forward {
    /// Adresse locale d'écoute.
    pub local_addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Écoute sur `bind` et relie chaque connexion au service `service` de `peer`.
pub async fn forward(handle: &P2pHandle, bind: SocketAddr, peer: PeerId, service: &str) -> Result<Forward, P2pError> {
    let protocol = protocol_name(service, &peer)?;
    let listener = TcpListener::bind(bind).await?;
    let local_addr = listener.local_addr()?;
    let control = handle.control();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else { return };
            let (mut control, protocol) = (control.clone(), protocol.clone());
            tokio::spawn(async move {
                if let Ok(stream) = control.open_stream(peer, protocol).await {
                    let mut tunnel = stream.compat();
                    let _ = copy_bidirectional(&mut tcp, &mut tunnel).await;
                }
            });
        }
    });
    Ok(Forward { local_addr, task })
}
