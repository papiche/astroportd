//! Envoi des enveloppes NIP-59 (partages de fichiers, photos de FaceCloud) **à la station de l'ami**.
//!
//! Le relai d'un ami qui vit sur une autre station n'est pas forcément dans `--relay`. La station d'accueil de
//! l'ami est donc cherchée d'abord (`home_station` de son DID, répliqué par le crawl N¹/N²), puis jointe par le maillage
//! (`/x/strfry-<PeerID>`, adresse publique en repli) ; les relais de `--relay` passent ensuite. L'envoi réussit dès qu'un
//! relai accepte l'enveloppe : celui de l'ami accepte un gift wrap adressé à un MULTIPASS qu'il héberge, sans authentification.
//!
//! Station d'accueil inconnue (un compte jamais croisé, la station NODE d'un inconnu qui se rétablit) : les stations de l'essaim
//! sont essayées à la suite ([`MAX_SWARM_TRIES`] au plus). Seule celle qui héberge le destinataire accepte l'enveloppe ; les autres
//! la refusent sans la garder, et elle reste illisible pour toutes.

use std::path::PathBuf;
use std::sync::Arc;

use astro_db::Db;
use astro_facecloud::{Outbox, OutboxError};
use astro_nostr_proto::Event;
use rusqlite::OptionalExtension;

use crate::p2p_relay::P2pRelayClient;

/// Stations de l'essaim essayées au plus quand la station d'accueil du destinataire est inconnue.
pub const MAX_SWARM_TRIES: usize = 32;

/// Boîte d'envoi par le maillage.
pub struct MeshOutbox {
    db: Arc<Db>,
    client: Arc<P2pRelayClient>,
    fixed: Vec<String>,
    swarm_dir: Option<PathBuf>,
}

impl MeshOutbox {
    /// `fixed` : relais de `--relay`, essayés après la station d'accueil du destinataire.
    pub fn new(db: Arc<Db>, client: Arc<P2pRelayClient>, fixed: Vec<String>, swarm_dir: Option<PathBuf>) -> Self {
        Self { db, client, fixed, swarm_dir }
    }

    /// Cibles à essayer, la station d'accueil d'abord.
    async fn targets(&self, recipient: &str) -> Vec<String> {
        let hex = recipient.to_owned();
        let home: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT home_station FROM identities WHERE hex = ?1", [hex], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten();
        let mut out = Vec::new();
        let dir = self.swarm_dir.clone();
        let peers = match dir {
            Some(dir) => tokio::task::spawn_blocking(move || astro_sync::read_peers(&dir)).await.unwrap_or_default(),
            None => Vec::new(),
        };
        // Une station de l'essaim se reconnaît aussi à sa clé NODE (`NODEHEX` de son 12345.json) : réponse adressée à la station même.
        let node = peers.iter().find(|p| p.node_hex.as_deref() == Some(recipient));
        let known = home.is_some() || node.is_some();
        if let Some(p) = node {
            out.push(p.target());
        }
        if let (Some(home), true) = (home, self.swarm_dir.is_some()) {
            let node_id = home.split(':').next().unwrap_or_default().to_owned();
            // La station est connue du maillage par son PeerID même quand son 12345.json manque : le tunnel suffit.
            out.push(peers.iter().find(|p| p.node_id == node_id).map_or_else(|| format!("p2p://{node_id}"), astro_sync::Peer::target));
        }
        // NIP-65 : sinon (ou en plus), les relais de LECTURE annoncés par le destinataire (kind 10002).
        for url in read_relays_of(&self.db, recipient).await {
            if !out.contains(&url) {
                out.push(url);
            }
        }
        for fixed in &self.fixed {
            if !out.contains(fixed) {
                out.push(fixed.clone());
            }
        }
        if !known {
            for target in peers.iter().take(MAX_SWARM_TRIES).map(astro_sync::Peer::target) {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
        }
        out
    }
}

/// Relais de lecture annoncés par un compte (NIP-65, dernier kind 10002 connu).
pub async fn read_relays_of(db: &Db, hex: &str) -> Vec<String> {
    let hex = hex.to_owned();
    let tags: Option<String> = db.readers().read(move |c| Ok(c.query_row("SELECT tags FROM nostr_events WHERE pubkey = ?1 AND kind = 10002 ORDER BY created_at DESC LIMIT 1", [hex], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten();
    let listed: Vec<Vec<String>> = tags.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    astro_nostr_proto::relay_list::parse(&listed).into_iter().filter(|e| e.read).map(|e| e.url).collect()
}

impl Outbox for MeshOutbox {
    async fn send(&self, recipient_hex: &str, wrap: Event) -> Result<(), OutboxError> {
        let mut failures = Vec::new();
        for target in self.targets(recipient_hex).await {
            match self.client.publish(&target, &wrap).await {
                Ok((true, _)) => return Ok(()),
                Ok((false, message)) => failures.push(format!("{target} : {message}")),
                Err(e) => failures.push(format!("{target} : {e}")),
            }
        }
        if failures.is_empty() {
            failures.push("aucune station d'accueil connue pour ce destinataire et aucun relai (--relay)".to_owned());
        }
        Err(OutboxError(format!("enveloppe pour {}… non livrée ({})", &recipient_hex[..8.min(recipient_hex.len())], failures.join(" ; "))))
    }
}

#[cfg(test)]
mod tests {
    use astro_nostr_proto::UnsignedEvent;

    use super::*;

    #[tokio::test]
    async fn an_unknown_recipient_is_tried_on_the_swarm_and_a_node_key_reaches_its_own_station() {
        let dir = std::env::temp_dir().join(format!("astroportd-swarm-try-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (peer, node) in [("12D3KooWAaaa", "a1"), ("12D3KooWBbbb", "b2")] {
            std::fs::create_dir_all(dir.join("swarm").join(peer)).unwrap();
            std::fs::write(dir.join("swarm").join(peer).join("12345.json"), format!(r#"{{"NODEHEX":"{}"}}"#, node.repeat(32))).unwrap();
        }
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let client = Arc::new(P2pRelayClient::new(astro_sync::WsClient::new(std::time::Duration::from_secs(1)), None));
        let outbox = MeshOutbox::new(db.clone(), client, vec!["wss://fixe.example".into()], Some(dir.join("swarm")));
        assert_eq!(outbox.targets(&"cc".repeat(32)).await, ["wss://fixe.example", "p2p://12D3KooWAaaa", "p2p://12D3KooWBbbb"], "inconnu : le relai fixe, puis l'essaim");
        assert_eq!(outbox.targets(&"b2".repeat(32)).await, ["p2p://12D3KooWBbbb", "wss://fixe.example"], "clé NODE : sa station, sans balayer l'essaim");
        // Station d'accueil connue : pas de balayage.
        db.writer().execute("INSERT INTO identities(hex, role, hosted, home_station) VALUES (?1, 'multipass', 0, ?2)", vec!["dd".repeat(32).into(), format!("12D3KooWAaaa:{}", "a1".repeat(32)).into()]).await.unwrap();
        assert_eq!(outbox.targets(&"dd".repeat(32)).await, ["p2p://12D3KooWAaaa", "wss://fixe.example"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_latest_nip65_list_gives_the_read_relays_only() {
        let dir = std::env::temp_dir().join(format!("astroportd-nip65-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 2).unwrap();
        let sign = |at: u64, tags: Vec<Vec<String>>| UnsignedEvent { created_at: at, kind: 10_002, tags, content: String::new() }.sign(&[7; 32]).unwrap();
        let r = |p: &[&str]| p.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let hex = astro_nostr_proto::pubkey_of(&[7; 32]).unwrap();
        assert!(read_relays_of(&db, &hex).await.is_empty(), "aucune liste connue");
        for e in [sign(1_800_000_000, vec![r(&["r", "wss://vieux.example"])]), sign(1_800_000_100, vec![r(&["r", "wss://lit.example", "read"]), r(&["r", "wss://ecrit.example", "write"]), r(&["r", "wss://les-deux.example"])])] {
            db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &e).map(|_| ())).await.unwrap();
        }
        assert_eq!(read_relays_of(&db, &hex).await, ["wss://lit.example", "wss://les-deux.example"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
