//! Cryptographie d'`astroportd`.
//!
//! - [`uenc`] : codec UENC bivalent. Lecture v1 (monolithique, legacy) et v2 (flux chunké de 64 Kio) ;
//!   écriture v2 uniquement.
//! - [`nip98`] : validation durcie de l'en-tête `Authorization: Nostr …`.
//! - [`UencKey`] : clé AES-256 de fichier, effacée de la mémoire à la destruction.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod key;
pub mod keys;
pub mod nip44;
pub mod nip59;
pub mod nip98;
pub mod uenc;

pub use key::UencKey;
pub use keys::{FileKeySource, KeySource, SecretBytes, StaticKeySource};
pub use uenc::{Cipher, UencError};
