//! Interface typée du runtime Ğ1 générée par `subxt` à partir de `res/g1_metadata.scale`.
//!
//! La metadata vient de `gcli-v2s/res/g1_metadata.scale` ; pour la mettre à jour quand le runtime change :
//! `subxt metadata --url ws://localhost:9944 -f bytes > crates/astro-g1-runtime/res/g1_metadata.scale`.
//! Le code généré est volumineux : ce crate est volontairement séparé de `astro-duniter`.

#![allow(warnings, clippy::all, missing_docs, unsafe_code)]

/// Metadata brute du runtime (pour le client hors ligne).
pub const METADATA: &[u8] = include_bytes!("../res/g1_metadata.scale");

#[subxt::subxt(
    runtime_metadata_path = "res/g1_metadata.scale",
    derive_for_all_types = "Debug, Clone",
    substitute_type(path = "sp_core::crypto::AccountId32", with = "::subxt::utils::AccountId32")
)]
pub mod runtime {}

pub use runtime::*;
