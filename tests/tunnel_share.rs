//! Partage à chaud d'un service de la machine (ex. un bureau à distance) : le Capitaine l'ajoute depuis Options, un ami
//! autorisé le joint par le maillage, puis le retrait le referme — sans redémarrer la station.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_db::Db;
use astro_p2p::tunnel::forward;
use astro_p2p::{Multiaddr, P2pConfig, P2pNode};
use astroportd::tunnel_policy::TunnelPolicy;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn node(seed: u8) -> (P2pNode, Multiaddr) {
    let n = P2pNode::spawn(P2pConfig::local([seed; 32])).await.unwrap();
    let tcp = n.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", n.handle.peer_id()).parse().unwrap();
    (n, full)
}

/// Serveur d'écho : se fait passer pour le serveur VNC de la machine.
async fn echo() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    addr
}

/// Envoie « ping » par l'accès local et rend la réponse (vide si le flux est refusé).
async fn ping(local: std::net::SocketAddr) -> Vec<u8> {
    let Ok(mut s) = TcpStream::connect(local).await else { return Vec::new() };
    if s.write_all(b"ping").await.is_err() {
        return Vec::new();
    }
    let mut buf = vec![0u8; 4];
    match tokio::time::timeout(Duration::from_secs(8), s.read_exact(&mut buf)).await {
        Ok(Ok(_)) => buf,
        _ => Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_captain_shares_a_desktop_port_live_a_named_friend_reaches_it_and_unsharing_closes_it() {
    let dir = std::env::temp_dir().join(format!("astroportd-tunnel-share-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let policy = TunnelPolicy::load(db.clone(), false, None, ActivityBus::new()).await;
    let ((a, a_addr), (b, _), (c, _)) = (node(41).await, node(42).await, node(43).await);
    b.handle.dial(a_addr.clone()).await.unwrap();
    c.handle.dial(a_addr).await.unwrap();
    let fixed = vec!["ssh".to_owned()];
    policy.attach(&a.handle, &fixed);

    // Ajouté sans choisir qui : fermé à tous (seulement la liste, vide).
    let target = echo().await;
    let v = policy.set(&json!({ "share": { "name": "bureau-vnc", "target": target } }), &fixed, 0).await.unwrap();
    let svc = v["services"].as_array().unwrap().iter().find(|s| s["name"] == "bureau-vnc").unwrap().clone();
    assert_eq!((svc["mode"].as_str(), svc["shared"].as_bool()), (Some("listed"), Some(true)), "{v}");
    let fwd_b = forward(&b.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "bureau-vnc").await.unwrap();
    assert!(ping(fwd_b.local_addr).await.is_empty(), "personne n'est encore autorisé");

    // Le Capitaine nomme B pour 2 h : B passe, C non.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    policy.set(&json!({ "service": "bureau-vnc", "grant": { "peer": b.handle.peer_id().to_string(), "label": "Léa", "hours": 2 } }), &fixed, now).await.unwrap();
    assert_eq!(ping(fwd_b.local_addr).await, b"ping");
    let fwd_c = forward(&c.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "bureau-vnc").await.unwrap();
    assert!(ping(fwd_c.local_addr).await.is_empty(), "C n'est pas nommé");

    // Gardé en base : une station relancée retrouve le service et son réglage.
    let again = TunnelPolicy::load(db.clone(), false, None, ActivityBus::new()).await;
    assert_eq!(again.shared_targets(), vec![("bureau-vnc".to_owned(), target.clone())]);
    assert!(again.permits(&b.handle.peer_id().to_string(), "bureau-vnc", now).await);

    // Retrait : plus offert, plus permis.
    policy.set(&json!({ "unshare": "bureau-vnc" }), &fixed, now).await.unwrap();
    assert!(!policy.permits(&b.handle.peer_id().to_string(), "bureau-vnc", now).await);
    assert!(ping(fwd_b.local_addr).await.is_empty(), "service retiré");
    assert!(policy.shared_targets().is_empty());

    // Re-partagé (même nom) : l'écoute précédente est libérée, le service revient.
    policy.set(&json!({ "share": { "name": "bureau-vnc", "target": target, "mode": "swarm" } }), &fixed, now).await.unwrap();
    assert_eq!(ping(fwd_c.local_addr).await, b"ping", "tout le réseau : C passe");
    let _ = std::fs::remove_dir_all(dir);
}
