//! Envoi des enveloppes NIP-59 (partages de fichiers, photos de FaceCloud) **à la station de l'ami**.
//!
//! Le relai d'un ami qui vit sur une autre station n'est pas forcément dans `--relay`. La station d'accueil de
//! l'ami est donc cherchée d'abord (`home_station` de son DID, répliqué par le crawl N¹/N²), puis jointe par le maillage
//! (`/x/strfry-<PeerID>`, adresse publique en repli) ; les relais de `--relay` passent ensuite. L'envoi réussit dès qu'un
//! relai accepte l'enveloppe : celui de l'ami accepte un gift wrap adressé à un MULTIPASS qu'il héberge, sans authentification.

use std::path::PathBuf;
use std::sync::Arc;

use astro_db::Db;
use astro_facecloud::{Outbox, OutboxError};
use astro_nostr_proto::Event;
use rusqlite::OptionalExtension;

use crate::p2p_relay::P2pRelayClient;

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
        if let (Some(home), Some(dir)) = (home, self.swarm_dir.clone()) {
            let node_id = home.split(':').next().unwrap_or_default().to_owned();
            let peers = tokio::task::spawn_blocking(move || astro_sync::read_peers(&dir)).await.unwrap_or_default();
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
