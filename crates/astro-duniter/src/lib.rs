//! Client Duniter v2s (Ğ1) natif : remplace `gcli`.
//!
//! - [`Ed25519Signer`] : signature ed25519 en mémoire (graine effacée à la destruction), adresses SS58 préfixe 4450 ;
//! - [`Amount`] : montants en centimes de Ğ1, jamais en flottant ;
//! - [`Chain`] : soldes, virements (avec mémo `utility.batch` + `system.remark_with_event`) et flux `Balances::Transfer` ;
//! - [`SubxtChain`] : implémentation sur un nœud RPC ; [`RpcPool`] : bascule automatique entre nœuds de secours ;
//! - [`offline`] : construction et signature d'extrinsics hors ligne (tests, signature à froid).

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod account;
mod chain;
pub mod offline;
mod notify;
mod pool;
mod signer;

pub use account::{Account, AccountError};
pub use astro_core::{Amount, AmountError};
pub use chain::{Balance, Chain, ChainError, SubxtChain, TransferEvent, TransferMode, TxOutcome};
pub use offline::{ChainInfo, sign_transfer_offline};
pub use notify::{FundsNotifier, IncomingFunds};
pub use pool::{Connector, PoolConfig, RpcPool, SubxtConnector};
pub use signer::Ed25519Signer;
