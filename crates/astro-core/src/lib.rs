//! Vérité cryptographique et temps solaire d'`astroportd`.
//!
//! - [`formula`] : les cinq formules de crédentiels (`KeyFormula`) et leur détection.
//! - [`kdf`] : scrypt Duniter, dérivations Ğ1 / IPFS / Nostr, étirement PBKDF2 de la clé LOVE.
//! - [`solar`] : équation du temps de Spencer, prochain 20h12 solaire vrai, garde-fou d'horloge.
//!
//! Chaque dérivation est vérifiée contre la sortie réelle de `tools/keygen` (voir `tests/`).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod activity;
pub mod amount;
pub mod formula;
pub mod kdf;
pub mod solar;
pub mod week;

pub use activity::{Activity, ActivityBus, Audience};
pub use amount::{Amount, AmountError};
pub use formula::{Credentials, FormulaInput, KeyFormula, KeyKind};
pub use kdf::{NostrKeys, Seed};
