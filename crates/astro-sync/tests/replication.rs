//! Où et par qui les données d'un membre sont répliquées : mesure par relai, par ids, sans jamais confondre
//! « absent » et « injoignable ».

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use astro_nostr_proto::{Event, Filter};
use astro_sync::peers::Peer;
use astro_sync::replication::{PeersFn, ProviderFn};
use astro_sync::{ClientError, RelayClient, ReplicationConfig, Replicator};
use common::*;

/// Relais simulés : chaque cible a ses événements, ou échoue.
struct Mesh(HashMap<String, Result<Vec<Event>, String>>);

impl RelayClient for Mesh {
    async fn fetch(&self, relay: &str, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        match self.0.get(relay) {
            Some(Ok(events)) => Ok(events.iter().filter(|e| filter.matches(e)).cloned().collect()),
            Some(Err(why)) => Err(ClientError::Connection(why.clone())),
            None => Err(ClientError::Connection("cible inconnue".into())),
        }
    }
}

fn peer(id: &str, name: &str, captain: &[u8; 32]) -> Peer {
    Peer { node_id: id.into(), hostname: Some(name.into()), relay: None, captain_hex: Some(hex(captain)), node_hex: None, captain_email: Some(format!("{name}@example.org")) }
}

#[tokio::test]
async fn replication_is_measured_per_station_with_friendship_and_failures_kept_apart() {
    let dir = TempDir::new("replication");
    let db = open_db(&dir);
    host(&db, &ALICE).await;
    let notes: Vec<Event> = (0..4).map(|i| ev(&ALICE, 1, 100 + i, vec![], &format!("note {i}"))).collect();
    for n in &notes {
        ingest(&db, n).await;
    }
    // Bob est un ami (niveau 1), Carol un ami d'ami (niveau 2) pour Alice.
    ingest(&db, &follows(&ALICE, 200, &[&BOB])).await;
    db.writer().execute("INSERT OR REPLACE INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 2, 0)", vec![hex(&ALICE).into(), hex(&CAROL).into()]).await.unwrap();

    let (bob, carol, dead) = (peer("12D3Bob", "bob", &BOB), peer("12D3Carol", "carol", &CAROL), peer("12D3Dead", "mort", &DAVE));
    let mesh = Mesh(HashMap::from([
        (bob.target(), Ok(notes[..3].to_vec())), // Bob a 3 des 4 notes
        (carol.target(), Ok(vec![notes[0].clone()])),
        (dead.target(), Err("tunnel : fermé".into())),
    ]));
    let peers: PeersFn = {
        let all = vec![bob.clone(), carol.clone(), dead.clone(), peer("12D3Me", "moi", &ALICE)];
        Arc::new(move || all.clone())
    };
    let providers: ProviderFn = Arc::new(|cid| Box::pin(async move { if cid == "bafy-a" { Ok(vec!["12D3Bob".to_owned(), "12D3Inconnu".to_owned()]) } else { Err("kubo muet".to_owned()) } }));
    db.writer()
        .execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mtime, data) VALUES (?1, '/a.jpg', '/', 'file', 'bafy-a', 2, '{}'), (?1, '/b.jpg', '/', 'file', 'bafy-b', 1, '{}')", vec![hex(&ALICE).into()])
        .await
        .unwrap();
    let cfg = ReplicationConfig { own_node_id: Some("12D3Me".into()), timeout: Duration::from_secs(5), ..ReplicationConfig::default() };
    let r = Replicator::new(db.clone(), Mesh(mesh.0), peers, Some(providers), cfg);
    assert_eq!(r.run_once().await.unwrap(), 1);

    let raw: String = db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'replication' AND key = ?1", [hex(&ALICE)], |r| r.get(0))?)).await.unwrap();
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(doc["events"], 5, "4 notes + 1 contacts ; {doc}");
    assert_eq!(doc["replicated"], 3, "les 3 notes de Bob (dont celle de Carol) ; le kind 3 d'Alice est chez personne : {doc}");
    let peers = doc["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 3, "la station elle-même n'est pas une copie : {doc}");
    let by = |name: &str| peers.iter().find(|p| p["name"] == name).unwrap().clone();
    assert_eq!((by("bob")["have"].as_u64(), by("bob")["level"].as_i64()), (Some(3), Some(1)));
    assert_eq!((by("carol")["have"].as_u64(), by("carol")["level"].as_i64()), (Some(1), Some(2)));
    assert!(by("mort")["have"].is_null() && by("mort")["error"].as_str().unwrap().contains("fermé"), "injoignable ≠ 0 copie : {doc}");
    assert_eq!(doc["unreachable"], 1);
    let media = &doc["media"];
    assert_eq!((media["files"].as_u64(), media["replicated"].as_u64(), media["lookup_failed"].as_u64()), (Some(2), Some(1), Some(1)), "{doc}");
    assert_eq!(media["peers"][0]["name"], "bob", "seuls les fournisseurs connus de la constellation comptent : {doc}");
}
