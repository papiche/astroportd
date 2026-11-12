//! Serveur WebDAV personnel chiffré (UENC v2) et manifeste uCloud rejouable.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod adopt;
pub mod auth;
pub mod blob;
pub mod caldav;
pub mod devices;
pub mod carddav;
pub mod exif;
pub mod ical;
pub mod ingest;
pub mod kubo;
pub mod locks;
pub mod manifest;
pub mod parse;
pub mod pool;
pub mod public;
pub mod server;
pub mod share;
pub mod jpeg_scale;
pub mod thumb;
pub mod vcard;
pub mod vfs;
pub mod xml;

pub use blob::{AutoBlobStore, BlobError, BlobMeta, BlobStore, BlobWriter, FsBlobStore, KuboBlobStore};
pub use public::PublicFetch;
pub use manifest::{ManifestError, ManifestPublisher};
pub use server::{DavConfig, DavState, ReadFailure, LoginHook, ReadFailureHook, UploadHook, get_owner, router};
