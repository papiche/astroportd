//! FaceCloud : reconnaissance faciale des photos uCloud et partage automatique entre amis.
//!
//! Pipeline : photo déposée (WebDAV) → file d'attente persistante (une inférence à la fois) → détection + embeddings
//! 512D ([`FaceEngine`]) → recherche cosinus dans `face_vectors` → décision :
//! score ≥ 0,82 reconnu, [0,55 ; 0,82[ suggestion « est-ce X ? », en dessous inconnu (`Inconnu_<hash8>`) →
//! partage aux amis réciproques : copie silencieuse dans `/Partages/De_<nom>/` s'ils sont hébergés ici, sinon
//! message fichier NIP-17 emballé en NIP-59 (voir [`Outbox`]).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod board;
pub mod engine;
pub mod imageio;
mod pipeline;
mod queue;
pub mod thumbs;
pub mod review;
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod scrfd;

pub use engine::{AnyFaceEngine, BBox, DIM, DetectedFace, EngineError, FaceEngine, HttpFaceEngine, SwitchableEngine, embedding_hash, l2_normalize, unknown_name};
pub use pipeline::{Decision, FaceCloud, FaceConfig, FaceError, FaceReport, Outbox, OutboxError, PhotoReport, ShareReport, ShareVia, Skipped};
pub use queue::QueueStats;
