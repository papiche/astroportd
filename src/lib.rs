//! Bibliothèque du binaire `astroportd` : les services de la station, l'interface web et le diagnostic, exposés
//! pour être testés de bout en bout (`tests/`) indépendamment de la ligne de commande.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod beacon;
pub mod captains;
pub mod control;
pub mod account;
pub mod address_book;
pub mod agents;
pub mod assist;
pub mod chat;
pub mod coop_config;
pub mod doctor;
pub mod dnslink;
pub mod energy;
pub mod duniter_nodes;
pub mod feedback;
pub mod fetch;
pub mod forwards;
pub mod funds;
pub mod identity_links;
pub mod init;
pub mod first_run;
pub mod integrity;
pub mod invites;
pub mod library;
pub mod member_ai;
pub mod mesh_outbox;
pub mod organize;
pub mod p2p_relay;
pub mod paf;
pub mod new_stations;
pub mod person_keys;
pub mod rotate;
pub mod private_backup;
pub mod register;
pub mod share_inbox;
pub mod station_admin;
pub mod station_domain;
pub mod swarm_cards;
pub mod sponsor;
pub mod swarm_route;
pub mod sharing;
pub mod solar_cron;
pub mod upload_worker;
pub mod webdav_pull;
pub mod welcome;
pub mod bridge;
pub mod coop_wallets;
pub mod constellation;
pub mod billet;
pub mod zen_credit;
pub mod oc_emission;
pub mod oc_member;
pub mod public_site;
pub mod upload_image;
pub mod profile_image;
pub mod charter_accept;
pub mod hosting_fee;
pub mod plane;
pub mod systemd_unit;
pub mod presence;
pub mod geo_lease;
pub mod intrusion_guard;
pub mod member_primo;
pub mod primal_init;
pub mod settlement;
pub mod zen_react;
pub mod station;
pub mod stun;
pub mod turn;
pub mod mesh_admin;
pub mod trash;
pub mod tunnel_policy;
pub mod ui_backend;

/// Identifiant de connexion d'un compte (son email, à défaut sa clé) : celui des URL CardDAV.
pub async fn account_login(db: &astro_db::Db, owner: &str) -> String {
    astro_dav::carddav::login_of(db, owner).await
}
