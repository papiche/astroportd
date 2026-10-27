//! FaceCloud : reconnaissance faciale des photos uCloud et partage automatique entre amis.
//!
//! Pipeline : photo déposée (WebDAV) → file d'attente persistante (une inférence à la fois) → détection + embeddings
//! 512D ([`FaceEngine`]) → recherche cosinus dans `face_vectors` → décision :
//! score ≥ 0,82 reconnu, [0,55 ; 0,82[ suggestion « est-ce X ? », en dessous inconnu (`Inconnu_<hash8>`) →
//! partage aux amis réciproques : copie silencieuse dans `/Partages/De_<nom>/` s'ils sont hébergés ici, sinon
//! message fichier NIP-17 emballé en NIP-59 (voir [`Outbox`]).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod engine;
mod pipeline;
mod queue;
pub mod review;

pub use engine::{BBox, DIM, DetectedFace, EngineError, FaceEngine, HttpFaceEngine, embedding_hash, l2_normalize, unknown_name};
pub use pipeline::{Decision, FaceCloud, FaceConfig, FaceError, FaceReport, Outbox, OutboxError, PhotoReport, ShareReport, ShareVia, Skipped};
pub use queue::QueueStats;
