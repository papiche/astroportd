//! Façade web d'`astroportd` : TLS automatique (ACME), HTTP→HTTPS, reverse proxy intégré, application web embarquée
//! ([`web`]), API JSON, supervision `/health` et flux d'activité SSE ([`api`]).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod acme;
pub mod api;
pub mod frontend;
pub mod proxy;
pub mod qr;
pub mod tls;
pub mod web;

pub use acme::{AcmeConfig, AcmeError, AcmeManager, CertResolver, ChallengeStore, KvCertStore};
pub use api::{ApiError, Backend, Principal, Status, Subsystem};
pub use frontend::{Frontend, http_router};
pub use proxy::{Gate, ProxyRoute, proxy_router};
pub use tls::{TlsListener, TlsPeer};
