//! Maillage P2P entre stations : identité IPFS, réseau privé, tunnels DRAGON, échange de blobs, circuit relais.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use astro_dav::blob::{BlobStore, BlobWriter, FsBlobStore, cid_raw_sha256};
use astro_p2p::blob::{FetchError, P2pBlobStore, ServeOpts, fetch_blob, fetch_from, serve_blobs};
use astro_p2p::tunnel::{ExposeOpts, expose, forward, protocol_name};
use astro_p2p::{Multiaddr, P2pConfig, P2pNode, generate_swarm_key, parse_swarm_key};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("astro-p2p-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sample(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

async fn node(seed: u8, psk: Option<&str>, relay_server: bool) -> (P2pNode, Multiaddr) {
    let mut cfg = P2pConfig::local([seed; 32]);
    cfg.psk = psk.map(|k| parse_swarm_key(k).unwrap());
    cfg.relay_server = relay_server;
    let node = P2pNode::spawn(cfg).await.unwrap();
    let tcp: Multiaddr = node.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", node.handle.peer_id()).parse().unwrap();
    (node, full)
}

async fn put_blob(store: &FsBlobStore, data: &[u8]) -> String {
    let mut w = store.begin().await.unwrap();
    w.write_all(data).await.unwrap();
    w.commit().await.unwrap().cid
}

const FETCH: Duration = Duration::from_secs(20);

#[tokio::test]
async fn the_peer_id_is_the_ipfs_peer_id_of_the_station_key() {
    let seed = astro_core::kdf::duniter_seed("salt-test", "pepper-test").unwrap();
    let mut cfg = P2pConfig::local(*seed.ed25519_secret_seed());
    cfg.listen.clear();
    let node = P2pNode::spawn(cfg).await.unwrap();
    assert_eq!(node.handle.peer_id().to_string(), seed.ipfs_peer_id());
    assert_eq!(node.handle.peer_id().to_string(), "12D3KooWFSoxH5cZqhpcywX4P4ZKgaU1yTzcopYEvVhSQfHdhwwJ", "valeur produite par tools/keygen -t ipfs");
    node.shutdown().await;
}

#[tokio::test]
async fn only_nodes_sharing_the_swarm_key_can_join() {
    let key = generate_swarm_key().unwrap();
    let other = generate_swarm_key().unwrap();
    let (a, a_addr) = node(1, Some(&key), false).await;
    let (b, _) = node(2, Some(&key), false).await;
    assert_eq!(b.handle.dial(a_addr.clone()).await.unwrap(), a.handle.peer_id());
    for _ in 0..100 {
        if a.handle.connected_peers().contains(&b.handle.peer_id()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await; // l'événement entrant d'A arrive juste après la fin du dial de B
    }
    assert_eq!(a.handle.connected_peers(), vec![b.handle.peer_id()]);
    let (c, _) = node(3, Some(&other), false).await;
    assert!(c.handle.dial(a_addr.clone()).await.is_err(), "swarm.key différente");
    let (d, _) = node(4, None, false).await;
    assert!(d.handle.dial(a_addr).await.is_err(), "pas de swarm.key");
    assert!(!a.handle.connected_peers().contains(&c.handle.peer_id()) && !a.handle.connected_peers().contains(&d.handle.peer_id()));
}

async fn echo_server() -> std::net::SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn a_service_is_exposed_and_joined_through_the_mesh_without_opening_ports() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(1, Some(&key), false).await;
    let (b, _) = node(2, Some(&key), false).await;
    let (c, _) = node(3, Some(&key), false).await;
    b.handle.dial(a_addr.clone()).await.unwrap();
    c.handle.dial(a_addr).await.unwrap();
    let echo = echo_server().await;
    let allow = HashSet::from([b.handle.peer_id()]);
    let exposed = expose(&a.handle, "echo", echo.to_string(), ExposeOpts { allow: Some(allow), max_streams: 8, ..Default::default() }).unwrap();
    assert_eq!(exposed.protocol, format!("/x/echo-{}", a.handle.peer_id()));

    let fwd = forward(&b.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "echo").await.unwrap();
    let payload = sample(1 << 20, 9);
    let mut conns = Vec::new();
    for i in 0..3u8 {
        let (addr, payload) = (fwd.local_addr, payload.clone());
        conns.push(tokio::spawn(async move {
            let mut s = TcpStream::connect(addr).await.unwrap();
            let data: Vec<u8> = payload.iter().map(|b| b.wrapping_add(i)).collect();
            let (mut r, mut w) = s.split();
            let write = async {
                w.write_all(&data).await.unwrap();
                w.shutdown().await.unwrap();
            };
            let read = async {
                let mut back = Vec::new();
                r.read_to_end(&mut back).await.unwrap();
                back
            };
            let ((), back) = tokio::join!(write, read);
            assert!(back == data, "1 Mo identique à l'aller-retour, connexion {i}");
        }));
    }
    for c in conns {
        c.await.unwrap();
    }

    let fwd_c = forward(&c.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "echo").await.unwrap();
    let mut s = TcpStream::connect(fwd_c.local_addr).await.unwrap();
    let _ = s.write_all(b"coucou").await;
    let mut back = Vec::new();
    let got = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut back)).await;
    assert!(got.is_ok() && back.is_empty(), "aucun octet ne doit revenir : {got:?} {back:?}");

    let id = a.handle.peer_id();
    assert!(protocol_name("ssh", &id).is_ok() && protocol_name("brain-gpu_1.2", &id).is_ok());
    for bad in ["", "SSH", "a b", "a/b", &"x".repeat(33)] {
        assert!(protocol_name(bad, &id).is_err(), "{bad:?}");
    }
    drop(exposed);
}

#[tokio::test]
async fn a_blob_is_pulled_from_a_friend_and_its_integrity_is_verified() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(1, Some(&key), false).await;
    let (b, _) = node(2, Some(&key), false).await;
    b.handle.dial(a_addr).await.unwrap();
    let (da, db, dc) = (TempDir::new("a"), TempDir::new("b"), TempDir::new("c"));
    let store_a = Arc::new(FsBlobStore::open(&da.0).unwrap());
    let store_b = FsBlobStore::open(&db.0).unwrap(); // station B : aucun Kubo, magasin vide
    let data = sample(3_000_000, 5);
    let cid = put_blob(&store_a, &data).await;
    let _server = serve_blobs(&a.handle, store_a.clone(), ServeOpts::default()).unwrap();

    let meta = fetch_from(&b.handle, a.handle.peer_id(), &cid, &store_b, 1 << 30, FETCH).await.unwrap();
    assert_eq!((meta.cid.as_str(), meta.size), (cid.as_str(), 3_000_000));
    let (mut r, len) = store_b.open(&cid).await.unwrap();
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.unwrap();
    assert_eq!((len, got == data), (3_000_000, true));
    assert_eq!(cid_raw_sha256(&Sha256::digest(&got).into()), cid, "intégrité : le CID est l'empreinte SHA-256");

    let missing = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
    let err = fetch_from(&b.handle, a.handle.peer_id(), missing, &store_b, 1 << 30, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("n'a pas ce blob"), "{err}");
    assert!(matches!(fetch_from(&b.handle, a.handle.peer_id(), "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi", &store_b, 1 << 30, FETCH).await, Err(FetchError::BadCid)));
    assert!(matches!(fetch_from(&b.handle, a.handle.peer_id(), "../../etc/passwd", &store_b, 1 << 30, FETCH).await, Err(FetchError::BadCid)));
    let small = FsBlobStore::open(&dc.0).unwrap();
    let err = fetch_from(&b.handle, a.handle.peer_id(), &cid, &small, 1000, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("limite"), "{err}");
}

#[tokio::test]
async fn a_lying_peer_cannot_plant_a_blob_under_a_cid_that_is_not_its_hash() {
    let key = generate_swarm_key().unwrap();
    let (liar, liar_addr) = node(1, Some(&key), false).await;
    let (b, _) = node(2, Some(&key), false).await;
    b.handle.dial(liar_addr).await.unwrap();
    let dir = TempDir::new("liar");
    let store = Arc::new(FsBlobStore::open(&dir.0).unwrap());
    let real_cid = put_blob(&store, b"contenu authentique").await;
    let wanted = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku"; // CID du fichier vide
    std::fs::copy(dir.0.join(&real_cid), dir.0.join(wanted)).unwrap();
    let _server = serve_blobs(&liar.handle, store, ServeOpts::default()).unwrap();
    let victim_dir = TempDir::new("victim");
    let victim = FsBlobStore::open(&victim_dir.0).unwrap();
    let err = fetch_from(&b.handle, liar.handle.peer_id(), wanted, &victim, 1 << 30, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("rejeté"), "{err}");
    let stored: Vec<_> = std::fs::read_dir(&victim_dir.0).unwrap().map(|e| e.unwrap().file_name()).filter(|n| n != ".tmp").collect();
    assert!(stored.is_empty(), "aucun blob validé, aucun résidu : {stored:?}");
    assert_eq!(std::fs::read_dir(victim_dir.0.join(".tmp")).unwrap().count(), 0);
}

#[tokio::test]
async fn peers_are_tried_in_turn_and_the_p2p_store_pulls_missing_blobs_on_demand() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(1, Some(&key), false).await; // possède le blob
    let (e, e_addr) = node(2, Some(&key), false).await; // ami qui ne l'a pas
    let (b, _) = node(3, Some(&key), false).await;
    b.handle.dial(e_addr).await.unwrap();
    b.handle.dial(a_addr).await.unwrap();
    let (da, de, db) = (TempDir::new("a"), TempDir::new("e"), TempDir::new("b"));
    let (store_a, store_e) = (Arc::new(FsBlobStore::open(&da.0).unwrap()), Arc::new(FsBlobStore::open(&de.0).unwrap()));
    let data = sample(500_000, 8);
    let cid = put_blob(&store_a, &data).await;
    let _sa = serve_blobs(&a.handle, store_a, ServeOpts::default()).unwrap();
    let _se = serve_blobs(&e.handle, store_e, ServeOpts::default()).unwrap();

    let store_b = Arc::new(FsBlobStore::open(&db.0).unwrap());
    let meta = fetch_blob(&b.handle, &[e.handle.peer_id(), a.handle.peer_id()], &cid, store_b.as_ref(), 1 << 30, FETCH).await.unwrap();
    assert_eq!(meta.cid, cid);
    store_b.remove(&cid).await.unwrap();
    let none = fetch_blob(&b.handle, &[e.handle.peer_id()], &cid, store_b.as_ref(), 1 << 30, FETCH).await.unwrap_err();
    assert!(matches!(none, FetchError::Unavailable { tried: 1, .. }), "{none}");
    let p2p_store = P2pBlobStore::new(store_b.clone(), b.handle.clone(), vec![]);
    let (mut r, len) = p2p_store.open(&cid).await.unwrap();
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.unwrap();
    assert_eq!((len, got == data), (500_000, true));
    assert!(store_b.open(&cid).await.is_ok(), "le blob est désormais local");
    assert!(p2p_store.open("bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku").await.is_err());
}

#[tokio::test]
async fn behind_a_box_a_station_is_reachable_through_a_relay_circuit() {
    let key = generate_swarm_key().unwrap();
    let (relay, relay_addr) = node(9, Some(&key), true).await;
    let mut public = relay_addr.clone();
    public.pop(); // sans /p2p/<PeerID>
    relay.handle.add_external_address(public).await;
    let (a, _) = node(1, Some(&key), false).await; // « derrière la box » : joint le relais, n'est joint que par lui
    let (b, _) = node(2, Some(&key), false).await;
    let circuit = a.handle.listen_via_relay(relay_addr.clone()).await.unwrap();
    assert!(circuit.to_string().contains("p2p-circuit"), "{circuit}");

    let to_a: Multiaddr = format!("{relay_addr}/p2p-circuit/p2p/{}", a.handle.peer_id()).parse().unwrap();
    assert_eq!(b.handle.dial(to_a).await.unwrap(), a.handle.peer_id());

    let da = TempDir::new("ra");
    let store_a = Arc::new(FsBlobStore::open(&da.0).unwrap());
    let data = sample(700_000, 2);
    let cid = put_blob(&store_a, &data).await;
    let _server = serve_blobs(&a.handle, store_a, ServeOpts::default()).unwrap();
    let db = TempDir::new("rb");
    let store_b = FsBlobStore::open(&db.0).unwrap();
    let meta = fetch_from(&b.handle, a.handle.peer_id(), &cid, &store_b, 1 << 30, FETCH).await.unwrap();
    assert_eq!(meta.cid, cid, "blob rapatrié à travers le circuit relais, intégrité vérifiée");

    let echo = echo_server().await;
    let _exposed = expose(&a.handle, "echo", echo.to_string(), ExposeOpts::default()).unwrap();
    let fwd = forward(&b.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "echo").await.unwrap();
    let mut s = TcpStream::connect(fwd.local_addr).await.unwrap();
    s.write_all(b"bonjour a travers le relais").await.unwrap();
    s.shutdown().await.unwrap();
    let mut back = String::new();
    s.read_to_string(&mut back).await.unwrap();
    assert_eq!(back, "bonjour a travers le relais");
}

#[tokio::test]
async fn a_peer_can_be_asked_whether_it_holds_a_blob_without_transferring_it() {
    use astro_p2p::blob::has_blob;
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(41, Some(&key), false).await;
    let (b, _) = node(42, Some(&key), false).await;
    b.handle.dial(a_addr).await.unwrap();
    let dir = TempDir::new("has");
    let store = Arc::new(FsBlobStore::open(dir.0.join("a")).unwrap());
    let data = sample(300_000, 5);
    let cid = put_blob(&store, &data).await;
    let _server = serve_blobs(&a.handle, store.clone(), ServeOpts::default()).unwrap();

    assert_eq!(has_blob(&b.handle, a.handle.peer_id(), &cid, FETCH).await, Ok(Some(300_000)), "présent : la taille, aucun octet de contenu");
    let absent = cid_raw_sha256(&Sha256::digest(b"absent").into());
    assert_eq!(has_blob(&b.handle, a.handle.peer_id(), &absent, FETCH).await, Ok(None), "absent : ce n'est pas une erreur");
    assert!(has_blob(&b.handle, a.handle.peer_id(), "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi", FETCH).await.is_err(), "CID non échangeable : refusé avant tout flux");
    // Le téléchargement complet fonctionne toujours (le protocole GET n'a pas bougé).
    let dest = FsBlobStore::open(dir.0.join("b")).unwrap();
    assert_eq!(fetch_from(&b.handle, a.handle.peer_id(), &cid, &dest, 1 << 20, FETCH).await.unwrap().cid, cid);
}
