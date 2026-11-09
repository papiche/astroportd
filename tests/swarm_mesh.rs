//! Deux stations derrière une box, qui ne se connaissent pas, et une station relais : la configuration coopérative donne
//! le relais (`P2P_RELAYS`), chaque station y réserve sa place, la balise de l'une (kind 30850, `p2p:addrs`) suffit à
//! l'autre pour la joindre par le relais, puis la carte de station remplit le dossier `swarm/` — sans Astroport.ONE et
//! sans qu'aucun Capitaine ait copié d'adresse.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use astro_p2p::{Multiaddr, P2pConfig, P2pNode, generate_swarm_key, parse_swarm_key};
use astroportd::beacon::{P2P_TAG, Stations};
use astroportd::mesh_admin::Mesh;
use astroportd::swarm_cards::Card;

const UP: &str = "ConstellationDeTest";

async fn node(seed: u8, key: &str, relay_server: bool) -> P2pNode {
    let mut cfg = P2pConfig::local([seed; 32]);
    cfg.psk = Some(parse_swarm_key(key).unwrap());
    cfg.relay_server = relay_server;
    P2pNode::spawn(cfg).await.unwrap()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("astroportd-swarm-mesh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn store(db: &Db, ev: Event) {
    db.writer()
        .transaction(move |tx| {
            insert_event(tx, &ev)?;
            astro_db::apply_statements(tx, &astro_sync::Projector::plan(&ev).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?)
        })
        .await
        .unwrap();
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_stations_behind_a_box_find_each_other_through_the_cooperative_relay_and_the_beacon() {
    let key = generate_swarm_key().unwrap();
    let relay = node(81, &key, true).await;
    let tcp = relay.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    relay.handle.add_external_address(tcp.clone()).await;
    let relay_addr: Multiaddr = format!("{tcp}/p2p/{}", relay.handle.peer_id()).parse().unwrap();
    let (a, b) = (node(82, &key, false).await, node(83, &key, false).await);

    // La coopérative publie la station relais ; chaque station l'a reçue par la synchronisation.
    let patch: BTreeMap<String, Option<String>> = [("P2P_RELAYS".to_owned(), Some(relay_addr.to_string()))].into();
    let coop = astroportd::coop_config::build_event(None, &patch, UP, 1_000).unwrap();
    let (da, db_b) = (tmp("a"), tmp("b"));
    let (dba, dbb) = (Arc::new(Db::open(&da.join("u.db"), 2).unwrap()), Arc::new(Db::open(&db_b.join("u.db"), 2).unwrap()));
    store(&dba, coop.clone()).await;
    store(&dbb, coop).await;

    // A réserve sa place chez le relais, sans qu'aucune option ne le lui dise.
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    let mesh_a = Arc::new(Mesh::new(dba.clone(), Some(a.handle.clone()), Vec::new(), UP.into(), ActivityBus::new()).with_relays(Vec::new(), Some(UP.into()), false));
    tokio::spawn(mesh_a.clone().keep_alive(Duration::from_secs(300), stop.clone()));
    until("A doit réserver sa place chez le relais de la coopérative", || a.handle.reserved_relays() == vec![relay.handle.peer_id()]).await;
    let view = mesh_a.view().await;
    assert_eq!((view["relays"][0]["source"].as_str(), view["relays"][0]["reserved"].as_bool()), (Some("coopérative"), Some(true)));

    // La balise de A annonce sa place chez le relais ; B la reçoit (relai de la coopérative) dans son annuaire.
    let me_a = a.handle.peer_id();
    let circuit: Vec<String> = a.handle.listen_addrs().iter().map(ToString::to_string).filter(|x| x.contains("/p2p-circuit")).map(|x| if x.ends_with(&format!("/p2p/{me_a}")) { x } else { format!("{x}/p2p/{me_a}") }).collect();
    assert!(!circuit.is_empty());
    let swarm_id = astroportd::coop_wallets::uplanet_g1pub(UP).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let tags = vec![vec!["d".into(), "economic-health".into()], vec!["station".into(), me_a.to_string()], vec!["swarm_id".into(), swarm_id], vec![P2P_TAG.into(), circuit.join(" ")]];
    store(&dbb, UnsignedEvent { created_at: now, kind: kind::ECONOMIC_HEALTH, tags, content: "{}".into() }.sign(&[0xAA; 32]).unwrap()).await;

    // B réserve aussi sa place, lit l'annuaire et joint A par le relais : elles ne se connaissaient pas.
    let stations = Arc::new(Stations::new(dbb.clone(), UP.into(), Some(b.handle.peer_id().to_string())));
    assert_eq!(stations.p2p_addrs(now).await.len(), circuit.len(), "l'annuaire rend les adresses de A");
    let mesh_b = Arc::new(Mesh::new(dbb.clone(), Some(b.handle.clone()), Vec::new(), UP.into(), ActivityBus::new()).with_relays(Vec::new(), Some(UP.into()), false).with_directory(stations, Arc::new(tokio::sync::Notify::new())));
    tokio::spawn(mesh_b.keep_alive(Duration::from_secs(300), stop.clone()));
    until("B doit joindre A, trouvée dans l'annuaire des balises", || b.handle.connected_peers().contains(&me_a)).await;

    // La carte de A arrive dans le dossier swarm de B, lisible comme un 12345.json d'Astroport.ONE.
    let card = Arc::new(Card { db: dba.clone(), handle: a.handle.clone(), hostname: "station-a".into(), node_hex: "ab".repeat(32), my_relay: None, origin: None });
    let _answers = card.serve().unwrap();
    let swarm = db_b.join("swarm");
    tokio::spawn(astroportd::swarm_cards::keep_every(b.handle.clone(), swarm.clone(), Duration::ZERO, Duration::from_secs(1), stop));
    until("la carte de A doit être écrite dans le dossier swarm de B", || astro_sync::read_peers(&swarm).iter().any(|p| p.node_id == me_a.to_string())).await;
    let peers = astro_sync::read_peers(&swarm);
    let pa = peers.iter().find(|p| p.node_id == me_a.to_string()).unwrap();
    assert_eq!((pa.hostname.as_deref(), pa.node_hex.as_deref()), (Some("station-a"), Some("ab".repeat(32).as_str())));
    let _ = stop_tx.send(true);
    let _ = std::fs::remove_dir_all(&da);
    let _ = std::fs::remove_dir_all(&db_b);
}
