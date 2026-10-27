//! Filtrage NIP-42 de bout en bout, sur un vrai serveur WebSocket et une vraie base SQLite :
//! rejet des curieux, acceptation de N1/N2, exception d'amorçage Gift Wrap.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use astro_db::Db;
use astro_nostr_proto::{ClientMessage, Event, Filter, RelayMessage, UnsignedEvent, kind, pubkey_of};
use astro_relay::{DbCircle, Relay, RelayConfig, serve};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const ALICE: [u8; 32] = [0xA1; 32]; // MULTIPASS hébergé
const BOB: [u8; 32] = [0xB2; 32]; // N1 d'Alice
const CAROL: [u8; 32] = [0xC3; 32]; // N2 d'Alice (via Bob)
const MALLORY: [u8; 32] = [0xD4; 32]; // inconnue

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn hex_of(secret: &[u8; 32]) -> String {
    pubkey_of(secret).unwrap()
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    addr: SocketAddr,
    _dir: TempDir,
    db: Arc<Db>,
}

async fn start(config: RelayConfig) -> Harness {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("astro-relay-it-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 4).unwrap());
    // Alice est hébergée ; Bob est son N1 ; Carol son N2.
    let (alice, bob, carol) = (hex_of(&ALICE), hex_of(&BOB), hex_of(&CAROL));
    let w = db.writer();
    w.execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'multipass', 1)", vec![alice.clone().into()]).await.unwrap();
    for (pubkey, level, via) in [(&bob, 1, None), (&carol, 2, Some(&bob))] {
        w.execute(
            "INSERT INTO social_graph(owner_hex, pubkey, level, via, updated_at) VALUES (?1, ?2, ?3, ?4, 0)",
            vec![alice.clone().into(), pubkey.clone().into(), i64::from(level).into(), via.cloned().into()],
        )
        .await
        .unwrap();
    }
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await.unwrap();
    let relay = Relay::new(db.clone(), circle, config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, relay));
    Harness { addr, _dir: TempDir(dir), db }
}

struct Client {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    challenge: String,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let mut c = Client { ws, challenge: String::new() };
        match c.recv().await {
            RelayMessage::Auth(challenge) => c.challenge = challenge,
            other => panic!("le relai doit ouvrir par un défi AUTH, reçu {other:?}"),
        }
        c
    }

    async fn send(&mut self, m: ClientMessage) {
        self.ws.send(Message::text(m.to_json())).await.unwrap();
    }

    async fn recv(&mut self) -> RelayMessage {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), self.ws.next()).await.expect("délai dépassé").unwrap().unwrap();
            if let Message::Text(t) = msg {
                return RelayMessage::parse(t.as_str()).unwrap();
            }
        }
    }

    async fn auth_with(&mut self, secret: &[u8; 32], challenge: &str, created_at: u64) -> (bool, String) {
        let ev = UnsignedEvent {
            created_at,
            kind: kind::AUTH,
            tags: vec![vec!["relay".into(), "ws://relay.test".into()], vec!["challenge".into(), challenge.into()]],
            content: String::new(),
        }
        .sign(secret)
        .unwrap();
        self.send(ClientMessage::Auth(ev)).await;
        match self.recv().await {
            RelayMessage::Ok { accepted, message, .. } => (accepted, message),
            other => panic!("OK attendu, reçu {other:?}"),
        }
    }

    async fn auth(&mut self, secret: &[u8; 32]) {
        let challenge = self.challenge.clone();
        let (ok, msg) = self.auth_with(secret, &challenge, now()).await;
        assert!(ok, "AUTH refusé : {msg}");
    }

    async fn publish(&mut self, ev: &Event) -> (bool, String) {
        self.send(ClientMessage::Event(ev.clone())).await;
        match self.recv().await {
            RelayMessage::Ok { accepted, message, .. } => (accepted, message),
            other => panic!("OK attendu, reçu {other:?}"),
        }
    }

    /// `Ok(événements)` jusqu'à EOSE, ou `Err(message CLOSED)`.
    async fn req(&mut self, sub: &str, filter: Filter) -> Result<Vec<Event>, String> {
        self.send(ClientMessage::Req { sub_id: sub.into(), filters: vec![filter] }).await;
        let mut events = Vec::new();
        loop {
            match self.recv().await {
                RelayMessage::Event { event, .. } => events.push(*event),
                RelayMessage::Eose(_) => return Ok(events),
                RelayMessage::Closed { message, .. } => return Err(message),
                other => panic!("réponse inattendue : {other:?}"),
            }
        }
    }
}

fn event(secret: &[u8; 32], kind: u32, tags: Vec<Vec<String>>, content: &str) -> Event {
    UnsignedEvent { created_at: now(), kind, tags, content: content.into() }.sign(secret).unwrap()
}

fn p_tag(secret: &[u8; 32]) -> Vec<String> {
    vec!["p".into(), hex_of(secret)]
}

fn kinds(ks: &[u32]) -> Filter {
    Filter { kinds: Some(ks.to_vec()), ..Filter::default() }
}

#[tokio::test]
async fn every_connection_starts_with_an_auth_challenge() {
    let h = start(RelayConfig::default()).await;
    let a = Client::connect(h.addr).await;
    let b = Client::connect(h.addr).await;
    assert_eq!(a.challenge.len(), 32);
    assert_ne!(a.challenge, b.challenge, "un défi distinct par connexion");
}

#[tokio::test]
async fn curious_strangers_are_turned_away_from_private_kinds() {
    let h = start(RelayConfig::default()).await;
    let mut bob = Client::connect(h.addr).await;
    bob.auth(&BOB).await;
    assert!(bob.publish(&event(&BOB, kind::APP_DATA, vec![vec!["d".into(), "secret".into()]], "données privées")).await.0);

    // 1. Anonyme : CLOSED auth-required sur chaque kind privé.
    let mut anon = Client::connect(h.addr).await;
    for k in [kind::DM_LEGACY, kind::GIFT_WRAP, kind::APP_DATA] {
        let err = anon.req("s", kinds(&[k])).await.unwrap_err();
        assert!(err.starts_with("auth-required:"), "kind {k} : {err}");
    }
    // Même mélangé à un kind public : la souscription entière est refusée.
    assert!(anon.req("s", kinds(&[kind::TEXT_NOTE, kind::APP_DATA])).await.unwrap_err().starts_with("auth-required:"));
    // Sans `kinds` : servie, mais aucun événement privé ne sort.
    let all = anon.req("s", Filter::default()).await.unwrap();
    assert!(all.iter().all(|e| e.kind != kind::APP_DATA));

    // 2. Authentifié mais hors du cercle : restricted.
    let mut mallory = Client::connect(h.addr).await;
    mallory.auth(&MALLORY).await;
    let err = mallory.req("s", kinds(&[kind::APP_DATA])).await.unwrap_err();
    assert!(err.starts_with("restricted:"), "{err}");
    let all = mallory.req("t", Filter::default()).await.unwrap();
    assert!(all.iter().all(|e| e.kind != kind::APP_DATA), "pas de fuite par une souscription large");
}

#[tokio::test]
async fn circle_members_read_private_app_data_and_everyone_reads_public_kinds() {
    let h = start(RelayConfig::default()).await;
    let mut bob = Client::connect(h.addr).await;
    bob.auth(&BOB).await;
    assert!(bob.publish(&event(&BOB, kind::APP_DATA, vec![vec!["d".into(), "x".into()]], "privé")).await.0);
    assert!(bob.publish(&event(&BOB, kind::TEXT_NOTE, vec![], "public")).await.0);

    for member in [&ALICE, &BOB, &CAROL] {
        let mut c = Client::connect(h.addr).await;
        c.auth(member).await;
        let got = c.req("s", kinds(&[kind::APP_DATA])).await.unwrap();
        assert_eq!(got.len(), 1, "membre {:?}", &member[..1]);
    }
    let mut anon = Client::connect(h.addr).await;
    let notes = anon.req("s", kinds(&[kind::TEXT_NOTE])).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].content, "public");
}

#[tokio::test]
async fn writes_are_restricted_to_the_social_circle() {
    let h = start(RelayConfig::default()).await;
    for (who, allowed) in [(&ALICE, true), (&BOB, true), (&CAROL, true), (&MALLORY, false)] {
        let mut c = Client::connect(h.addr).await;
        c.auth(who).await;
        let (ok, msg) = c.publish(&event(who, kind::TEXT_NOTE, vec![], "salut")).await;
        assert_eq!(ok, allowed, "{msg}");
        if !allowed {
            assert!(msg.starts_with("restricted:"), "{msg}");
        }
    }
    // L'authentification ne suffit pas : Mallory, même authentifiée, reste dehors ; et sans AUTH aussi.
    let mut anon = Client::connect(h.addr).await;
    assert!(!anon.publish(&event(&MALLORY, kind::TEXT_NOTE, vec![], "x")).await.0);
    // Un membre n'a pas besoin d'AUTH pour publier : la signature de l'événement suffit.
    assert!(anon.publish(&event(&CAROL, kind::TEXT_NOTE, vec![], "carol")).await.0);
}

#[tokio::test]
async fn gift_wrap_bootstrap_exception_with_rate_limit() {
    let config = RelayConfig { gift_wrap_burst: 3, gift_wrap_per_minute: 1, ..RelayConfig::default() };
    let h = start(config).await;
    let mut stranger = Client::connect(h.addr).await;

    // Un inconnu (clé éphémère) peut écrire un gift wrap vers le MULTIPASS hébergé…
    for i in 0..3 {
        let (ok, msg) = stranger.publish(&event(&MALLORY, kind::GIFT_WRAP, vec![p_tag(&ALICE)], &format!("hello {i}"))).await;
        assert!(ok, "gift wrap {i} : {msg}");
    }
    // …dans la limite du débit par IP.
    let (ok, msg) = stranger.publish(&event(&MALLORY, kind::GIFT_WRAP, vec![p_tag(&ALICE)], "trop")).await;
    assert!(!ok && msg.starts_with("rate-limited:"), "{msg}");

    // Mais pas vers un destinataire non hébergé ici, ni vers un simple N1.
    for target in [&MALLORY, &BOB] {
        let (ok, msg) = stranger.publish(&event(&MALLORY, kind::GIFT_WRAP, vec![p_tag(target)], "x")).await;
        assert!(!ok && msg.starts_with("restricted:"), "{msg}");
    }
    // Et pas d'autre kind : l'exception ne vaut que pour le kind 1059.
    let (ok, _) = stranger.publish(&event(&MALLORY, kind::TEXT_NOTE, vec![], "x")).await;
    assert!(!ok);
}

#[tokio::test]
async fn gift_wraps_are_readable_only_by_their_recipient() {
    let h = start(RelayConfig::default()).await;
    let mut stranger = Client::connect(h.addr).await;
    assert!(stranger.publish(&event(&MALLORY, kind::GIFT_WRAP, vec![p_tag(&ALICE)], "pour Alice")).await.0);

    let mut alice = Client::connect(h.addr).await;
    alice.auth(&ALICE).await;
    assert_eq!(alice.req("s", kinds(&[kind::GIFT_WRAP])).await.unwrap().len(), 1);

    // Bob est dans le cercle, mais n'est pas le destinataire.
    let mut bob = Client::connect(h.addr).await;
    bob.auth(&BOB).await;
    assert!(bob.req("s", kinds(&[kind::GIFT_WRAP])).await.unwrap().is_empty());
}

#[tokio::test]
async fn auth_rejects_wrong_challenge_stale_clock_wrong_kind_and_bad_signature() {
    let h = start(RelayConfig { relay_url: Some("wss://relay.test".into()), ..RelayConfig::default() }).await;
    let mut c = Client::connect(h.addr).await;

    let (ok, msg) = c.auth_with(&BOB, "mauvais-defi", now()).await;
    assert!(!ok && msg.contains("challenge"), "{msg}");

    let challenge = c.challenge.clone();
    let (ok, msg) = c.auth_with(&BOB, &challenge, now() - 3600).await;
    assert!(!ok && msg.contains("created_at"), "{msg}");

    // Mauvais kind.
    let wrong_kind = event(&BOB, kind::TEXT_NOTE, vec![vec!["challenge".into(), challenge.clone()]], "");
    c.send(ClientMessage::Auth(wrong_kind)).await;
    assert!(matches!(c.recv().await, RelayMessage::Ok { accepted: false, .. }));

    // Signature falsifiée.
    let mut forged = UnsignedEvent {
        created_at: now(),
        kind: kind::AUTH,
        tags: vec![vec!["relay".into(), "wss://relay.test".into()], vec!["challenge".into(), challenge.clone()]],
        content: String::new(),
    }
    .sign(&BOB)
    .unwrap();
    forged.sig = "00".repeat(64);
    c.send(ClientMessage::Auth(forged)).await;
    assert!(matches!(c.recv().await, RelayMessage::Ok { accepted: false, .. }));

    // Seul l'hôte du tag `relay` compte : `ws://relay.test` convient pour `wss://relay.test`.
    let (ok, msg) = c.auth_with(&BOB, &challenge, now()).await;
    assert!(ok, "{msg}");
}

#[tokio::test]
async fn auth_relay_tag_must_match_the_configured_host() {
    let h = start(RelayConfig { relay_url: Some("wss://autre.example".into()), ..RelayConfig::default() }).await;
    let mut c = Client::connect(h.addr).await;
    let challenge = c.challenge.clone();
    let (ok, msg) = c.auth_with(&BOB, &challenge, now()).await;
    assert!(!ok && msg.contains("relay"), "{msg}");
    // Et sans authentification valide, les kinds privés restent fermés.
    assert!(c.req("s", kinds(&[kind::APP_DATA])).await.unwrap_err().starts_with("auth-required:"));
}

#[tokio::test]
async fn invalid_events_are_refused_and_replaceable_events_replace() {
    let h = start(RelayConfig::default()).await;
    let mut bob = Client::connect(h.addr).await;
    let mut forged = event(&BOB, kind::TEXT_NOTE, vec![], "x");
    forged.content = "altéré".into();
    let (ok, msg) = bob.publish(&forged).await;
    assert!(!ok && msg.starts_with("invalid:"), "{msg}");

    let future = UnsignedEvent { created_at: now() + 100_000, kind: 1, tags: vec![], content: "futur".into() }.sign(&BOB).unwrap();
    let (ok, msg) = bob.publish(&future).await;
    assert!(!ok && msg.contains("future"), "{msg}");

    let ev = event(&BOB, kind::TEXT_NOTE, vec![], "une fois");
    assert!(bob.publish(&ev).await.0);
    let (ok, msg) = bob.publish(&ev).await;
    assert!(ok && msg.starts_with("duplicate:"), "{msg}");

    let old = UnsignedEvent { created_at: now() - 100, kind: 0, tags: vec![], content: "{\"name\":\"ancien\"}".into() }.sign(&BOB).unwrap();
    let new = UnsignedEvent { created_at: now(), kind: 0, tags: vec![], content: "{\"name\":\"nouveau\"}".into() }.sign(&BOB).unwrap();
    assert!(bob.publish(&new).await.0);
    assert!(!bob.publish(&old).await.0, "une version plus ancienne ne remplace pas");
    let mut anon = Client::connect(h.addr).await;
    let profiles = anon.req("s", Filter { kinds: Some(vec![0]), authors: Some(vec![hex_of(&BOB)]), ..Filter::default() }).await.unwrap();
    assert_eq!(profiles.len(), 1);
    assert!(profiles[0].content.contains("nouveau"));
}

#[tokio::test]
async fn live_events_reach_subscribers_through_the_same_filter() {
    let h = start(RelayConfig::default()).await;
    let mut listener = Client::connect(h.addr).await;
    listener.auth(&CAROL).await;
    assert!(listener.req("live-notes", kinds(&[kind::TEXT_NOTE])).await.unwrap().is_empty());
    assert!(listener.req("live-data", kinds(&[kind::APP_DATA])).await.unwrap().is_empty());

    let mut bob = Client::connect(h.addr).await;
    assert!(bob.publish(&event(&BOB, kind::TEXT_NOTE, vec![], "en direct")).await.0);
    assert!(bob.publish(&event(&BOB, kind::APP_DATA, vec![vec!["d".into(), "y".into()]], "privé en direct")).await.0);

    let mut received = Vec::new();
    for _ in 0..2 {
        match listener.recv().await {
            RelayMessage::Event { sub_id, event } => received.push((sub_id, event.content)),
            other => panic!("EVENT attendu, reçu {other:?}"),
        }
    }
    received.sort();
    assert_eq!(received, [("live-data".to_string(), "privé en direct".to_string()), ("live-notes".to_string(), "en direct".to_string())]);

    // Un curieux abonné à tout ne reçoit que le public.
    let mut anon = Client::connect(h.addr).await;
    assert!(anon.req("all", Filter::default()).await.unwrap().iter().all(|e| e.kind != kind::APP_DATA));
    assert!(bob.publish(&event(&BOB, kind::APP_DATA, vec![vec!["d".into(), "z".into()]], "secret 2")).await.0);
    assert!(bob.publish(&event(&BOB, kind::TEXT_NOTE, vec![], "public 2")).await.0);
    match anon.recv().await {
        RelayMessage::Event { event, .. } => assert_eq!(event.content, "public 2", "le privé n'est pas diffusé aux anonymes"),
        other => panic!("EVENT attendu, reçu {other:?}"),
    }
    drop(h.db);
}

#[tokio::test]
async fn subscription_limits_and_malformed_input() {
    let config = RelayConfig { max_subscriptions: 2, max_filters: 2, ..RelayConfig::default() };
    let h = start(config).await;
    let mut c = Client::connect(h.addr).await;
    assert!(c.req("a", kinds(&[1])).await.is_ok());
    assert!(c.req("b", kinds(&[1])).await.is_ok());
    assert!(c.req("c", kinds(&[1])).await.unwrap_err().contains("too many subscriptions"));
    assert!(c.req("a", kinds(&[1])).await.is_ok(), "remplacer une souscription existante reste permis");
    c.send(ClientMessage::Close("a".into())).await;
    assert!(c.req("c", kinds(&[1])).await.is_ok(), "CLOSE libère une place");
    c.send(ClientMessage::Req { sub_id: "x".into(), filters: vec![Filter::default(); 3] }).await;
    assert!(matches!(c.recv().await, RelayMessage::Closed { message, .. } if message.contains("too many filters")));
    c.ws.send(Message::text("pas du json")).await.unwrap();
    assert!(matches!(c.recv().await, RelayMessage::Notice(m) if m.starts_with("invalid:")));
}

#[tokio::test]
async fn nip11_document_is_served_over_http() {
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    let dir = std::env::temp_dir().join(format!("astro-relay-nip11-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = TempDir(dir.clone());
    let db = Arc::new(Db::open(&dir.join("u.db"), 1).unwrap());
    let relay = Relay::new(db, Arc::new(DbCircle::new()), RelayConfig::default());
    let res = relay
        .router()
        .oneshot(Request::builder().uri("/").header(header::ACCEPT, "application/nostr+json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.headers()[header::CONTENT_TYPE], "application/nostr+json");
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(doc["name"], "astroportd");
    assert!(doc["supported_nips"].as_array().unwrap().contains(&serde_json::json!(42)));
}
