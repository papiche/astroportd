//! BRO LifeOS : l'agent personnel de la station.
//!
//! - [`memory`] : mémoire sémantique par MULTIPASS (SQLite + `sqlite-vec`, `bge-m3` en 1024 dimensions), quatre
//!   emplacements hérités du terrain (0 identité, 13 préférences, 14 objectifs, 15 journal) ;
//! - [`reve`] : cycle RÊVE, compression des souvenirs anciens en un seul souvenir `[RÊVE]` (≥ 150 → on garde les 80
//!   plus récents), purge atomique ;
//! - [`guard`] : ce qui ne doit **jamais** entrer en mémoire (clés, mots de passe, IBAN…) ;
//! - [`ollama`] : embeddings et résumés par Ollama (HTTP), derrière les traits [`Embedder`] et [`Summarizer`] ;
//! - [`broker`] : courtier autonome — une station sans GPU dont la file s'allonge (modèle d'Erlang-C) réserve une heure
//!   de calcul chez le Brain le plus proche, par un virement Ẑen signé ;
//! - [`bus`] : bus de commandes par messages privés NIP-59 adressés à la station (solde, statut, documents).
//!
//! Aucune erreur n'est avalée : un embedding indisponible ou un résumé vide interrompt l'opération **sans rien
//! modifier**, et l'échec est publié sur le bus d'activité.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod broker;
pub mod bus;
pub mod guard;
pub mod memory;
pub mod ollama;
pub mod reve;
#[cfg(test)]
mod testkit;

pub use memory::{Memory, MemoryStore, Meta, Recalled, Remembered, Slot};
pub use ollama::{Embedder, HashingEmbedder, OllamaClient, Summarizer};
pub use reve::{DreamReport, REVE_KEEP, REVE_THRESHOLD};

/// Erreurs de BRO.
#[derive(Debug, thiserror::Error)]
pub enum BroError {
    /// Contenu refusé (secret, clé…).
    #[error("contenu non mémorisable : {0}")]
    Unstorable(&'static str),
    /// Texte vide.
    #[error("texte vide")]
    Empty,
    /// Emplacement mémoire inconnu.
    #[error("emplacement mémoire inconnu : {0} (0, 13, 14 ou 15)")]
    BadSlot(i64),
    /// Moteur d'embedding ou de résumé injoignable ou fautif.
    #[error("modèle : {0}")]
    Model(String),
    /// Le résumé RÊVE est vide : rien n'est purgé.
    #[error("résumé RÊVE vide : mémoire laissée intacte")]
    EmptySummary,
    /// Base de données.
    #[error("base : {0}")]
    Db(#[from] astro_db::DbError),
    /// Une autre compression a modifié les souvenirs entre-temps.
    #[error("souvenirs modifiés pendant la compression : abandon sans effet")]
    Raced,
}
