//! Bibliothèque du binaire `astroportd` : les services de la station, l'interface web et le diagnostic, exposés
//! pour être testés de bout en bout (`tests/`) indépendamment de la ligne de commande.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod doctor;
pub mod fetch;
pub mod init;
pub mod lease_gate;
pub mod media;
pub mod solar_cron;
pub mod upload_worker;
pub mod station;
pub mod ui_backend;
