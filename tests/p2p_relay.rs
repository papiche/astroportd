//! L'astuce de DRAGON pour le crawl : joindre le relai d'un pair par un tunnel `ipfs p2p` (`/x/strfry-<PeerID>`),
//! sans aucun port public, puis retomber sur son adresse publique seulement si le tunnel est inutilisable.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use astro_db::{Db, events};
use astro_nostr_proto::{Event, Filter, UnsignedEvent, pubkey_of};
use astro_p2p::tunnel::{ExposeOpts, expose};
use astro_p2p::{Multiaddr, P2pConfig, P2pNode};
use astro_relay::{DbCircle, Relay, RelayConfig, serve};
use astro_sync::peers::RELAY_SERVICE;
use astro_sync::{ClientError, RelayClient, WsClient};
use astroportd::p2p_relay::P2pRelayClient;

const BOB: [u8; 32] = [4; 32];

async fn relay_with_a_note(tag: &str, text: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("astroportd-p2prelay-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let note: Event = UnsignedEvent { created_at: 1_800_000_000, kind: 1, tags: vec![], content: text.into() }.sign(&BOB).unwrap();
    db.writer().transaction(move |tx| events::insert_event(tx, &note).map(|_| ())).await.unwrap();
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await.unwrap();
    let relay = Relay::new(db, circle, RelayConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(serve(listener, relay));
    (dir, addr)
}

async fn node(seed: u8) -> (P2pNode, Multiaddr) {
    let n = P2pNode::spawn(P2pConfig::local([seed; 32])).await.unwrap();
    let tcp = n.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", n.handle.peer_id()).parse().unwrap();
    (n, full)
}

fn notes() -> Filter {
    Filter { kinds: Some(vec![1]), authors: Some(vec![pubkey_of(&BOB).unwrap()]), ..Filter::default() }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peers_relay_is_reached_through_the_mesh_and_the_public_address_is_only_a_fallback() {
    // B : une station dont le relai n'est exposé QUE par le maillage (écoute locale).
    let (dir_b, relay_b) = relay_with_a_note("b", "note vue par le maillage").await;
    let (b, b_addr) = node(21).await;
    let _exposed = expose(&b.handle, RELAY_SERVICE, relay_b, ExposeOpts::default()).unwrap();
    // A : la station qui crawle.
    let (a, _) = node(22).await;
    a.handle.dial(b_addr).await.unwrap();
    let client = P2pRelayClient::new(WsClient::new(Duration::from_secs(3)), Some(a.handle.clone()));

    // 1. Tunnel : l'adresse publique est injoignable, elle n'est pas utilisée.
    let via_mesh = format!("p2p://{}|ws://127.0.0.1:1", b.handle.peer_id());
    let got = client.fetch(&via_mesh, &notes()).await.unwrap();
    assert_eq!(got.iter().map(|e| e.content.as_str()).collect::<Vec<_>>(), ["note vue par le maillage"]);
    let route = client.routes().into_iter().find(|r| r.target == via_mesh).unwrap();
    assert_eq!((route.via, route.error), ("p2p", None));
    // Le tunnel est ouvert une fois et réutilisé.
    assert_eq!(client.fetch(&via_mesh, &notes()).await.unwrap().len(), 1);

    // 2. Pair injoignable par le maillage (jamais vu) : repli sur l'adresse publique, et la route le dit.
    let (dir_c, relay_c) = relay_with_a_note("c", "note vue par l'adresse publique").await;
    let (ghost, _) = node(23).await; // existe, mais A ne le connaît pas : le tunnel ne s'ouvre pas
    let via_fallback = format!("p2p://{}|ws://{relay_c}", ghost.handle.peer_id());
    let got = client.fetch(&via_fallback, &notes()).await.unwrap();
    assert_eq!(got[0].content, "note vue par l'adresse publique");
    let route = client.routes().into_iter().find(|r| r.target == via_fallback).unwrap();
    assert_eq!(route.via, "wss");
    assert!(route.error.is_some_and(|e| e.contains("tunnel")), "la raison de l'échec du tunnel est conservée");

    // 3. Pas de repli : l'échec est rendu tel quel, jamais silencieux.
    let nowhere = format!("p2p://{}", ghost.handle.peer_id());
    assert!(matches!(client.fetch(&nowhere, &notes()).await, Err(ClientError::Connection(_) | ClientError::Timeout)));
    assert_eq!(client.routes().into_iter().find(|r| r.target == nowhere).unwrap().via, "down");

    // 4. Station sans maillage : seules les adresses publiques servent.
    let alone = P2pRelayClient::new(WsClient::new(Duration::from_secs(3)), None);
    assert!(alone.fetch(&format!("p2p://{}", b.handle.peer_id()), &notes()).await.is_err());
    assert_eq!(alone.fetch(&via_fallback, &notes()).await.unwrap()[0].content, "note vue par l'adresse publique");
    assert_eq!(alone.fetch(&format!("ws://{}", via_fallback.rsplit("ws://").next().unwrap()), &notes()).await.unwrap().len(), 1, "adresse ordinaire : directe");
    let _ = (std::fs::remove_dir_all(dir_b), std::fs::remove_dir_all(dir_c));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_envelope_for_a_friend_is_published_through_the_mesh_to_his_station() {
    use astro_crypto::nip59::{Rumor, gift_wrap};
    const CAROL: [u8; 32] = [0xC3; 32];
    let dir = std::env::temp_dir().join(format!("astroportd-p2ppublish-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'carol@example.org', 1)", vec![pubkey_of(&CAROL).unwrap().into()]).await.unwrap();
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await.unwrap();
    let relay = Relay::new(db.clone(), circle, RelayConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().to_string();
    tokio::spawn(serve(listener, relay));
    let (b, b_addr) = node(51).await;
    let _exposed = expose(&b.handle, RELAY_SERVICE, local, ExposeOpts::default()).unwrap();
    let (a, _) = node(52).await;
    a.handle.dial(b_addr).await.unwrap();
    let client = P2pRelayClient::new(WsClient::new(Duration::from_secs(5)), Some(a.handle.clone()));

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let rumor = Rumor { pubkey: pubkey_of(&BOB).unwrap(), created_at: now, kind: astro_nostr_proto::kind::FILE, tags: vec![], content: "ipfs://x".into() };
    let wrap = gift_wrap(&BOB, &pubkey_of(&CAROL).unwrap(), &rumor, now).unwrap();
    let (accepted, message) = client.publish(&format!("p2p://{}|ws://127.0.0.1:1", b.handle.peer_id()), &wrap).await.unwrap();
    assert!(accepted, "{message}");
    let id = wrap.id.clone();
    let stored: i64 = db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM nostr_events WHERE id = ?1 AND kind = 1059", [id], |r| r.get(0))?)).await.unwrap();
    assert_eq!(stored, 1, "l'enveloppe est dans le relai de la station de l'ami");
    assert_eq!(client.routes()[0].via, "p2p");
    // Une enveloppe pour quelqu'un qui n'est pas hébergé là est refusée par ce relai.
    let stranger = gift_wrap(&BOB, &pubkey_of(&[9; 32]).unwrap(), &rumor, now).unwrap();
    assert!(!client.publish(&format!("p2p://{}", b.handle.peer_id()), &stranger).await.unwrap().0);
    let _ = std::fs::remove_dir_all(dir);
}
