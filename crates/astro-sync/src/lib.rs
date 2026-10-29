//! Moteur de projection, de replay et de crawl N² d'`astroportd`.
//!
//! Nostr et IPFS sont les sources de vérité ; `astro-db` n'en est que la projection.
//! - [`Projector`] traduit un événement en requêtes SQL (réseau social, identités, registre Ğ1-N², économie).
//! - [`replay_local`] reconstruit toutes les tables de projection depuis `nostr_events` ; [`replay_with_ucloud`]
//!   y ajoute le VFS et le trousseau uCloud, depuis les manifestes chiffrés (Nostr + IPFS).
//! - [`Crawler`] récupère, clef par clef, les événements des membres N1 et N2 sur les relais de la constellation.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod client;
pub mod crawl;
pub mod ingest;
pub mod outbox;
pub mod peers;
pub mod projector;
pub mod replication;
pub mod replay;

pub use client::{ClientError, RelayClient, Signer, WsClient};
pub use crawl::{CrawlConfig, CrawlStats, Crawler, PassHook, PassReport, RelaySource};
pub use ingest::{Ingested, ingest_event};
pub use outbox::RelayOutbox;
pub use peers::{Peer, read_peers};
pub use projector::{ProjectError, Projector};
pub use replication::{ReplicationConfig, Replicator};
pub use replay::{ReplayStats, replay_local, replay_with_ucloud};

/// Erreurs du moteur de synchronisation.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// Erreur de base de données.
    #[error("base de données : {0}")]
    Db(#[from] astro_db::DbError),
    /// Échec d'un échange avec un relai.
    #[error("relai : {0}")]
    Relay(String),
}
