//! Embarquement des Capitaines sans DNS : deux stations neuves, chacune avec son Capitaine, ne se connaissent que par le
//! maillage. Les machines se découvrent seules (cartes de station, crawl par le tunnel du relai) ; les personnes, elles, ne
//! se suivent que si un Capitaine clique sur « Suivre ». Aucun rôle n'est écrit à la main : tout vient d'événements signés.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_crypto::StaticKeySource;
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::{Event, UnsignedEvent, kind, pubkey_of};
use astro_p2p::tunnel::{ExposeOpts, Exposed, expose};
use astro_p2p::{Multiaddr, P2pConfig, P2pNode};
use astro_relay::{DbCircle, Relay, RelayConfig, serve};
use astro_sync::peers::RELAY_SERVICE;
use astro_sync::{CrawlConfig, Crawler, Signer, WsClient};
use astroportd::captains::Captains;
use astroportd::forwards::Forwards;
use astroportd::p2p_relay::{P2pRelayClient, SharedRelayClient};
use astroportd::swarm_cards::Card;
use astroportd::tunnel_policy::TunnelPolicy;
use serde_json::{Value, json};

/// Une station : base, maillage, relai joint seulement par le maillage, dossier des cartes, écran Capitaines, crawler.
struct Station {
    dir: PathBuf,
    db: Arc<Db>,
    node: P2pNode,
    addr: Multiaddr,
    captain: String,
    home: String,
    caps: Captains,
    crawler: Crawler<SharedRelayClient>,
    _relay: Exposed,
}

async fn station(tag: &str, seed: u8, captain_secret: [u8; 32]) -> Station {
    let dir = std::env::temp_dir().join(format!("astroportd-onboarding-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("swarm")).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let node = P2pNode::spawn(P2pConfig::local([seed; 32])).await.unwrap();
    let tcp = node.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let addr: Multiaddr = format!("{tcp}/p2p/{}", node.handle.peer_id()).parse().unwrap();
    let peer = node.handle.peer_id().to_string();
    let node_hex = pubkey_of(&[seed ^ 0x5A; 32]).unwrap();

    // Capitaine hébergé ici, avec son DID de naissance (sans station d'attache ni marque de Capitaine : comme aujourd'hui).
    let captain = pubkey_of(&captain_secret).unwrap();
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'captain', ?2, 1)", vec![captain.clone().into(), format!("{tag}@exemple.org").into()]).await.unwrap();
    let doc = json!({ "id": format!("did:nostr:{captain}"), "metadata": { "email": format!("{tag}@exemple.org") } });
    let did = UnsignedEvent { created_at: 1_800_000_000, kind: kind::DID, tags: vec![vec!["d".into(), "did".into()], vec!["t".into(), "uplanet".into()]], content: doc.to_string() }.sign(&captain_secret).unwrap();
    store(&db, did).await;
    let profile = UnsignedEvent { created_at: 1_800_000_000, kind: 0, tags: vec![], content: json!({ "name": format!("Capitaine {tag}") }).to_string() }.sign(&captain_secret).unwrap();
    store(&db, profile).await;

    // Relai de la station, exposé par le maillage seulement (aucun domaine, aucun port public).
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().to_string();
    tokio::spawn(serve(listener, Relay::new(db.clone(), circle, RelayConfig::default())));
    let relay = expose(&node.handle, RELAY_SERVICE, local, ExposeOpts::default()).unwrap();

    // Carte de station : sans relai public (`my_relay` absent).
    let card = Arc::new(Card { db: db.clone(), handle: node.handle.clone(), hostname: tag.into(), node_hex: node_hex.clone(), my_relay: None, origin: None });
    std::mem::forget(card.serve().unwrap());

    let swarm = dir.join("swarm");
    let caps = Captains::new(db.clone(), Arc::new(StaticKeySource::new().with(captain_secret)), None, Some(node.handle.clone()), Arc::new(Forwards::new(Some(node.handle.clone()))), ActivityBus::new()).with_swarm(Some(swarm.clone()));
    // Crawler câblé comme `serve` : relais et Capitaines tirés des cartes du maillage.
    // Comme en production : la station s'authentifie (NIP-42) avec la clé de son NODE.
    let ws = WsClient::new(Duration::from_secs(5)).with_signer(Signer::new([seed ^ 0x5A; 32]));
    let client = SharedRelayClient(Arc::new(P2pRelayClient::new(ws, Some(node.handle.clone()))));
    let (relays_dir, own) = (swarm.clone(), peer.clone());
    let relays: astro_sync::RelaySource = Arc::new(move || astro_sync::read_peers(&relays_dir).iter().filter(|p| p.node_id != own).map(astro_sync::Peer::target).collect());
    let crawler = Crawler::new(db.clone(), client, CrawlConfig::default()).with_relay_source(relays).with_keys_source(astroportd::swarm_cards::captain_keys(Some(swarm), Some(peer.clone())));
    Station { dir, db, node, addr, captain, home: format!("{peer}:{node_hex}"), caps, crawler, _relay: relay }
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

fn row(view: &Value, hex: &str) -> Option<Value> {
    view["captains"].as_array().unwrap().iter().find(|c| c["hex"] == hex).cloned()
}

fn flags(c: &Value) -> (bool, bool, bool, bool) {
    (c["verified"].as_bool().unwrap(), c["i_follow"].as_bool().unwrap(), c["follows_me"].as_bool().unwrap(), c["friend"].as_bool().unwrap())
}

async fn crawl(s: &Station) -> astro_sync::CrawlStats {
    let r = s.crawler.pass().await;
    let stats = r.result.unwrap();
    assert!(stats.errors.is_empty(), "{:?}", stats.errors);
    stats
}

#[tokio::test(flavor = "multi_thread")]
async fn two_fresh_stations_without_dns_discover_their_captains_and_only_a_click_makes_them_friends() {
    let (a, b) = (station("a", 81, [0xA7; 32]).await, station("b", 82, [0xB8; 32]).await);
    a.node.handle.dial(b.addr.clone()).await.unwrap();

    // Avant tout : chaque Capitaine ne connaît que lui-même.
    assert!(a.caps.view().await.unwrap()["captains"].as_array().unwrap().is_empty());

    // Chaque station annonce son Capitaine (DID `t = captain` + station d'attache) ; une seconde annonce ne republie rien.
    assert!(a.caps.announce(&a.home).await.unwrap());
    assert!(b.caps.announce(&b.home).await.unwrap());
    assert!(!a.caps.announce(&a.home).await.unwrap(), "rien de neuf : pas de nouvelle version du DID");

    // Échange réel des cartes par le maillage (protocole `/astroport/station/1`), dans les deux sens.
    let (_stop_tx, stop) = tokio::sync::watch::channel(false);
    for s in [&a, &b] {
        tokio::spawn(astroportd::swarm_cards::keep_every(s.node.handle.clone(), s.dir.join("swarm"), Duration::ZERO, Duration::from_secs(3600), stop.clone()));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while astro_sync::read_peers(&a.dir.join("swarm")).is_empty() || astro_sync::read_peers(&b.dir.join("swarm")).is_empty() {
        assert!(std::time::Instant::now() < deadline, "cartes non échangées");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(astro_sync::read_peers(&a.dir.join("swarm"))[0].captain_hex.as_deref(), Some(b.captain.as_str()));

    // Premier passage du crawl : A va chercher le Capitaine B chez B, par le tunnel du relai.
    crawl(&a).await;
    let role: (String, i64, String) = a.db.readers().read({ let h = b.captain.clone(); move |c| Ok(c.query_row("SELECT role, hosted, home_station FROM identities WHERE hex = ?1", [h], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?) }).await.unwrap();
    assert_eq!(role, ("captain".to_owned(), 0, b.home.clone()), "le DID signé de B le dit Capitaine de sa station");

    // Sur A : B apparaît, confirmé par sa carte, personne ne suit personne.
    let v = a.caps.view().await.unwrap();
    assert!(row(&v, &b.captain).is_some(), "le Capitaine B apparaît sur A : {v}");
    let rb = row(&v, &b.captain).unwrap();
    assert_eq!(flags(&rb), (true, false, false, false), "{rb}");
    assert_eq!(rb["label"], "Capitaine b");
    assert!(rb["services"].is_null(), "pas ami : on ne demande rien");

    // A clique sur « Suivre ». B, au passage suivant de son crawl, voit « vous suit ».
    a.caps.act(&json!({ "follow": b.captain })).await.unwrap();
    crawl(&b).await;
    let vb = b.caps.view().await.unwrap();
    assert!(row(&vb, &a.captain).is_some(), "le Capitaine A apparaît sur B : {vb}");
    let ra = row(&vb, &a.captain).unwrap();
    assert_eq!(flags(&ra), (true, false, true, false), "{ra}");

    // B partage un service à ses Capitaines amis, puis suit A en retour : les deux sont amis.
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap().to_string();
    tokio::spawn(async move { while echo.accept().await.is_ok() {} });
    b.caps.act(&json!({ "follow": a.captain })).await.unwrap();
    let policy_b = TunnelPolicy::load(b.db.clone(), false, None, ActivityBus::new()).await;
    policy_b.attach(&b.node.handle, &[]);
    policy_b.set(&json!({ "share": { "name": "bureau-vnc", "target": echo_addr, "mode": "friends" } }), &[], 0).await.unwrap();
    // A suit B : B entre dans son N¹ et A lui demande aussi ses kinds privés. Le relai de B ne compte pas le NODE de A dans son
    // cercle : il refuse, et A relit ce qui est public (dont la liste de contacts de B, qui dit qu'il suit A).
    let stats = crawl(&a).await;
    assert!(stats.public_only >= 1, "{stats:?}");

    let rb = row(&a.caps.view().await.unwrap(), &b.captain).unwrap();
    assert_eq!(flags(&rb), (true, true, true, true), "{rb}");
    assert!(rb["services"].is_array(), "{rb}");
    let names: Vec<&str> = rb["services"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["bureau-vnc"], "la station de B ouvre son service à son ami A");
    assert_eq!(flags(&row(&b.caps.view().await.unwrap(), &a.captain).unwrap()), (true, true, true, true));

    let _ = (std::fs::remove_dir_all(&a.dir), std::fs::remove_dir_all(&b.dir));
}

#[tokio::test]
async fn a_did_that_claims_to_be_captain_is_not_enough_to_appear_nor_to_take_over_a_station() {
    let dir = std::env::temp_dir().join(format!("astroportd-onboarding-claim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("swarm")).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let (me, mallory, member) = ([0x11; 32], [0x22; 32], [0x33; 32]);
    db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'captain', 1)", vec![pubkey_of(&me).unwrap().into()]).await.unwrap();
    let claim = |secret: &[u8; 32], at: u64, captain: bool| {
        let hex = pubkey_of(secret).unwrap();
        let mut tags = vec![vec!["d".into(), "did".into()], vec!["home_station".into(), format!("12D3KooWVictime:{}", "ab".repeat(32))]];
        if captain {
            tags.push(vec!["t".into(), "captain".into()]);
        }
        UnsignedEvent { created_at: at, kind: kind::DID, tags, content: json!({ "id": format!("did:nostr:{hex}") }).to_string() }.sign(secret).unwrap()
    };
    // Mallory se dit Capitaine de la station « Victime », dont aucune carte ne le nomme.
    store(&db, claim(&mallory, 10, true)).await;
    let role: String = db.readers().read({ let h = pubkey_of(&mallory).unwrap(); move |c| Ok(c.query_row("SELECT role FROM identities WHERE hex = ?1", [h], |r| r.get(0))?) }).await.unwrap();
    assert_eq!(role, "captain", "le rôle suit le DID signé…");
    let caps = Captains::new(db.clone(), Arc::new(StaticKeySource::new().with(me)), None, None, Arc::new(Forwards::new(None)), ActivityBus::new()).with_swarm(Some(dir.join("swarm")));
    assert!(caps.view().await.unwrap()["captains"].as_array().unwrap().is_empty(), "…mais sans carte qui le confirme, il n'apparaît pas");

    // Un DID plus récent sans la marque : redevient un simple membre.
    store(&db, claim(&mallory, 20, false)).await;
    let role: String = db.readers().read({ let h = pubkey_of(&mallory).unwrap(); move |c| Ok(c.query_row("SELECT role FROM identities WHERE hex = ?1", [h], |r| r.get(0))?) }).await.unwrap();
    assert_eq!(role, "multipass");

    // Un membre hébergé ici qui se dit Capitaine ne le devient pas ; un Capitaine distant hébergé ici redevient membre.
    db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'multipass', 1)", vec![pubkey_of(&member).unwrap().into()]).await.unwrap();
    store(&db, claim(&member, 10, true)).await;
    store(&db, claim(&mallory, 30, true)).await;
    db.writer().transaction({ let h = pubkey_of(&mallory).unwrap(); move |tx| astro_db::apply_statements(tx, &[astro_sync::Projector::mark_hosted(&h, "multipass")]) }).await.unwrap();
    let hosted_captains: i64 = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM identities WHERE hosted = 1 AND role = 'captain'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(hosted_captains, 1, "seul le Capitaine désigné ici reste Capitaine de la station");
    let _ = std::fs::remove_dir_all(dir);
}
