//! Écran des Capitaines entre deux stations réelles : suivre un Capitaine, découvrir les services que **sa** station nous
//! ouvre (et rien d'autre), s'y connecter à chaud, puis ne plus le suivre — l'amitié et l'accès tombent ensemble.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_crypto::StaticKeySource;
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::{UnsignedEvent, pubkey_of};
use astro_p2p::{Multiaddr, P2pConfig, P2pNode};
use astroportd::captains::Captains;
use astroportd::forwards::Forwards;
use astroportd::tunnel_policy::TunnelPolicy;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CAP_A: [u8; 32] = [0xA1; 32];
const CAP_B: [u8; 32] = [0xB2; 32];
const CAP_C: [u8; 32] = [0xC3; 32];

async fn node(seed: u8) -> (P2pNode, Multiaddr) {
    let n = P2pNode::spawn(P2pConfig::local([seed; 32])).await.unwrap();
    let tcp = n.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", n.handle.peer_id()).parse().unwrap();
    (n, full)
}

fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

async fn db(tag: &str) -> (std::path::PathBuf, Arc<Db>) {
    let dir = std::env::temp_dir().join(format!("astroportd-captains-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    (dir, db)
}

async fn captain(db: &Db, s: &[u8; 32], hosted: bool, peer: &str) {
    db.writer().execute("INSERT INTO identities(hex, role, hosted, peer_id) VALUES (?1, 'captain', ?2, ?3)", vec![hex(s).into(), i64::from(hosted).into(), peer.to_owned().into()]).await.unwrap();
}

async fn follows(db: &Db, who: &[u8; 32], whom: &[u8; 32]) {
    let ev = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(whom)]], content: String::new() }.sign(who).unwrap();
    db.writer()
        .transaction(move |tx| {
            insert_event(tx, &ev)?;
            astro_db::apply_statements(tx, &astro_sync::Projector::plan(&ev).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?)
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn following_a_captain_shows_only_what_his_station_opens_to_us_and_unfollowing_closes_it() {
    let ((na, a_addr), (nb, _)) = (node(71).await, node(72).await);
    nb.handle.dial(a_addr).await.unwrap();
    let (pa, pb) = (na.handle.peer_id().to_string(), nb.handle.peer_id().to_string());
    let ((da, db_a), (dbb, db_b)) = (db("a").await, db("b").await);

    // Station A : Capitaine A (hébergé) ; il connaît B (station B) et C (sans station connue).
    captain(&db_a, &CAP_A, true, &pa).await;
    captain(&db_a, &CAP_B, false, &pb).await;
    captain(&db_a, &CAP_C, false, "").await;
    follows(&db_a, &CAP_B, &CAP_A).await; // B suit déjà A
    // Station B : Capitaine B (hébergé), qui suit A ; A le suit aussi (vu par B).
    captain(&db_b, &CAP_B, true, &pb).await;
    captain(&db_b, &CAP_A, false, &pa).await;
    follows(&db_b, &CAP_B, &CAP_A).await;
    follows(&db_b, &CAP_A, &CAP_B).await;

    // B partage un bureau à ses Capitaines amis, et Ollama à « la liste » seulement (personne) : A ne doit voir que le bureau.
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 4];
                if s.read_exact(&mut b).await.is_ok() {
                    let _ = s.write_all(&b).await;
                }
            });
        }
    });
    let policy_b = TunnelPolicy::load(db_b.clone(), false, None, ActivityBus::new()).await;
    let fixed = vec!["ollama".to_owned()];
    policy_b.attach(&nb.handle, &fixed);
    policy_b.set(&json!({ "share": { "name": "bureau-vnc", "target": echo_addr, "mode": "friends" } }), &fixed, 0).await.unwrap();
    policy_b.set(&json!({ "service": "ollama", "mode": "listed" }), &fixed, 0).await.unwrap();

    let forwards = Arc::new(Forwards::new(Some(na.handle.clone())));
    let caps = Captains::new(db_a.clone(), Arc::new(StaticKeySource::new().with(CAP_A)), None, Some(na.handle.clone()), forwards, None, ActivityBus::new());
    let v = caps.view().await.unwrap();
    let row = |v: &serde_json::Value, h: &str| v["captains"].as_array().unwrap().iter().find(|c| c["hex"] == h).unwrap().clone();
    let b = row(&v, &hex(&CAP_B));
    assert_eq!((b["i_follow"].as_bool(), b["follows_me"].as_bool(), b["friend"].as_bool()), (Some(false), Some(true), Some(false)), "{b}");
    assert!(b["services"].is_null(), "pas encore ami : on ne demande rien");

    // A suit B : ami réciproque, la station B répond avec ce qu'elle ouvre à A.
    let v = caps.act(&json!({ "follow": hex(&CAP_B) })).await.unwrap();
    let b = row(&v, &hex(&CAP_B));
    assert_eq!(b["friend"], true, "{b}");
    let names: Vec<&str> = b["services"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["bureau-vnc"], "Ollama est en liste vide : A ne le découvre même pas");
    assert!(caps.act(&json!({ "follow": hex(&CAP_A) })).await.is_err(), "se suivre soi-même");
    assert!(caps.act(&json!({ "follow": "zz" })).await.is_err());
    assert!(caps.act(&json!({ "connect": { "peer": "12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG", "service": "bureau-vnc" } })).await.is_err(), "station d'un inconnu : refusé ici");

    // Se connecter : un accès local relié au bureau de B.
    let v = caps.act(&json!({ "connect": { "peer": pb, "service": "bureau-vnc" } })).await.unwrap();
    let local: std::net::SocketAddr = row(&v, &hex(&CAP_B))["services"][0]["local"]["addr"].as_str().unwrap().parse().unwrap();
    let mut s = tokio::net::TcpStream::connect(local).await.unwrap();
    s.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(8), s.read_exact(&mut back)).await.unwrap().unwrap();
    assert_eq!(&back, b"ping");

    // Ne plus suivre B : la liste de contacts de A ne le nomme plus, et l'écran ne lui demande plus rien.
    let v = caps.act(&json!({ "unfollow": hex(&CAP_B) })).await.unwrap();
    let b = row(&v, &hex(&CAP_B));
    assert_eq!((b["i_follow"].as_bool(), b["friend"].as_bool()), (Some(false), Some(false)));
    assert!(caps.act(&json!({ "connect": { "peer": pb, "service": "bureau-vnc" } })).await.is_err(), "plus ami : plus d'accès depuis cet écran");
    for d in [da, dbb] {
        let _ = std::fs::remove_dir_all(d);
    }
}
