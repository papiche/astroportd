//! Serveur WebDAV personnel chiffré (UENC v2) et manifeste uCloud rejouable.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod auth;
pub mod blob;
pub mod ingest;
pub mod kubo;
pub mod locks;
pub mod media;
pub mod manifest;
pub mod parse;
pub mod server;
pub mod share;
pub mod thumb;
pub mod vfs;
pub mod xml;

pub use blob::{AutoBlobStore, BlobError, BlobMeta, BlobStore, BlobWriter, FsBlobStore, KuboBlobStore};
pub use manifest::{ManifestError, ManifestPublisher};
pub use server::{DavConfig, DavState, UploadHook, router};
