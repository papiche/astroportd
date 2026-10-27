//! Interface web de bout en bout, sur une vraie base et un vrai WebDAV : négociation de `/`, API authentifiée,
//! album et pagination, partage par lien magique, visages, flux SSE, console de BRO (avec un faux Ollama HTTP).

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use astro_bro::{MemoryStore, OllamaClient};
use astro_core::ActivityBus;
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::auth::derive_dav_token;
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher};
use astro_db::{Db, SqliteReplayStore};
use astro_p2p::routing::Hardware;
use astro_relay::{DbCircle, Relay, RelayConfig};
use astroportd::station::{StationDeps, StationInfo, StationServices};
use astroportd::ui_backend::StationUi;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use base64::Engine;
use http_body_util::BodyExt;
use rusqlite::types::Value as V;
use serde_json::{Value, json};
use tower::ServiceExt;

const SECRET: &[u8] = b"secret-de-station-pour-les-tests";
const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB2; 32];

fn percent(seg: &str) -> String {
    seg.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn hex(s: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(s).unwrap()
}

fn basic(email: &str, secret: &[u8; 32]) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{email}:{}", derive_dav_token(SECRET, &hex(secret)))))
}

struct Bench {
    app: Router,
    db: Arc<Db>,
    bus: ActivityBus,
    dir: std::path::PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn bench(tag: &str, ollama: Option<&str>) -> Bench {
    bench_with(tag, ollama, |_| {}, None).await
}

async fn bench_with(tag: &str, ollama: Option<&str>, tune: impl FnOnce(&mut DavConfig), swarm_dir: Option<std::path::PathBuf>) -> Bench {
    let dir = std::env::temp_dir().join(format!("astroportd-ui-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    for (secret, role, email) in [(&ALICE, "multipass", "alice@example.org"), (&BOB, "captain", "bob@example.org")] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, ?2, ?3, 1)", vec![V::Text(hex(secret)), V::Text(role.into()), V::Text(email.into())]).await.unwrap();
    }
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BOB));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys, Duration::from_secs(3600));
    let mut config = DavConfig::new(SECRET.to_vec());
    config.lease_node = Some(hex(&[1; 32]));
    tune(&mut config);
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config, on_upload: None });
    let bus = ActivityBus::new();
    let info = StationInfo { node_hex: hex(&[1; 32]), peer_id: Some("12D3KooWTest".into()), domain: Some("station.test".into()), started: Instant::now() };
    let services = Arc::new(StationServices::new(StationDeps { db: db.clone(), info, nostr_dir: None, chain: None, hardware: Hardware { vram_gb: 24.0, cpu_cores: 16, ram_gb: 64.0 }, p2p: None, weekly_rent_cents: 100, activity: bus.clone(), swarm_dir, origin: Some((45.76, 4.83)), exposed: vec!["comfyui".into(), "ssh".into()] }));
    let client = ollama.map(|url| Arc::new(OllamaClient::new(url, "bge-m3", "llama3.2", Duration::from_secs(5))));
    let memory = client.as_ref().map(|o| Arc::new(MemoryStore::new(db.clone(), o.clone(), o.clone(), Some(bus.clone()))));
    let registrar = Arc::new(astroportd::register::Registrar::new(db.clone(), dir.join("nostr"), "0".repeat(64), Arc::new(|hex| derive_dav_token(SECRET, hex)), astroportd::register::Limits::default()));
    let ui = Arc::new(StationUi::new(services, dav.clone(), memory, client, true).with_registrar(registrar));
    let relay = Relay::new(db.clone(), Arc::new(DbCircle::new()), RelayConfig::default());
    let app = astro_dav::router(dav)
        .merge(relay.router())
        .merge(astro_ui::web::static_router())
        .merge(astro_ui::api::router(ui, bus.clone(), Some("https://station.test".into())))
        .layer(axum::middleware::from_fn(astro_ui::web::negotiate));
    Bench { app, db, bus, dir }
}

async fn call(app: &Router, method: Method, uri: &str, auth: Option<&str>, headers: &[(&str, &str)], body: Vec<u8>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri).header(header::HOST, "station.test");
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    (status, headers, resp.into_body().collect().await.unwrap().to_bytes().to_vec())
}

async fn api(app: &Router, method: Method, uri: &str, auth: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (headers, bytes): (&[(&str, &str)], Vec<u8>) = match body {
        Some(v) => (&[("content-type", "application/json")], v.to_string().into_bytes()),
        None => (&[], Vec::new()),
    };
    let (s, _, b) = call(app, method, uri, Some(auth), headers, bytes).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn put(app: &Router, auth: &str, path: &str, data: &[u8]) {
    // Les dossiers parents sont créés (MKCOL), comme le ferait un client WebDAV.
    let mut dir = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()).collect::<Vec<_>>().split_last().map_or(&[][..], |(_, init)| init) {
        dir = format!("{dir}/{part}");
        let url: String = format!("/dav{dir}").split('/').map(percent).collect::<Vec<_>>().join("/");
        let (s, ..) = call(app, Method::from_bytes(b"MKCOL").unwrap(), &url, Some(auth), &[], vec![]).await;
        assert!(s.is_success() || s == StatusCode::METHOD_NOT_ALLOWED, "MKCOL {dir} : {s}");
    }
    let url: String = format!("/dav{path}").split('/').map(percent).collect::<Vec<_>>().join("/");
    let (s, ..) = call(app, Method::PUT, &url, Some(auth), &[], data.to_vec()).await;
    assert!(s.is_success(), "PUT {path} : {s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn root_serves_the_app_to_browsers_and_nip11_to_nostr_clients() {
    let b = bench("root", None).await;
    let (s, h, body) = call(&b.app, Method::GET, "/", None, &[("accept", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8")], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("<title>Astroport</title>"));
    assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("default-src 'none'"));
    let (s, _, body) = call(&b.app, Method::GET, "/", None, &[("accept", "application/nostr+json")], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("\"software\":\"astroportd\""), "NIP-11 conservé : {}", String::from_utf8_lossy(&body));
    let (s, h, js) = call(&b.app, Method::GET, "/ui/app.js", None, &[], vec![]).await;
    assert_eq!((s, h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "text/javascript; charset=utf-8"));
    assert!(js.len() < 40 * 1024);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_api_requires_a_hosted_account_and_isolates_owners() {
    let b = bench("auth", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    assert_eq!(api(&b.app, Method::GET, "/api/v1/me", "Basic bm9wZTpub3Bl", None).await.0, StatusCode::UNAUTHORIZED);
    let (s, me) = api(&b.app, Method::GET, "/api/v1/me", &alice, None).await;
    assert_eq!((s, me["email"].as_str(), me["captain"].as_bool()), (StatusCode::OK, Some("alice@example.org"), Some(false)));
    assert_eq!(api(&b.app, Method::GET, "/api/v1/me", &bob, None).await.1["captain"], true);

    put(&b.app, &alice, "/Photos/vacances.jpg", b"\xff\xd8\xff pas vraiment un jpeg").await;
    put(&b.app, &alice, "/Documents/contrat.pdf", b"%PDF-1.4 secret").await;
    let (s, list) = api(&b.app, Method::GET, "/api/v1/ucloud/list?path=/", &alice, None).await;
    assert_eq!(s, StatusCode::OK);
    let names: Vec<&str> = list["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Documents", "Photos"], "dossiers triés");
    assert_eq!(list["dav"], "/dav");
    let (_, photos) = api(&b.app, Method::GET, "/api/v1/ucloud/list?path=/Photos", &alice, None).await;
    assert_eq!((photos["entries"][0]["name"].as_str(), photos["entries"][0]["mime"].as_str()), (Some("vacances.jpg"), Some("image/jpeg")));
    // Bob ne voit rien d'Alice ; chemin piégé et dossier absent refusés proprement.
    assert_eq!(api(&b.app, Method::GET, "/api/v1/ucloud/list?path=/Photos", &bob, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(api(&b.app, Method::GET, "/api/v1/ucloud/list?path=/../etc", &alice, None).await.0, StatusCode::BAD_REQUEST);
    // Le fichier est déchiffré par la station au téléchargement.
    let (s, _, bytes) = call(&b.app, Method::GET, "/dav/Photos/vacances.jpg", Some(&alice), &[], vec![]).await;
    assert_eq!((s, bytes.as_slice()), (StatusCode::OK, &b"\xff\xd8\xff pas vraiment un jpeg"[..]));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dashboard_reports_the_station_the_relay_and_the_sovereignty_widget() {
    let b = bench("dash", None).await;
    let alice = basic("alice@example.org", &ALICE);
    put(&b.app, &alice, "/a.txt", b"bonjour").await;
    put(&b.app, &alice, "/b.txt", b"le monde").await;
    let (s, d) = api(&b.app, Method::GET, "/api/v1/dashboard", &alice, None).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!((d["station"]["domain"].as_str(), d["station"]["peer_id"].as_str()), (Some("station.test"), Some("12D3KooWTest")));
    assert_eq!(d["p2p"]["enabled"], false);
    assert_eq!((d["p2p"]["score"].as_f64(), d["p2p"]["tier"].as_str()), (Some(160.0), Some("Brain")));
    assert_eq!(d["sovereignty"]["encrypted_files"], 2);
    assert_eq!(d["sovereignty"]["encrypted_bytes"], 7 + 8);
    assert!(d["balance"]["error"].as_str().unwrap().contains("Duniter"), "solde indisponible dit clairement : {d}");
    assert!(d["energy"]["weeks"].is_null() && d["energy"]["max_weeks"] == 12.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_album_paginates_without_losing_photos_that_share_a_timestamp() {
    let b = bench("album", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    // 4 photos de la même seconde, 3 plus anciennes, 1 vidéo, 1 document (exclu), 1 sans contenu (exclue).
    let rows: Vec<(String, &str, i64, Option<&str>)> = [("t1", "image/jpeg", 5000), ("t2", "image/jpeg", 5000), ("t3", "image/png", 5000), ("t4", "video/mp4", 5000), ("o1", "image/jpeg", 4000), ("o2", "image/jpeg", 3000), ("o3", "image/jpeg", 2000), ("doc", "application/pdf", 6000)]
        .iter()
        .map(|(n, m, t)| (format!("/Album/{n}"), *m, *t, Some("QmCid")))
        .chain([("/Album/vide".to_owned(), "image/jpeg", 7000, None)])
        .collect();
    for (path, mime, t, cid) in rows {
        db_insert(&b.db, &hex(&ALICE), &path, mime, t, cid).await;
    }
    let mut seen = Vec::new();
    let mut before: Option<i64> = None;
    for _ in 0..10 {
        let q = before.map_or("limit=2".to_owned(), |t| format!("limit=2&before={t}"));
        let (s, page) = api(&b.app, Method::GET, &format!("/api/v1/ucloud/photos?{q}"), &alice, None).await;
        assert_eq!(s, StatusCode::OK);
        seen.extend(page["items"].as_array().unwrap().iter().map(|i| i["path"].as_str().unwrap().to_owned()));
        match page["next"].as_i64() {
            Some(t) => before = Some(t),
            None => break,
        }
    }
    assert_eq!(seen, ["/Album/t1", "/Album/t2", "/Album/t3", "/Album/t4", "/Album/o1", "/Album/o2", "/Album/o3"], "ordre antéchronologique, ex æquo complets, rien en double");
    assert!(api(&b.app, Method::GET, "/api/v1/ucloud/photos", &bob, None).await.1["items"].as_array().unwrap().is_empty());
}

async fn db_insert(db: &Db, owner: &str, path: &str, mime: &str, t: i64, cid: Option<&str>) {
    let parent = path.rsplit_once('/').map_or("/", |(p, _)| p).to_owned();
    db.writer()
        .execute(
            "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, ?3, 'file', ?4, ?5, 10, ?6, '{}')",
            vec![V::Text(owner.into()), V::Text(path.into()), V::Text(parent), cid.map_or(V::Null, |c| V::Text(c.into())), V::Text(mime.into()), V::Integer(t)],
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_magic_link_is_created_with_its_key_in_the_fragment_and_a_qr_code() {
    let b = bench("share", None).await;
    let alice = basic("alice@example.org", &ALICE);
    put(&b.app, &alice, "/Fêtes/anniversaire-de-lea.txt", b"gateau au chocolat").await;
    let (s, v) = api(&b.app, Method::POST, "/api/v1/ucloud/share", &alice, Some(json!({ "paths": ["/Fêtes"], "ttl_hours": 24 }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let url = v["url"].as_str().unwrap();
    let (base, key) = url.split_once('#').unwrap();
    assert!(base.starts_with("https://station.test/s/") && key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()), "{url}");
    assert_eq!(v["items"], 1);
    assert!(v["qr_svg"].as_str().unwrap().contains("<svg"));
    // La page invitée et le manifeste chiffré répondent, sans authentification, sans rien révéler en clair.
    let slug_path = base.strip_prefix("https://station.test").unwrap();
    assert_eq!(call(&b.app, Method::GET, slug_path, None, &[], vec![]).await.0, StatusCode::OK);
    let (s, _, manifest) = call(&b.app, Method::GET, &format!("{slug_path}/m"), None, &[], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    let text = String::from_utf8_lossy(&manifest);
    assert!(!text.contains("anniversaire") && !text.contains("chocolat"), "le manifeste du lien est chiffré");
    // Un dossier inexistant n'ouvre aucun lien.
    assert_eq!(api(&b.app, Method::POST, "/api/v1/ucloud/share", &alice, Some(json!({ "paths": ["/Absent"] }))).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn faces_group_by_person_and_the_grey_zone_is_validated_in_one_click() {
    let b = bench("faces", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    let owner = hex(&ALICE);
    let face = |id: i64, name: &str, cid: &str, det: f64, sugg: Option<(&str, f64)>| {
        let (db, owner, name, cid) = (b.db.clone(), owner.clone(), name.to_owned(), cid.to_owned());
        let sugg = sugg.map(|(n, s)| (n.to_owned(), s));
        async move {
            db.writer()
                .execute(
                    "INSERT INTO faces(face_id, owner_hex, cid, source_path, name, det_score, bbox_x1, bbox_y1, bbox_x2, bbox_y2, embed_hash, ts, ucloud_path, suggest_name, suggest_score) \
                     VALUES (?1, ?2, ?3, '/p', ?4, ?5, 10, 20, 110, 140, ?6, 0, ?7, ?8, ?9)",
                    vec![V::Integer(id), V::Text(owner), V::Text(cid.clone()), V::Text(name), V::Real(det), V::Text(format!("h{id}")), V::Text(format!("/Photos/{cid}.jpg")), sugg.as_ref().map_or(V::Null, |s| V::Text(s.0.clone())), sugg.as_ref().map_or(V::Null, |s| V::Real(s.1))],
                )
                .await
                .unwrap();
        }
    };
    face(1, "Léa", "c1", 0.80, None).await;
    face(2, "Léa", "c2", 0.99, None).await;
    face(3, "Léa", "c3", 0.70, None).await;
    face(4, "Inconnu_aaaa1111", "c4", 0.9, None).await;
    face(5, "Inconnu_bbbb2222", "c5", 0.9, None).await;
    face(6, "Inconnu_cccc3333", "c6", 0.9, Some(("Paul", 0.71))).await;

    let (s, f) = api(&b.app, Method::GET, "/api/v1/faces", &alice, None).await;
    assert_eq!(s, StatusCode::OK, "{f}");
    assert_eq!((f["people"].as_array().unwrap().len(), f["people"][0]["name"].as_str(), f["people"][0]["count"].as_i64()), (1, Some("Léa"), Some(3)));
    assert_eq!(f["people"][0]["sample"]["face_id"], 2, "la détection la plus sûre représente la personne");
    assert_eq!(f["people"][0]["sample"]["bbox"], json!([10.0, 20.0, 110.0, 140.0]));
    assert_eq!(f["unknown"], 2, "les visages avec suggestion ne sont pas comptés comme simples inconnus");
    assert_eq!((f["suggestions"][0]["suggest_name"].as_str(), f["suggestions"][0]["face_id"].as_i64()), (Some("Paul"), Some(6)));

    // Un autre compte ne peut ni valider ni nommer.
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/6/accept", &bob, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/4/name", &bob, Some(json!({ "name": "Intrus" }))).await.0, StatusCode::NOT_FOUND);
    // Un clic : « Oui, c'est Paul ».
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/6/accept", &alice, None).await.1["accepted"], true);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/6/accept", &alice, None).await.0, StatusCode::NOT_FOUND, "déjà validée");
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/4/name", &alice, Some(json!({ "name": "Maman" }))).await.0, StatusCode::OK);
    let (_, f) = api(&b.app, Method::GET, "/api/v1/faces", &alice, None).await;
    let names: Vec<&str> = f["people"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Léa", "Maman", "Paul"]);
    assert!(f["suggestions"].as_array().unwrap().is_empty() && f["unknown"] == 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_activity_stream_carries_station_events_live() {
    let b = bench("sse", None).await;
    let alice = basic("alice@example.org", &ALICE);
    b.bus.publish("system", "test", "avant la connexion", None);
    let resp = b.app.clone().oneshot(Request::builder().uri("/api/v1/activity").header(header::AUTHORIZATION, &alice).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut body = resp.into_body();
    let first = String::from_utf8(body.frame().await.unwrap().unwrap().into_data().unwrap().to_vec()).unwrap();
    assert!(first.contains("avant la connexion"), "{first}");
    // Une action de l'utilisateur (commande dans la console) apparaît en direct.
    let (_, r) = api(&b.app, Method::POST, "/api/v1/bro/say", &alice, Some(json!({ "text": "#statut" }))).await;
    assert!(r["reply"].as_str().unwrap().contains("Power-Score 160.0 (Brain)"), "{r}");
    b.bus.publish("economy", "test", "virement constaté", Some(json!({ "cents": 100 })));
    let live = String::from_utf8(body.frame().await.unwrap().unwrap().into_data().unwrap().to_vec()).unwrap();
    assert!(live.contains("event: economy") && live.contains("virement constaté") && live.contains("\"cents\":100"), "{live}");
}

#[tokio::test(flavor = "multi_thread")]
async fn health_lists_the_subsystems_without_authentication() {
    let b = bench("health", None).await;
    let (s, _, body) = call(&b.app, Method::GET, "/health", None, &[], vec![]).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "ok");
    for (name, status) in [("db", "ok"), ("relay", "ok"), ("p2p", "disabled"), ("duniter", "disabled"), ("bro", "disabled"), ("facecloud", "ok")] {
        assert_eq!(v["subsystems"][name]["status"], status, "{name} : {v}");
    }
    assert_eq!(v["subsystems"]["db"]["detail"], "SQLite WAL");
    assert!(v.to_string().find("alice").is_none(), "aucune donnée personnelle sur un point public");
}

/// Faux Ollama : embeddings par hachage de mots (déterministes), résumé fixe, liste de modèles.
async fn fake_ollama() -> String {
    use astro_bro::{Embedder, HashingEmbedder};
    use axum::routing::{get, post};
    let app = Router::new()
        .route("/api/tags", get(|| async { axum::Json(json!({ "models": [] })) }))
        .route(
            "/api/embed",
            post(|axum::Json(v): axum::Json<Value>| async move {
                let v = HashingEmbedder.embed(v["input"].as_str().unwrap_or("")).await.unwrap();
                axum::Json(json!({ "embeddings": [v] }))
            }),
        )
        .route("/api/generate", post(|| async { axum::Json(json!({ "response": "résumé de test" })) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bro_console_remembers_recalls_and_refuses_secrets_through_ollama() {
    let url = fake_ollama().await;
    let b = bench("bro", Some(&url)).await;
    let alice = basic("alice@example.org", &ALICE);
    let say = |t: &str| api(&b.app, Method::POST, "/api/v1/bro/say", &alice, Some(json!({ "text": t })));
    let (s, r) = say("J'aime le pain au levain du dimanche").await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["reply"], "📝 Noté dans votre journal.");
    let (_, r) = say("Quel pain au levain ?").await;
    let reply = r["reply"].as_str().unwrap();
    assert!(reply.contains("Je me souviens que") && reply.contains("J'aime le pain au levain du dimanche"), "{reply}");
    let (_, r) = say("mon mot de passe est hunter2024").await;
    assert!(r["reply"].as_str().unwrap().starts_with("🔒"), "{r}");
    let (_, r) = say("J'aime le pain au levain du dimanche").await;
    assert_eq!(r["reply"].as_str().unwrap().lines().next(), Some("📝 Je le savais déjà."));
    // La mémoire est celle d'Alice seulement.
    let bob = basic("bob@example.org", &BOB);
    let (_, r) = api(&b.app, Method::POST, "/api/v1/bro/say", &bob, Some(json!({ "text": "Parle-moi de pain au levain" }))).await;
    assert!(!r["reply"].as_str().unwrap().contains("dimanche"), "{r}");
    // Ollama répond : BRO est en bonne santé.
    let (_, _, body) = call(&b.app, Method::GET, "/health", None, &[], vec![]).await;
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["subsystems"]["bro"]["status"], "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_ollama_free_text_is_refused_but_commands_still_work() {
    let b = bench("bro-off", None).await;
    let alice = basic("alice@example.org", &ALICE);
    let (s, r) = api(&b.app, Method::POST, "/api/v1/bro/say", &alice, Some(json!({ "text": "bonjour BRO" }))).await;
    assert_eq!(s, StatusCode::NOT_IMPLEMENTED);
    assert!(r["error"].as_str().unwrap().contains("--ollama"));
    let (_, r) = api(&b.app, Method::POST, "/api/v1/bro/say", &alice, Some(json!({ "text": "#solde" }))).await;
    assert!(r["reply"].as_str().unwrap().starts_with("⚠️ nœud Duniter non configuré"), "{r}");
    let (_, r) = api(&b.app, Method::POST, "/api/v1/bro/say", &alice, Some(json!({ "text": "#upload bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku contrat.pdf" }))).await;
    assert!(r["reply"].as_str().unwrap().starts_with("Document « contrat.pdf » mis en file"), "{r}");
    let (_, r) = api(&b.app, Method::POST, "/api/v1/bro/say", &alice, Some(json!({ "text": "#upload bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku contrat.pdf" }))).await;
    assert!(r["reply"].as_str().unwrap().starts_with("Déjà en file"), "{r}");
    let (_, r) = api(&b.app, Method::POST, "/api/v1/bro/say", &alice, Some(json!({ "text": "#upload pas-un-cid" }))).await;
    assert!(r["reply"].as_str().unwrap().starts_with("⚠️ source invalide"), "{r}");
}

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 251) as u8, (y % 241) as u8, 90]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

/// Virement ledger `alice → node` portant un bail, projeté comme le ferait le relai.
async fn buy(db: &Db, buyer: &[u8; 32], amount: &str, resource: &str, units: u32, at: u64, d: &str) {
    use astro_nostr_proto::{UnsignedEvent, kind};
    let tags = vec![
        vec!["d".into(), d.into()], vec!["p".into(), hex(&[1; 32])], vec!["amount".into(), amount.into()], vec!["prev".into(), "genesis".into()],
        vec!["t".into(), "g1-n2".into()], vec!["t".into(), "lease".into()], vec!["resource".into(), resource.into()], vec!["units".into(), units.to_string()],
        vec!["unit_price".into(), if resource == "gpu_1_24" { "0.50" } else { "4.00" }.into()],
    ];
    let e = UnsignedEvent { created_at: at, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(buyer).unwrap();
    db.writer()
        .transaction(move |tx| {
            astro_db::events::insert_event(tx, &e)?;
            astro_db::apply_statements(tx, &astro_sync::Projector::plan(&e).unwrap())?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn publish_tariffs(db: &Db) {
    use astro_core::Amount;
    use astro_economy::lease::{Resource, Tariff};
    let mut t = Tariff::default();
    t.prices.insert(Resource::Gpu1of24, Amount(50));
    t.prices.insert(Resource::Disk128, Amount(400));
    let e = astro_nostr_proto::UnsignedEvent { created_at: 1_790_000_000, kind: astro_nostr_proto::kind::ECONOMIC_HEALTH, tags: t.to_tags(), content: String::new() }.sign(&[1; 32]).unwrap();
    db.writer()
        .transaction(move |tx| {
            astro_db::events::insert_event(tx, &e)?;
            astro_db::apply_statements(tx, &astro_sync::Projector::plan(&e).unwrap())?;
            Ok(())
        })
        .await
        .unwrap();
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[tokio::test(flavor = "multi_thread")]
async fn photos_and_faces_carry_their_thumbnail_so_the_grid_never_downloads_originals() {
    let b = bench("thumbs", None).await;
    let alice = basic("alice@example.org", &ALICE);
    put(&b.app, &alice, "/Photos/grande.jpg", &jpeg(1600, 1200)).await;
    put(&b.app, &alice, "/Photos/vide.txt", b"pas une image").await;
    let mut item = Value::Null;
    for _ in 0..100 {
        let (_, photos) = api(&b.app, Method::GET, "/api/v1/ucloud/photos", &alice, None).await;
        item = photos["items"][0].clone();
        if !item["thumb"].is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(item["path"], "/Photos/grande.jpg");
    assert_eq!(item["thumb"], json!({ "w": 320, "h": 240, "ow": 1600, "oh": 1200 }), "dimensions de la miniature et de l'original (rien du CID)");
    // La miniature est servie par la station, légère, en WebP.
    let (s, h, small) = call(&b.app, Method::GET, "/dav/Photos/grande.jpg?thumb", Some(&alice), &[], vec![]).await;
    assert_eq!((s, h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "image/webp"));
    let (_, _, full) = call(&b.app, Method::GET, "/dav/Photos/grande.jpg", Some(&alice), &[], vec![]).await;
    assert!(small.len() * 5 < full.len(), "{} contre {}", small.len(), full.len());
    // Un visage cité sur cette photo reçoit les mêmes dimensions.
    let owner = hex(&ALICE);
    b.db
        .writer()
        .execute(
            "INSERT INTO faces(face_id, owner_hex, cid, source_path, name, det_score, bbox_x1, bbox_y1, bbox_x2, bbox_y2, embed_hash, ts, ucloud_path) VALUES (1, ?1, 'c', '/p', 'Léa', 0.9, 100, 100, 400, 450, 'h1', 0, '/Photos/grande.jpg')",
            vec![V::Text(owner)],
        )
        .await
        .unwrap();
    let (_, f) = api(&b.app, Method::GET, "/api/v1/faces", &alice, None).await;
    assert_eq!(f["people"][0]["sample"]["thumb"]["ow"], 1600);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_footprint_shows_disk_quota_leases_and_the_node_tariffs() {
    let b = bench_with("footprint", None, |c| c.quota_multipass = 5_000, None).await;
    let alice = basic("alice@example.org", &ALICE);
    publish_tariffs(&b.db).await;
    put(&b.app, &alice, "/a.txt", &[b'x'; 3000]).await;
    let (s, f) = api(&b.app, Method::GET, "/api/v1/me/footprint", &alice, None).await;
    assert_eq!(s, StatusCode::OK, "{f}");
    assert_eq!((f["disk"]["used_bytes"].as_i64(), f["disk"]["quota_bytes"].as_i64(), f["disk"]["leased"].as_bool()), (Some(3000), Some(5000), Some(false)));
    assert_eq!(f["tariffs"].as_array().unwrap().len(), 2);

    // Plus de place : le dépôt de 3 000 octets de plus est refusé (507) ...
    let (s, ..) = call(&b.app, Method::PUT, "/dav/b.txt", Some(&alice), &[], vec![b'y'; 3000]).await;
    assert_eq!(s, StatusCode::INSUFFICIENT_STORAGE);
    // ... jusqu'à l'achat d'un palier de 128 Go (4 Ẑen / semaine, 1 semaine) : le quota monte.
    let t = now();
    buy(&b.db, &ALICE, "4.00", "disk_128gb", 1, t, "lease-disk").await;
    let (_, f) = api(&b.app, Method::GET, "/api/v1/me/footprint", &alice, None).await;
    assert_eq!((f["disk"]["quota_bytes"].as_i64(), f["disk"]["leased"].as_bool()), (Some(128 << 30), Some(true)));
    let (s, ..) = call(&b.app, Method::PUT, "/dav/b.txt", Some(&alice), &[], vec![b'y'; 3000]).await;
    assert_eq!(s, StatusCode::CREATED, "le quota relevé par le bail permet le dépôt");
    // Un bail de GPU sous-payé apparaît dans l'historique, avec son statut.
    buy(&b.db, &ALICE, "0.50", "gpu_1_24", 3, t, "lease-gpu").await;
    let (_, f) = api(&b.app, Method::GET, "/api/v1/me/footprint", &alice, None).await;
    let leases = f["leases"].as_array().unwrap();
    assert_eq!(leases.len(), 2);
    let gpu = leases.iter().find(|l| l["resource"] == "gpu_1_24").unwrap();
    assert_eq!((gpu["status"].as_str(), gpu["active"].as_bool(), gpu["units"].as_i64()), (Some("underpaid"), Some(false), Some(3)));
    let disk = leases.iter().find(|l| l["resource"] == "disk_128gb").unwrap();
    assert_eq!((disk["status"].as_str(), disk["active"].as_bool(), disk["mine"].as_bool()), (Some("active"), Some(true), Some(true)));
    // Opex (redevances d'usage) et Capex (parts d'investissement) sont deux comptes distincts.
    b.db.writer().execute("INSERT INTO rent_payments(event_id, owner_hex, at, amount_cents, src_kind) VALUES ('rent-1', ?1, ?2, 100, 30852)", vec![V::Text(hex(&ALICE)), V::Integer(t as i64)]).await.unwrap();
    // Capex : les actifs viennent du DID du NODE (kind 30800, `.assets[]`), projetés dès son insertion.
    {
        use astro_nostr_proto::{UnsignedEvent, kind};
        let node = [0x44u8; 32];
        let doc = json!({
            "id": format!("did:nostr:{}", astro_nostr_proto::pubkey_of(&node).unwrap()),
            "assets": [{"id": "rig-1", "label": "Serveur GPU (part)", "asset_type": "hardware", "email": "Alice@Example.org", "value_zen": 120.5, "amortized_zen": 20.25, "depreciable": true, "depreciation_weeks": 104, "status": "active"}],
        });
        let did = UnsignedEvent { created_at: t, kind: kind::DID, tags: vec![vec!["d".into(), "12D3KooWFSoxH5cZqhpcywX4P4ZKgaU1yTzcopYEvVhSQfHdhwwJ".into()]], content: doc.to_string() }.sign(&node).unwrap();
        b.db.writer()
            .transaction(move |tx| {
                astro_db::events::insert_event(tx, &did)?;
                astro_db::apply_statements(tx, &astro_sync::Projector::plan(&did).unwrap())?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let (_, f) = api(&b.app, Method::GET, "/api/v1/me/footprint", &alice, None).await;
    let (ox, cx) = (&f["opex"], &f["capex"]);
    assert_eq!((ox["weekly_rent_cents"].as_i64(), ox["rent_28d_cents"].as_i64(), ox["usage_28d_cents"].as_i64(), ox["usage_weekly_cents"].as_i64(), ox["total_28d_cents"].as_i64()), (Some(100), Some(100), Some(400), Some(400), Some(500)), "le bail sous-payé ne compte pas : {ox}");
    assert_eq!((cx["zencard"].as_bool(), cx["value_cents"].as_i64(), cx["amortized_cents"].as_i64(), cx["net_cents"].as_i64()), (Some(false), Some(12_050), Some(2_025), Some(10_025)), "{cx}");
    assert_eq!(cx["assets"][0]["label"], "Serveur GPU (part)");

    // Copie privée : sans le dépôt de médias branché, l'API le dit au lieu de mentir.
    let (s, c) = api(&b.app, Method::GET, "/api/v1/consent/private-copy", &alice, None).await;
    assert_eq!((s, c["accepted"].as_bool(), c["available"].as_bool(), c["article"].as_str()), (StatusCode::OK, Some(false), Some(false), Some("CPI L. 122-5")), "{c}");
    assert!(c["text"].as_str().unwrap().contains("usage strictement privé"));
    assert_eq!(api(&b.app, Method::POST, "/api/v1/consent/private-copy", &alice, Some(json!({ "version": 1 }))).await.0, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(call(&b.app, Method::GET, "/api/v1/consent/private-copy", None, &[], vec![]).await.0, StatusCode::UNAUTHORIZED);

    // Les baux d'Alice ne sont pas visibles de Bob.
    let bob = basic("bob@example.org", &BOB);
    assert!(api(&b.app, Method::GET, "/api/v1/me/footprint", &bob, None).await.1["leases"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_captain_screens_show_the_constellation_the_regie_and_the_hosted_accounts() {
    let swarm = std::env::temp_dir().join(format!("astroportd-swarm-{}", std::process::id()));
    for (id, json) in [
        ("12D3KooWnexus", json!({ "capacities": { "power_score": 160 }, "STATION_LAT": "43.30", "STATION_LON": "5.37", "dragon_services": "ollama.sh comfyui" })),
        ("12D3KooWrasp", json!({ "capacities": { "power_score": 8 }, "STATION_LAT": "48.85", "STATION_LON": "2.35", "dragon_services": "ssh" })),
    ] {
        std::fs::create_dir_all(swarm.join(id)).unwrap();
        std::fs::write(swarm.join(id).join("12345.json"), json.to_string()).unwrap();
    }
    let b = bench_with("captain", None, |_| {}, Some(swarm.clone())).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB)); // bob = Capitaine
    // Un MULTIPASS n'entre pas.
    for uri in ["/api/v1/station/constellation", "/api/v1/station/economy", "/api/v1/station/accounts"] {
        assert_eq!(api(&b.app, Method::GET, uri, &alice, None).await.0, StatusCode::FORBIDDEN, "{uri}");
    }
    assert_eq!(api(&b.app, Method::POST, &format!("/api/v1/station/accounts/{}/token", hex(&ALICE)), &alice, None).await.0, StatusCode::FORBIDDEN);

    // Constellation : triée par puissance, distance de Haversine depuis (45.76, 4.83), tarifs publiés.
    b.db.writer().execute("INSERT INTO identities(hex, role, peer_id, hosted) VALUES (?1, 'node', '12D3KooWnexus', 0)", vec![V::Text(hex(&[1; 32]))]).await.unwrap();
    publish_tariffs(&b.db).await;
    let (s, c) = api(&b.app, Method::GET, "/api/v1/station/constellation", &bob, None).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let ids: Vec<&str> = c["nodes"].as_array().unwrap().iter().map(|n| n["node_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["12D3KooWTest", "12D3KooWnexus", "12D3KooWrasp"], "ce nœud d'abord, puis par Power-Score décroissant");
    let nexus = &c["nodes"][1];
    assert_eq!((nexus["tier"].as_str(), nexus["status"].as_str()), (Some("Brain"), Some("known")));
    assert!((nexus["distance_km"].as_f64().unwrap() - 277.0).abs() < 8.0, "Lyon → Marseille : {}", nexus["distance_km"]);
    assert_eq!(nexus["services"], json!(["ollama", "comfyui"]));
    assert_eq!(nexus["tariffs"].as_array().unwrap().len(), 2, "tarifs publiés par ce nœud (30850)");
    assert_eq!(c["nodes"][0]["self"], true);
    assert_eq!(c["nodes"][0]["services"], json!(["comfyui", "ssh"]));

    // Régie : rien tant qu'aucune clôture ; puis une ventilation 3×1/3 et le Love Ledger.
    let (_, e) = api(&b.app, Method::GET, "/api/v1/station/economy", &bob, None).await;
    assert_eq!((e["kind"].as_str(), e["love"]["total_cents"].as_i64()), (Some("none"), Some(0)));
    let alloc = astro_economy::Allocation { week: "2026-W43".into(), surplus: astro_core::Amount(30_000), is_provision: astro_core::Amount(4_500), net: astro_core::Amount(25_500), captain_bonus: astro_core::Amount(255), cash: astro_core::Amount(8_415), rnd: astro_core::Amount(8_415), assets: astro_core::Amount(8_415) };
    b.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('economy.3x13', 1790000000, ?1)", vec![V::Text(alloc.to_json().to_string())]).await.unwrap();
    b.db.writer().execute("INSERT INTO love_ledger(at, kind, amount_cents, detail) VALUES (1790000100, 'captain_contribution', 1500, '{\"week\":\"2026-W42\"}')", vec![]).await.unwrap();
    let (_, e) = api(&b.app, Method::GET, "/api/v1/station/economy", &bob, None).await;
    assert_eq!((e["kind"].as_str(), e["week"].as_str()), (Some("split"), Some("2026-W43")));
    let shares: Vec<(&str, i64)> = e["shares"].as_array().unwrap().iter().map(|s| (s["name"].as_str().unwrap(), s["cents"].as_i64().unwrap())).collect();
    assert_eq!(shares, [("Trésorerie", 8415), ("R&D", 8415), ("Actifs", 8415)]);
    assert_eq!(e["shares"][0]["pct"], 33.3);
    assert_eq!((e["allocation"]["is_provision_cents"].as_i64(), e["allocation"]["captain_bonus_cents"].as_i64()), (Some(4500), Some(255)));
    assert_eq!((e["love"]["total_cents"].as_i64(), e["love"]["weeks"].as_i64(), e["love"]["recent"][0]["detail"]["week"].as_str()), (Some(1500), Some(1), Some("2026-W42")));

    // Comptes : cycle de vie (kv lifecycle), disque, quota ; jeton de secours tracé.
    put(&b.app, &alice, "/doc.txt", b"bonjour").await;
    b.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('lifecycle', ?1, ?2, 0)", vec![V::Text(hex(&ALICE)), V::Text(json!({ "status": "grace_period", "last_ok_at": 1, "status_since": 1_790_000_000 }).to_string())]).await.unwrap();
    let (_, a) = api(&b.app, Method::GET, "/api/v1/station/accounts", &bob, None).await;
    let alice_row = a["accounts"].as_array().unwrap().iter().find(|x| x["email"] == "alice@example.org").unwrap();
    assert_eq!((alice_row["status"].as_str(), alice_row["used_bytes"].as_i64(), alice_row["files"].as_i64()), (Some("grace_period"), Some(7), Some(1)));
    assert_eq!(alice_row["grace_ends_at"], 1_790_000_000 + 7 * 86_400);
    assert_eq!(alice_row["quota_bytes"], 10i64 << 30);
    let (s, t) = api(&b.app, Method::POST, &format!("/api/v1/station/accounts/{}/token", hex(&ALICE)), &bob, None).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!((t["login"].as_str(), t["token"].as_str()), (Some("alice@example.org"), Some(derive_dav_token(SECRET, &hex(&ALICE)).as_str())));
    // Le jeton remis fonctionne vraiment, et l'opération est tracée.
    let (s, ..) = call(&b.app, Method::GET, "/api/v1/me", Some(&basic("alice@example.org", &ALICE)), &[], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    let audits: i64 = b.db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM journal WHERE stream = 'audit.token'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(audits, 1);
    assert_eq!(api(&b.app, Method::POST, &format!("/api/v1/station/accounts/{}/token", hex(&[9; 32])), &bob, None).await.0, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(swarm);
}

#[tokio::test(flavor = "multi_thread")]
async fn anyone_can_create_a_multipass_from_the_web_and_log_in_with_the_returned_token() {
    let b = bench("register", None).await;
    // Sans compte : la page, ses fichiers et l'état de l'inscription sont publics.
    let (s, h, body) = call(&b.app, Method::GET, "/register", None, &[], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("Créer mon MULTIPASS") && h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("script-src 'self'"));
    for f in ["register.js", "register.css"] {
        assert_eq!(call(&b.app, Method::GET, &format!("/ui/{f}"), None, &[], vec![]).await.0, StatusCode::OK, "{f}");
    }
    let (s, _, body) = call(&b.app, Method::GET, "/api/v1/register", None, &[], vec![]).await;
    let info: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((s, info["open"].as_bool(), info["constellation"].as_str()), (StatusCode::OK, Some(true), Some("ORIGIN")), "{info}");

    let post = |email: &str| (&[("content-type", "application/json")][..], json!({ "email": email, "lang": "en" }).to_string().into_bytes());
    let (hdr, bytes) = post("Nouveau@Exemple.org");
    let (s, h, body) = call(&b.app, Method::POST, "/api/v1/register", None, hdr, bytes).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(h[header::CACHE_CONTROL], "no-store", "les secrets ne se mettent pas en cache");
    assert_eq!((v["email"].as_str(), v["constellation"].as_str()), (Some("nouveau@exemple.org"), Some("ORIGIN")));
    for k in ["token", "pass", "salt", "pepper", "nsec", "npub", "hex", "g1pub", "nostrns"] {
        assert!(v[k].as_str().is_some_and(|x| !x.is_empty()), "{k} : {v}");
    }
    assert!(v["nsec"].as_str().unwrap().starts_with("nsec1") && v["g1pub"].as_str().unwrap().starts_with('g'));

    // Le jeton rendu ouvre l'interface : le compte est connu de la base dès la réponse.
    let login = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("nouveau@exemple.org:{}", v["token"].as_str().unwrap())));
    let (s, me) = api(&b.app, Method::GET, "/api/v1/me", &login, None).await;
    assert_eq!((s, me["email"].as_str(), me["hex"].as_str()), (StatusCode::OK, Some("nouveau@exemple.org"), v["hex"].as_str()), "{me}");
    assert!(b.dir.join("nostr/nouveau@exemple.org/.secret.nostr").is_file());

    // Refus : doublon, adresse invalide, corps illisible.
    let (hdr, bytes) = post("NOUVEAU@exemple.org");
    assert_eq!(call(&b.app, Method::POST, "/api/v1/register", None, hdr, bytes).await.0, StatusCode::CONFLICT);
    let (hdr, bytes) = post("../../etc@x.y");
    assert_eq!(call(&b.app, Method::POST, "/api/v1/register", None, hdr, bytes).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(call(&b.app, Method::POST, "/api/v1/register", None, &[("content-type", "application/json")], b"pas du json".to_vec()).await.0, StatusCode::BAD_REQUEST);
    // L'inscription n'ouvre aucune autre porte : le reste de l'API exige toujours un compte.
    assert_eq!(call(&b.app, Method::GET, "/api/v1/me", None, &[], vec![]).await.0, StatusCode::UNAUTHORIZED);
}
