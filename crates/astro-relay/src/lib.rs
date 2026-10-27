//! Relais Nostr souverain d'`astroportd` : NIP-01, NIP-11, NIP-40, NIP-42, NIP-59.
//!
//! Remplace `strfry` et son plugin `writePolicy` : les événements sont validés par `astro-nostr-proto`,
//! stockés par `astro-db` et filtrés par le pare-feu social ([`Policy`]) à deux niveaux.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod circle;
pub mod limits;
pub mod policy;
mod server;

pub use circle::{CircleProvider, DbCircle, StaticCircle};
pub use limits::RateLimiter;
pub use policy::{AuthState, Policy, ReadAccess, WriteDecision};
pub use server::{PlanFn, Relay, RelayConfig, serve};
