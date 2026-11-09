//! Routage vers les services de la constellation, sans `12345.json` : l'annuaire des balises donne les stations et leur
//! Power-Score, chaque station dit ce qu'**elle nous ouvre**, et `tunnel connect` ouvre l'accès local choisi. Puis la prise
//! de contrôle (`control.sock`) qui porte les commandes `astroportd tunnel …`.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use astro_p2p::routing::Mode;
use astro_p2p::{Multiaddr, P2pConfig, P2pNode};
use astroportd::beacon::Stations;
use astroportd::forwards::Forwards;
use astroportd::swarm_route::SwarmRoute;
use astroportd::tunnel_policy::TunnelPolicy;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const UP: &str = "ConstellationDeRoutage";

async fn node(seed: u8) -> (P2pNode, Multiaddr) {
    let n = P2pNode::spawn(P2pConfig::local([seed; 32])).await.unwrap();
    let tcp = n.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", n.handle.peer_id()).parse().unwrap();
    (n, full)
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("astroportd-route-{tag}-{}", std::process::id()));
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

/// Balise 30850 d'une station : PeerID, Power-Score, position.
async fn beacon(db: &Db, key: u8, peer: &str, name: &str, power: u32, pos: Option<(&str, &str)>) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let mut tags = vec![
        vec!["d".into(), "economic-health".into()],
        vec!["station".into(), peer.to_owned()],
        vec!["station:name".into(), name.to_owned()],
        vec!["swarm_id".into(), astroportd::coop_wallets::uplanet_g1pub(UP).unwrap()],
        vec!["hw:power_score".into(), power.to_string()],
    ];
    if let Some((lat, lon)) = pos {
        tags.push(vec!["geo:lat".into(), lat.into()]);
        tags.push(vec!["geo:lon".into(), lon.into()]);
    }
    store(db, UnsignedEvent { created_at: now, kind: kind::ECONOMIC_HEALTH, tags, content: "{}".into() }.sign(&[key; 32]).unwrap()).await;
}

async fn echo() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 4];
                if s.read_exact(&mut b).await.is_ok() {
                    let _ = s.write_all(&b).await;
                }
            });
        }
    });
    addr
}

/// Une station qui offre `ollama` selon `mode`, et dont le service renvoie ce qu'on lui envoie.
async fn provider(node: &P2pNode, tag: &str, mode: &str) -> std::path::PathBuf {
    let dir = tmp(tag);
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let policy = TunnelPolicy::load(db, false, None, ActivityBus::new()).await;
    let fixed = vec!["ollama".to_owned()];
    policy.attach(&node.handle, &fixed);
    policy.set(&json!({ "service": "ollama", "mode": mode }), &fixed, 0).await.unwrap();
    let target = echo().await;
    // Le service local d'`ollama` : exposé derrière la politique, comme le fait `serve`.
    let opts = astro_p2p::tunnel::ExposeOpts { gate: Some(Arc::new(astroportd::tunnel_policy::PolicyGate::new(policy.clone()))), ..Default::default() };
    std::mem::forget(astro_p2p::tunnel::expose(&node.handle, "ollama", target, opts).unwrap());
    std::mem::forget(policy);
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn route_picks_the_strongest_station_that_opens_the_service_to_us_and_connect_opens_a_local_port() {
    let ((na, _), (nb, b_addr), (nc, c_addr), (nd, d_addr)) = (node(91).await, node(92).await, node(93).await, node(94).await);
    for addr in [b_addr, c_addr, d_addr] {
        na.handle.dial(addr).await.unwrap();
    }
    let (pa, pb, pc, pd) = (na.handle.peer_id().to_string(), nb.handle.peer_id().to_string(), nc.handle.peer_id().to_string(), nd.handle.peer_id().to_string());
    // B (20) et C (80) offrent ollama à tout le réseau ; D (99) l'a fermé : plus puissante, mais inutilisable.
    let dirs = [provider(&nb, "b", "swarm").await, provider(&nc, "c", "swarm").await, provider(&nd, "d", "off").await];
    let da = tmp("a");
    let dba = Arc::new(Db::open(&da.join("u.db"), 2).unwrap());
    beacon(&dba, 1, &pa, "moi", 10, Some(("45.76", "4.83"))).await; // Lyon
    beacon(&dba, 2, &pb, "b-lyon", 20, Some(("45.75", "4.85"))).await;
    beacon(&dba, 3, &pc, "c-marseille", 80, Some(("43.30", "5.37"))).await;
    beacon(&dba, 4, &pd, "d-ferme", 99, None).await;

    let stations = Arc::new(Stations::new(dba.clone(), UP.into(), Some(pa.clone())));
    let forwards = Arc::new(Forwards::new(Some(na.handle.clone())));
    let route = SwarmRoute::new(stations, Some(na.handle.clone()), forwards, None);

    // Puissance : C. D, plus fort, ne vous ouvre pas ollama : il n'est pas même candidat.
    let (peer, report) = route.route("ollama", Mode::Power).await.unwrap();
    assert_eq!((peer.as_str(), report["candidates"].as_u64()), (pc.as_str(), Some(2)), "{report}");
    // Proximité : B, à Lyon comme nous.
    let (peer, report) = route.route("ollama", Mode::Closest).await.unwrap();
    assert_eq!(peer, pb, "{report}");
    assert!(route.route("comfyui", Mode::Power).await.unwrap_err().contains("aucune station ne vous ouvre"), "service que personne n'offre");

    // La vue dit, station par station, ce qui nous est ouvert.
    let v = route.act(&json!({})).await.unwrap();
    let by_name = |n: &str| v["stations"].as_array().unwrap().iter().find(|s| s["name"] == n).unwrap().clone();
    assert_eq!(by_name("c-marseille")["services"][0]["name"], "ollama");
    assert_eq!(by_name("d-ferme")["services"].as_array().unwrap().len(), 0, "fermé à nous : rien n'est annoncé");

    // Connexion automatique sur un port libre : le flux traverse le maillage jusqu'au service de C.
    let r = route.act(&json!({ "connect": { "service": "ollama", "mode": "power", "hours": 1 } })).await.unwrap();
    assert_eq!(r["peer"], pc.as_str());
    let mut s = tokio::net::TcpStream::connect(r["addr"].as_str().unwrap()).await.unwrap();
    s.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(8), s.read_exact(&mut back)).await.unwrap().unwrap();
    assert_eq!(&back, b"ping");
    let v = route.act(&json!({})).await.unwrap();
    assert!(by_open(&v, "c-marseille").is_some_and(|a| a == r["addr"].as_str().unwrap()), "l'accès ouvert est visible dans la vue");

    // Mauvaises demandes : refusées sans rien ouvrir.
    for bad in [json!({ "connect": { "service": "ollama", "hours": 0 } }), json!({ "connect": { "service": "ollama", "hours": 9999 } }), json!({ "connect": { "service": "ollama", "port": 70000 } }), json!({ "connect": { "service": "" } }), json!({ "connect": { "service": "ollama", "peer": "pas-un-peer" } }), json!({ "format": "disque" })] {
        assert!(route.act(&bad).await.is_err(), "{bad}");
    }
    // Fermeture : par service, puis plus rien à fermer.
    assert_eq!(route.act(&json!({ "disconnect": { "service": "ollama" } })).await.unwrap()["closed"], 1);
    assert!(route.act(&json!({ "disconnect": { "service": "ollama" } })).await.is_err());

    for d in dirs.into_iter().chain([da]) {
        let _ = std::fs::remove_dir_all(d);
    }
}

fn by_open(v: &serde_json::Value, name: &str) -> Option<String> {
    v["stations"].as_array()?.iter().find(|s| s["name"] == name)?["services"][0]["local"]["addr"].as_str().map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_control_socket_serves_only_declared_sections_to_its_owner_and_refuses_hostile_requests() {
    use astroportd::control;
    let dir = tmp("ctl");
    let path = dir.join("control.sock");
    let mut sections: BTreeMap<&'static str, astroportd::ui_backend::AdminFn> = BTreeMap::new();
    sections.insert("echo", Arc::new(|_s, body| Box::pin(async move { Ok(json!({ "got": body })) })));
    sections.insert("boom", Arc::new(|_s, _b| Box::pin(async { Err("refusé par la section".to_owned()) })));
    let _ctl = control::serve(path.clone(), sections.clone()).unwrap();

    assert_eq!(control::call(&path, "echo", Some(json!({ "x": 1 }))).await.unwrap(), json!({ "got": { "x": 1 } }));
    assert_eq!(control::call(&path, "boom", None).await.unwrap_err(), "refusé par la section");
    let err = control::call(&path, "console", None).await.unwrap_err();
    assert!(err.contains("inconnue") && err.contains("echo"), "section non déclarée : {err}");

    // Droits : socket réservé à son propriétaire.
    let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).unwrap().permissions()) & 0o777;
    assert_eq!(mode, 0o600);

    // Demande hostile : trop longue, sans fin de ligne, JSON cassé.
    // (Trop longue : le démon ne lit que 64 Kio puis referme ; selon le noyau le client lit l'erreur ou une coupure.)
    let mut big = tokio::net::UnixStream::connect(&path).await.unwrap();
    let _ = big.write_all(&vec![b'x'; 70 * 1024]).await;
    let mut out = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(8), big.read_to_string(&mut out)).await.unwrap();
    assert!(!out.contains("\"ok\""), "jamais acceptée : {out:?}");
    for raw in [b"{\"section\":\"echo\"".to_vec(), b"pas du json\n".to_vec()] {
        let mut s = tokio::net::UnixStream::connect(&path).await.unwrap();
        let _ = s.write_all(&raw).await;
        let _ = s.shutdown().await;
        let mut out = String::new();
        let _ = tokio::time::timeout(Duration::from_secs(8), s.read_to_string(&mut out)).await.unwrap();
        assert!(out.contains("\"error\""), "refusée : {out:?}");
    }
    // Le démon tient toujours après ces demandes.
    assert!(control::call(&path, "echo", None).await.is_ok());

    // Un fichier ordinaire au même endroit n'est jamais écrasé.
    drop(_ctl);
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(&path, b"precieux").unwrap();
    assert!(control::serve(path.clone(), sections).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"precieux");
    let _ = std::fs::remove_dir_all(dir);
}
