//! Réseau pair-à-pair souverain des stations : libp2p (TCP + QUIC, Noise, Yamux), réseau privé par clé
//! `swarm.key`, circuit relay v2 et hole punching (DCUtR) pour traverser les box, tunnels TCP « DRAGON »
//! (`/x/<service>-<peerid>`) et échange direct de blobs chiffrés (`/astro/blob/1.0.0`), sans Kubo.
//!
//! Routage intelligent du swarm ([`routing`]) : Power-Score du matériel local et choix du meilleur nœud pour un
//! service distant (`power`, `closest`, `random`), héritage d'`astrosystemctl`.
//!
//! L'identité du nœud est la clé IPFS de la station : son `PeerID` est exactement `12D3Koo…` tel que
//! `keygen -t ipfs` le calcule.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod blob;
mod identity;
mod node;
mod psk;
pub mod routing;
pub mod tunnel;

pub use identity::keypair_from_seed;
pub use node::{P2pConfig, P2pHandle, P2pNode};
pub use psk::{fingerprint, generate_swarm_key, parse_swarm_key};

pub use libp2p::{Multiaddr, PeerId, pnet::PreSharedKey};

/// Erreurs du réseau P2P.
#[derive(Debug, thiserror::Error)]
pub enum P2pError {
    /// Clé ou graine invalide.
    #[error("identité invalide : {0}")]
    Identity(String),
    /// `swarm.key` illisible.
    #[error("swarm.key invalide : {0}")]
    SwarmKey(String),
    /// Construction du nœud.
    #[error("nœud : {0}")]
    Build(String),
    /// Écoute impossible.
    #[error("écoute : {0}")]
    Listen(String),
    /// Connexion impossible.
    #[error("connexion : {0}")]
    Dial(String),
    /// Le nœud est arrêté.
    #[error("nœud arrêté")]
    Closed,
    /// Délai dépassé.
    #[error("délai dépassé")]
    Timeout,
    /// Flux ou protocole.
    #[error("flux : {0}")]
    Stream(String),
    /// Entrée/sortie.
    #[error("entrée/sortie : {0}")]
    Io(#[from] std::io::Error),
}
