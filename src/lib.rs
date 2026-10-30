//! Bibliothèque du binaire `astroportd` : les services de la station, l'interface web et le diagnostic, exposés
//! pour être testés de bout en bout (`tests/`) indépendamment de la ligne de commande.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod account;
pub mod assist;
pub mod build_attest;
pub mod coop_config;
pub mod console;
pub mod doctor;
pub mod duniter_nodes;
pub mod fetch;
pub mod forwards;
pub mod funds;
pub mod identity_links;
pub mod init;
pub mod integrity;
pub mod invites;
pub mod library;
pub mod media;
pub mod mesh_outbox;
pub mod p2p_relay;
pub mod private_backup;
pub mod register;
pub mod share_inbox;
pub mod station_admin;
pub mod sharing;
pub mod solar_cron;
pub mod upload_worker;
pub mod zen_react;
pub mod station;
pub mod tunnel_policy;
pub mod ui_backend;

/// Identifiant de connexion d'un compte (son email, à défaut sa clé) : celui des URL CardDAV.
pub async fn account_login(db: &astro_db::Db, owner: &str) -> String {
    astro_dav::carddav::login_of(db, owner).await
}
