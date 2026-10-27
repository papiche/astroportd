//! Cycle complet de la régie des communs : tarif publié → achat d'une part 1/24 en Ẑen (virement 30852 `t=lease`) →
//! bail validé → ouverture autorisée du flux P2P vers ComfyUI ; refusée sans bail, sous-payé, expiré ou trop tard.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use astro_core::Amount;
use astro_db::{Db, events};
use astro_economy::lease::{LeaseRequest, Resource, Tariff};
use astro_nostr_proto::{Event, UnsignedEvent, kind, pubkey_of};
use astro_p2p::tunnel::{ExposeOpts, expose, forward};
use astro_p2p::{Multiaddr, P2pConfig, P2pNode};
use astro_sync::Projector;
use astroportd::lease_gate::LeaseGate;
use rusqlite::types::Value as V;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const NODE_A: [u8; 32] = [1; 32]; // station qui loue son GPU
const BOB: [u8; 32] = [4; 32]; // MULTIPASS hébergé par la station B
const EVE: [u8; 32] = [5; 32]; // MULTIPASS hébergé par la station C
const T0: u64 = 1_800_000_000;

fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

/// Stocke l'événement comme le ferait le relai : insertion et projection dans la même transaction.
async fn ingest(db: &Db, e: Event) {
    db.writer()
        .transaction(move |tx| {
            events::insert_event(tx, &e)?;
            astro_db::apply_statements(tx, &Projector::plan(&e).unwrap())?;
            Ok(())
        })
        .await
        .unwrap();
}

fn tariff_event(at: u64) -> Event {
    let mut t = Tariff::default();
    t.prices.insert(Resource::Gpu1of24, Amount(50)); // 0,50 Ẑen par semaine
    t.prices.insert(Resource::Disk128, Amount(400));
    UnsignedEvent { created_at: at, kind: kind::ECONOMIC_HEALTH, tags: t.to_tags(), content: String::new() }.sign(&NODE_A).unwrap()
}

fn purchase(buyer: &[u8; 32], amount: &str, req: &LeaseRequest, at: u64, d: &str) -> Event {
    let mut tags = vec![vec!["d".into(), d.into()], vec!["p".into(), hex(&NODE_A)], vec!["amount".into(), amount.into()], vec!["prev".into(), "genesis".into()], vec!["t".into(), "g1-n2".into()]];
    tags.extend(req.to_tags());
    UnsignedEvent { created_at: at, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(buyer).unwrap()
}

async fn lease_status(db: &Db, src: &str) -> Option<String> {
    let src = src.to_owned();
    db.readers().read(move |c| Ok(c.query_row("SELECT status FROM resource_leases WHERE src_event = ?1", [src], |r| r.get::<_, String>(0)).ok())).await.unwrap()
}

async fn host(db: &Db, owner: &[u8; 32], role: &str, home: Option<&str>) {
    db.writer()
        .execute(
            "INSERT INTO identities(hex, role, email, home_station, hosted) VALUES (?1, ?2, ?3, ?4, 0)",
            vec![V::Text(hex(owner)), V::Text(role.into()), V::Text(format!("{}@example.org", &hex(owner)[..6])), home.map_or(V::Null, |h| V::Text(h.into()))],
        )
        .await
        .unwrap();
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

async fn p2p(seed: u8) -> (P2pNode, Multiaddr) {
    let node = P2pNode::spawn(P2pConfig::local([seed; 32])).await.unwrap();
    let tcp = node.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", node.handle.peer_id()).parse().unwrap();
    (node, full)
}

/// Une conversation à travers le tunnel : `true` si le service répond (écho), `false` si le flux est refermé.
async fn talk(addr: std::net::SocketAddr) -> bool {
    let Ok(mut s) = TcpStream::connect(addr).await else { return false };
    if s.write_all(b"POST /prompt").await.is_err() {
        return false;
    }
    let mut back = [0u8; 12];
    matches!(tokio::time::timeout(Duration::from_secs(3), s.read_exact(&mut back)).await, Ok(Ok(_))) && &back == b"POST /prompt"
}

#[tokio::test(flavor = "multi_thread")]
async fn buying_a_gpu_share_opens_the_comfyui_tunnel_and_nothing_else_does() {
    let dir = std::env::temp_dir().join(format!("astroportd-lease-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());

    // Trois stations : A loue son GPU ; B héberge Bob ; C héberge Eve.
    let (a, a_addr) = p2p(10).await;
    let (b, _) = p2p(11).await;
    let (c, _) = p2p(12).await;
    b.handle.dial(a_addr.clone()).await.unwrap();
    c.handle.dial(a_addr).await.unwrap();
    // A connaît ses voisins par les DID/kind 0 répliqués : Bob vit chez B, Eve chez C.
    host(&db, &NODE_A, "node", None).await;
    host(&db, &BOB, "multipass", Some(&format!("{}:{}", b.handle.peer_id(), hex(&[2; 32])))).await;
    host(&db, &EVE, "multipass", Some(&format!("{}:{}", c.handle.peer_id(), hex(&[3; 32])))).await;

    // 1. Le nœud publie ses tarifs.
    ingest(&db, tariff_event(T0 - 100)).await;
    let price: i64 = db.readers().read(|c| Ok(c.query_row("SELECT price_cents FROM tariffs WHERE resource = 'gpu_1_24'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(price, 50);

    // 2. Bob achète 3 semaines à partir de T0 : 1,50 Ẑen ; Eve n'achète que pour 0,50 Ẑen (sous-payé).
    let req = LeaseRequest { resource: Resource::Gpu1of24, start: T0, units: 3, unit_price: Amount(50) };
    let bob_buy = purchase(&BOB, "1.50", &req, T0, "lease-bob");
    let eve_buy = purchase(&EVE, "0.50", &req, T0, "lease-eve");
    let (bob_id, eve_id) = (bob_buy.id.clone(), eve_buy.id.clone());
    ingest(&db, bob_buy).await;
    ingest(&db, eve_buy).await;
    assert_eq!(lease_status(&db, &bob_id).await.as_deref(), Some("active"));
    assert_eq!(lease_status(&db, &eve_id).await.as_deref(), Some("underpaid"));

    // 3. A expose ComfyUI derrière la garde ; l'horloge est réglable.
    let clock = Arc::new(AtomicU64::new(T0 + 600));
    let gate = LeaseGate::new(db.clone(), hex(&NODE_A), None).with_clock({
        let clock = clock.clone();
        Arc::new(move || clock.load(Ordering::SeqCst))
    });
    let comfy = echo_server().await;
    let _exposed = expose(&a.handle, "comfyui", comfy.to_string(), ExposeOpts { gate: Some(Arc::new(gate)), watch_every: Duration::from_millis(100), ..Default::default() }).unwrap();
    // Un service non lourd n'est pas gardé : ssh reste ouvert à tout le monde du maillage.
    let ssh = echo_server().await;
    let _ssh = expose(&a.handle, "ssh", ssh.to_string(), ExposeOpts::default()).unwrap();

    let from_b = forward(&b.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "comfyui").await.unwrap();
    let from_c = forward(&c.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "comfyui").await.unwrap();
    let ssh_from_c = forward(&c.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "ssh").await.unwrap();

    // Bob (bail actif) : le flux s'ouvre. Eve (sous-payé) : refusé. SSH : ouvert sans bail.
    assert!(talk(from_b.local_addr).await, "le détenteur d'un bail ouvre le tunnel ComfyUI");
    assert!(!talk(from_c.local_addr).await, "un bail sous-payé n'ouvre rien");
    assert!(talk(ssh_from_c.local_addr).await, "les services non lourds ne sont pas soumis aux baux");

    // 4. Eve complète son achat (1,50 Ẑen) : son flux s'ouvre à son tour.
    let eve_full = purchase(&EVE, "1.50", &LeaseRequest { start: T0 + 5, ..req.clone() }, T0 + 5, "lease-eve-2");
    let eve_full_id = eve_full.id.clone();
    ingest(&db, eve_full).await;
    assert_eq!(lease_status(&db, &eve_full_id).await.as_deref(), Some("active"));
    assert!(talk(from_c.local_addr).await);

    // 5. Watchdog : une session OUVERTE pendant le créneau est coupée dès que le bail expire, sans attendre
    //    qu'elle se ferme d'elle-même (le service lourd ne tourne pas gratuitement).
    clock.store(T0 + 3 * 7 * 86_400 - 1, Ordering::SeqCst);
    let mut session = TcpStream::connect(from_b.local_addr).await.unwrap();
    session.write_all(b"ping").await.unwrap();
    let mut echo = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(3), session.read_exact(&mut echo)).await.unwrap().unwrap();
    assert_eq!(&echo, b"ping");
    clock.store(T0 + 3 * 7 * 86_400, Ordering::SeqCst); // le créneau se termine pendant la session
    let mut rest = Vec::new();
    let cut = tokio::time::timeout(Duration::from_secs(3), session.read_to_end(&mut rest)).await;
    assert!(matches!(cut, Ok(Ok(_)) | Ok(Err(_))), "la session doit être coupée par le watchdog, pas rester ouverte : {cut:?}");
    assert!(rest.is_empty());

    // 6. Le créneau est fini : plus de nouveau flux, ni avant le début d'un bail futur.
    assert!(!talk(from_b.local_addr).await, "bail expiré : flux refusé");
    clock.store(T0 - 10, Ordering::SeqCst);
    assert!(!talk(from_b.local_addr).await, "bail pas encore commencé : flux refusé");
    clock.store(T0 + 3 * 7 * 86_400 - 1, Ordering::SeqCst);
    assert!(talk(from_b.local_addr).await, "dernière seconde du créneau");

    drop((a, b, c));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn leases_are_judged_against_the_published_tariff_and_the_24_share_capacity() {
    let dir = std::env::temp_dir().join(format!("astroportd-lease-cap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("uplanet.db"), 2).unwrap();
    let req = |start, units| LeaseRequest { resource: Resource::Gpu1of24, start, units, unit_price: Amount(50) };

    // Sans tarif publié, un bail est « unpriced » (le nœud ne vend pas cette ressource).
    let early = purchase(&BOB, "9.00", &req(T0, 1), T0, "early");
    let early_id = early.id.clone();
    ingest(&db, early).await;
    assert_eq!(lease_status(&db, &early_id).await.as_deref(), Some("unpriced"));

    ingest(&db, tariff_event(T0)).await;
    // 24 baux d'une semaine sur le même créneau : tous actifs ; le 25e est « full » ; un créneau voisin reste libre.
    let mut ids = Vec::new();
    for i in 0..25u8 {
        let buyer = [100 + i; 32];
        let e = purchase(&buyer, "0.50", &req(T0 + 100, 1), T0 + 50, &format!("cap-{i}"));
        ids.push(e.id.clone());
        ingest(&db, e).await;
    }
    for id in &ids[..24] {
        assert_eq!(lease_status(&db, id).await.as_deref(), Some("active"));
    }
    assert_eq!(lease_status(&db, &ids[24]).await.as_deref(), Some("full"));
    let next_hour = purchase(&BOB, "0.50", &req(T0 + 100 + 7 * 86_400, 1), T0 + 60, "next");
    let next_id = next_hour.id.clone();
    ingest(&db, next_hour).await;
    assert_eq!(lease_status(&db, &next_id).await.as_deref(), Some("active"), "le créneau suivant est libre");

    // Un virement marqué lease mais illisible est tracé, pas ignoré.
    let mut bad = purchase(&BOB, "0.50", &req(T0 + 100, 1), T0 + 70, "bad");
    bad.tags.retain(|t| t[0] != "units");
    let bad = UnsignedEvent { created_at: bad.created_at, kind: bad.kind, tags: bad.tags, content: String::new() }.sign(&BOB).unwrap();
    let bad_id = bad.id.clone();
    ingest(&db, bad).await;
    assert_eq!(lease_status(&db, &bad_id).await.as_deref(), Some("invalid"));

    // Un tarif plus ancien arrivé après un plus récent ne l'écrase pas (replay indépendant de l'ordre).
    let mut cheap = Tariff::default();
    cheap.prices.insert(Resource::Gpu1of24, Amount(1));
    ingest(&db, UnsignedEvent { created_at: T0 - 500, kind: kind::ECONOMIC_HEALTH, tags: cheap.to_tags(), content: String::new() }.sign(&NODE_A).unwrap()).await;
    let price: i64 = db.readers().read(|c| Ok(c.query_row("SELECT price_cents FROM tariffs WHERE resource = 'gpu_1_24'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(price, 50);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_price_is_frozen_in_the_transaction_and_a_later_tariff_never_rewrites_a_lease() {
    let dir = std::env::temp_dir().join(format!("astroportd-lease-frozen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("uplanet.db"), 2).unwrap();
    ingest(&db, tariff_event(T0 - 100)).await; // 0,50 Ẑen / h
    let at_price = |p: u64, start: u64| LeaseRequest { resource: Resource::Gpu1of24, start, units: 3, unit_price: Amount(p) };
    let honest = purchase(&BOB, "1.50", &at_price(50, T0), T0, "honest"); // prix du tarif, payé en entier
    let cheat = purchase(&EVE, "0.30", &at_price(10, T0 + 1), T0 + 1, "cheat"); // prix figé sous le tarif en vigueur
    let short = purchase(&[6; 32], "1.00", &at_price(50, T0 + 2), T0 + 2, "short"); // prix correct, paiement insuffisant
    let ids = [honest.id.clone(), cheat.id.clone(), short.id.clone()];
    for e in [honest, cheat, short] {
        ingest(&db, e).await;
    }
    let frozen: i64 = db.readers().read({ let id = ids[0].clone(); move |c| Ok(c.query_row("SELECT unit_price_cents FROM resource_leases WHERE src_event = ?1", [id], |r| r.get(0))?) }).await.unwrap();
    assert_eq!(frozen, 50, "le prix figé est conservé avec le bail");
    let statuses = |db: &Db, ids: Vec<String>| {
        let db = db.readers().clone();
        async move {
            let mut out = Vec::new();
            for id in ids {
                out.push(db.read(move |c| Ok(c.query_row("SELECT status FROM resource_leases WHERE src_event = ?1", [id], |r| r.get::<_, String>(0)).unwrap_or_default())).await.unwrap());
            }
            out
        }
    };
    assert_eq!(statuses(&db, ids.to_vec()).await, ["active", "underpaid", "underpaid"]);

    // Le nœud double ses prix : le bail déjà payé reste valable, en direct comme après un replay complet.
    ingest(&db, tariff_event_at(T0 + 1000, 500)).await;
    assert_eq!(statuses(&db, ids.to_vec()).await[0], "active");
    let stats = astro_sync::replay_local(&db, 100).await.unwrap();
    assert_eq!(stats.skipped, 0, "{:?}", stats.errors);
    assert_eq!(statuses(&db, ids.to_vec()).await[0], "active", "après replay : jugé au prix figé (0,50), pas au tarif courant (5,00)");
    let (price_now, price_frozen): (i64, i64) = db
        .readers()
        .read({ let id = ids[0].clone(); move |c| Ok((c.query_row("SELECT price_cents FROM tariffs WHERE resource = 'gpu_1_24'", [], |r| r.get(0))?, c.query_row("SELECT unit_price_cents FROM resource_leases WHERE src_event = ?1", [id], |r| r.get(0))?)) })
        .await
        .unwrap();
    assert_eq!((price_now, price_frozen), (500, 50));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn tariffs_history_keeps_the_floor_of_the_day_even_after_the_node_lowers_its_prices() {
    let dir = std::env::temp_dir().join(format!("astroportd-lease-history-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("uplanet.db"), 2).unwrap();
    ingest(&db, tariff_event_at(T0 - 100, 50)).await;
    let cheat = purchase(&EVE, "0.30", &LeaseRequest { resource: Resource::Gpu1of24, start: T0, units: 3, unit_price: Amount(10) }, T0, "cheat");
    let id = cheat.id.clone();
    ingest(&db, cheat).await;
    // Le nœud baisse ensuite ses prix : le plancher du jour du virement reste celui de l'historique.
    ingest(&db, tariff_event_at(T0 + 1000, 5)).await;
    let status = |db: &Db, id: String| {
        let r = db.readers().clone();
        async move { r.read(move |c| Ok(c.query_row("SELECT status FROM resource_leases WHERE src_event = ?1", [id], |r| r.get::<_, String>(0))?)).await.unwrap() }
    };
    assert_eq!(status(&db, id.clone()).await, "underpaid");
    let stats = astro_sync::replay_local(&db, 100).await.unwrap();
    assert_eq!(stats.skipped, 0, "{:?}", stats.errors);
    assert_eq!(status(&db, id).await, "underpaid", "le replay juge au tarif en vigueur à la date du virement");
    let rows: i64 = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM tariffs_history WHERE resource = 'gpu_1_24'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(rows, 2, "l'historique garde les deux grilles");
    let _ = std::fs::remove_dir_all(dir);
}

fn tariff_event_at(at: u64, gpu_cents: u64) -> Event {
    let mut t = Tariff::default();
    t.prices.insert(Resource::Gpu1of24, Amount(gpu_cents));
    UnsignedEvent { created_at: at, kind: kind::ECONOMIC_HEALTH, tags: t.to_tags(), content: String::new() }.sign(&NODE_A).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_home_station_is_projected_from_the_did_so_leases_reach_the_hosting_peer() {
    let dir = std::env::temp_dir().join(format!("astroportd-home-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("uplanet.db"), 2).unwrap();
    let home = format!("12D3KooWHostingStation:{}", hex(&[2; 32]));
    let did = |secret: &[u8; 32], doc_extra: serde_json::Value, tags: Vec<Vec<String>>| {
        let hexkey = hex(secret);
        let mut doc = serde_json::json!({ "id": format!("did:nostr:{hexkey}"), "metadata": { "email": format!("{}@example.org", &hexkey[..6]), "contractStatus": "active_rental" } });
        doc.as_object_mut().unwrap().extend(doc_extra.as_object().unwrap().clone());
        let mut all = vec![vec!["d".to_owned(), "did".to_owned()]];
        all.extend(tags);
        UnsignedEvent { created_at: T0, kind: kind::DID, tags: all, content: doc.to_string() }.sign(secret).unwrap()
    };
    ingest(&db, did(&BOB, serde_json::json!({ "astroportStation": home }), vec![])).await; // champ du DID
    ingest(&db, did(&EVE, serde_json::json!({}), vec![vec!["home_station".into(), home.clone()]])).await; // tag
    ingest(&db, did(&[6; 32], serde_json::json!({ "astroportStation": "n'importe quoi" }), vec![])).await; // format refusé
    let read = |k: [u8; 32]| {
        let hexkey = hex(&k);
        let r = db.readers().clone();
        async move { r.read(move |c| Ok(c.query_row("SELECT home_station FROM identities WHERE hex = ?1", [hexkey], |r| r.get::<_, Option<String>>(0))?)).await.unwrap() }
    };
    assert_eq!(read(BOB).await.as_deref(), Some(home.as_str()));
    assert_eq!(read(EVE).await.as_deref(), Some(home.as_str()));
    assert_eq!(read([6; 32]).await, None, "un format invalide n'est pas projeté");
    // Le bail de Bob ouvre donc l'accès à la station qui l'héberge.
    ingest(&db, tariff_event(T0 - 100)).await;
    let req = LeaseRequest { resource: Resource::Gpu1of24, start: T0, units: 1, unit_price: Amount(50) };
    ingest(&db, purchase(&BOB, "0.50", &req, T0, "bob-lease")).await;
    let allowed = |peer: &'static str| {
        let r = db.readers().clone();
        async move { r.read(move |c| Ok(astro_economy::lease::has_active_lease(c, &hex(&NODE_A), Resource::Gpu1of24, T0 + 60, peer)?)).await.unwrap() }
    };
    assert!(allowed("12D3KooWHostingStation").await);
    assert!(!allowed("12D3KooWSomeoneElse1").await);
    let _ = std::fs::remove_dir_all(dir);
}

/// Publication de test : le virement du courtier est « reçu » par le Brain, qui le projette comme son relai.
struct ToBrain(Arc<Db>);

impl astro_bro::broker::Publisher for ToBrain {
    async fn publish(&self, e: Event) -> Result<(), String> {
        ingest(&self.0, e).await;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_starved_satellite_buys_its_own_gpu_week_and_the_brain_projects_an_active_lease() {
    use astro_bro::broker::{Broker, BrokerConfig, Decision};
    use astro_p2p::routing::Candidate;
    let dir = std::env::temp_dir().join(format!("astroportd-broker-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let satellite = Arc::new(Db::open(&dir.join("sat.db"), 2).unwrap());
    let brain = Arc::new(Db::open(&dir.join("brain.db"), 2).unwrap());
    let sat_key = [8u8; 32];
    // Le satellite connaît le Brain (clé + tarif) ; le Brain connaît le tarif et le satellite.
    ingest(&satellite, tariff_event(T0 - 100)).await;
    satellite.writer().execute("INSERT INTO identities(hex, role, peer_id, hosted) VALUES (?1, 'node', '12D3KooWbrain', 0)", vec![V::Text(hex(&NODE_A))]).await.unwrap();
    ingest(&brain, tariff_event(T0 - 100)).await;
    for _ in 0..4 {
        satellite.writer().execute("INSERT INTO tasks(kind, payload, status, created_at) VALUES ('comfyui.image', '{}', 'pending', 0)", vec![]).await.unwrap();
    }
    let candidates = [Candidate { node_id: "12D3KooWbrain".into(), power_score: 160.0, lat: Some(45.7), lon: Some(4.8), services: vec!["comfyui".into()] }];
    let broker = Broker::new(satellite.clone(), sat_key, false, BrokerConfig::default(), Arc::new(ToBrain(brain.clone())), None).unwrap();
    let Decision::Bought { event_id, cost_cents, .. } = broker.tick(T0, &candidates, Some((45.76, 4.83))).await.unwrap() else { unreachable!() };
    assert_eq!(cost_cents, 50);
    let status: String = brain.readers().read(move |c| Ok(c.query_row("SELECT status FROM resource_leases WHERE src_event = ?1", [event_id], |r| r.get(0))?)).await.unwrap();
    assert_eq!(status, "active", "le Brain valide le virement du courtier comme n'importe quel achat");
    // Et la station satellite a désormais accès au flux GPU (son NODE est le bénéficiaire, pair = son PeerID).
    brain.writer().execute("INSERT INTO identities(hex, role, peer_id, hosted) VALUES (?1, 'node', '12D3KooWsatellite', 0)", vec![V::Text(hex(&sat_key))]).await.unwrap();
    let granted = brain.readers().read(|c| Ok(astro_economy::lease::has_active_lease(c, &hex(&NODE_A), Resource::Gpu1of24, T0 + 30, "12D3KooWsatellite")?)).await.unwrap();
    assert!(granted);
    let _ = std::fs::remove_dir_all(dir);
}
