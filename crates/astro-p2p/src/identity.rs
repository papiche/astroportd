//! Identité libp2p dérivée de la clé IPFS de la station.

use libp2p::identity::{Keypair, ed25519};

use crate::P2pError;

/// Paire de clés libp2p à partir de la graine ed25519 de 32 octets (celle de `keygen -t ipfs`).
///
/// Son `PeerID` est identique à `Seed::ipfs_peer_id()` d'`astro-core` : une seule identité pour IPFS et le maillage.
pub fn keypair_from_seed(seed: &[u8; 32]) -> Result<Keypair, P2pError> {
    let secret = ed25519::SecretKey::try_from_bytes(seed.to_vec()).map_err(|e| P2pError::Identity(e.to_string()))?;
    Ok(ed25519::Keypair::from(secret).into())
}
