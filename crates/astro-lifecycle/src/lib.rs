//! Cycle de vie de la station et des MULTIPASS qu'elle héberge.
//!
//! - [`discovery`] : détection automatique des identités locales (NODE, Capitaine, MULTIPASS hébergés) dans le dossier
//!   `~/.zen/game` — plus aucune clé à passer en variable d'environnement ;
//! - [`dunikey`] : lecture des portefeuilles `*.dunikey` (`Type: PubSec`) ;
//! - [`state`] : machine d'état hebdomadaire (loyer Opex ou parts sociétaires, grâce, suspension) ;
//! - [`migration`] : déménagement souverain (`home:station`) : conteneur chiffré UENC v2 et `cashback` résiduel ;
//! - [`rebuild`] : reconstruction du statut des comptes après un `replay` (loyers constatés en 30852 / 30851) ;
//! - [`ylevel`] : rituel Y-Level, clé SSH = PeerID IPFS = Ğ1 = Nostr (`astroportd init` / `ylevel`) ;
//! - [`store`] : traduction en requêtes SQL (`identities`, `kv`, `done`).
//!
//! Les secrets restent dans des `Zeroizing` et ne sont jamais écrits en base ni dans les journaux.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod discovery;
pub mod dunikey;
pub mod migration;
pub mod rebuild;
pub mod state;
pub mod store;
pub mod ylevel;

pub use discovery::{Discovered, DiscoveryError, HostedMultipass, Layout, NodeIdentity, Problem};
pub use state::{AccountState, Action, Contract, Cycle, Decision, Status, decide};
