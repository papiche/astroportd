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
    tune(&mut config);
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config, on_upload: None });
    let bus = ActivityBus::new();
    let info = StationInfo { node_hex: hex(&[1; 32]), peer_id: Some("12D3KooWTest".into()), domain: Some("station.test".into()), started: Instant::now() };
    let services = Arc::new(StationServices::new(StationDeps { db: db.clone(), info, nostr_dir: None, chain: None, hardware: Hardware { vram_gb: 24.0, cpu_cores: 16, ram_gb: 64.0 }, p2p: None, weekly_rent_cents: 100, activity: bus.clone(), swarm_dir, origin: Some((45.76, 4.83)), exposed: vec!["comfyui".into(), "ssh".into()] }));
    services.set_build(astroportd::build_attest::BuildInfo { version: "0.2.2-beta".into(), sha256: "ab".repeat(32), target: "x86_64-linux".into() });
    let client = ollama.map(|url| Arc::new(OllamaClient::new(url, "bge-m3", "llama3.2", Duration::from_secs(5))));
    let memory = client.as_ref().map(|o| Arc::new(MemoryStore::new(db.clone(), o.clone(), o.clone(), Some(bus.clone()))));
    let registrar = Arc::new(astroportd::register::Registrar::new(db.clone(), dir.join("nostr"), "0".repeat(64), Arc::new(|hex| derive_dav_token(SECRET, hex)), astroportd::register::Limits::default()));
    let links = Arc::new(astroportd::identity_links::IdentityLinks::new(db.clone(), dav.clone(), None, None));
    // Les invitations signent avec la clé du compte : celle d'Alice est rangée comme le fait la station (`.secret.nostr`).
    std::fs::create_dir_all(dir.join("nostr/alice@example.org")).unwrap();
    let nsec = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), &ALICE).unwrap();
    std::fs::write(dir.join("nostr/alice@example.org/.secret.nostr"), format!("NSEC={nsec}; NPUB=x; HEX={}\n", hex(&ALICE))).unwrap();
    std::fs::write(dir.join("nostr/alice@example.org/.pass"), "123456\n").unwrap();
    std::fs::create_dir_all(dir.join("nostr/bob@example.org")).unwrap();
    let nsec = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), &BOB).unwrap();
    std::fs::write(dir.join("nostr/bob@example.org/.secret.nostr"), format!("NSEC={nsec}; NPUB=x; HEX={}\n", hex(&BOB))).unwrap();
    // Deux stations de test se parlent en local : adresses privées et http admis pour les appels de station à station.
    let mut local = astroportd::fetch::FetchLimits::new(256 * 1024);
    local.allow_private = true;
    let invites = Arc::new(astroportd::invites::Invites::new(db.clone(), dav.blobs.clone(), Arc::new(astro_crypto::FileKeySource::new(dir.join("nostr"))), registrar.clone(), Arc::new(|_| Box::pin(async { Ok(()) }))).with_links(links.clone()).with_fetch_limits(local));
    let ui = Arc::new(StationUi::new(services, dav.clone(), memory, client, true).with_registrar(registrar).with_invites(invites).with_account(Arc::new(astroportd::account::AccountService::new(db.clone(), dir.join("nostr"), Arc::new(StaticKeySource::new().with(ALICE).with(BOB)), SECRET.to_vec(), Arc::new(|_| Box::pin(async { Ok(()) })), None).with_activity(bus.clone()))).with_contacts(links).with_coop(
        Arc::new(|| Box::pin(async { Ok(json!({ "exists": false, "sections": [] })) })),
        Arc::new(|values| Box::pin(async move { Ok(json!({ "saved": values.keys().collect::<Vec<_>>() })) })),
    ).with_admin(Arc::new(|section, body| Box::pin(async move {
        match (section.as_str(), body) {
            ("face", None) => Ok(json!({ "mode": "off" })),
            ("face", Some(b)) if b["mode"] == "gpu" => Err("mode attendu : off, local ou remote".into()),
            ("face", Some(b)) => Ok(b),
            _ => Err(format!("section inconnue : {section}")),
        }
    }))));
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
    assert!(d["balance"]["error"].as_str().unwrap().contains("Ğ1"), "solde indisponible dit clairement : {d}");
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
    assert!(r["error"].as_str().unwrap().contains("Ollama"));
    let (_, r) = api(&b.app, Method::POST, "/api/v1/bro/say", &alice, Some(json!({ "text": "#solde" }))).await;
    assert!(r["reply"].as_str().unwrap().starts_with("⚠️ solde indisponible"), "{r}");
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
async fn the_footprint_shows_disk_quota_and_the_opex_capex_accounts() {
    let b = bench_with("footprint", None, |c| c.quota_multipass = 5_000, None).await;
    let alice = basic("alice@example.org", &ALICE);
    put(&b.app, &alice, "/a.txt", &[b'x'; 3000]).await;
    let (s, f) = api(&b.app, Method::GET, "/api/v1/me/footprint", &alice, None).await;
    assert_eq!(s, StatusCode::OK, "{f}");
    assert_eq!((f["disk"]["used_bytes"].as_i64(), f["disk"]["quota_bytes"].as_i64()), (Some(3000), Some(5000)));

    // Plus de place : le dépôt de 3 000 octets de plus est refusé (507) : le quota est celui de la formule.
    let (s, ..) = call(&b.app, Method::PUT, "/dav/b.txt", Some(&alice), &[], vec![b'y'; 3000]).await;
    assert_eq!(s, StatusCode::INSUFFICIENT_STORAGE);
    let t = now();
    // Opex (redevances d'usage) et Capex (parts d'investissement) sont deux comptes distincts.
    b.db.writer().execute("INSERT INTO rent_payments(event_id, owner_hex, at, amount_cents, src_kind) VALUES ('rent-1', ?1, ?2, 100, 30852)", vec![V::Text(hex(&ALICE)), V::Integer(t as i64)]).await.unwrap();
    // Capex : les actifs viennent du DID du NODE (kind 30800, `.assets[]`), projetés dès son insertion.
    {
        use astro_nostr_proto::{UnsignedEvent, kind};
        let node = [0x44u8; 32];
        let doc = json!({
            "id": format!("did:nostr:{}", astro_nostr_proto::pubkey_of(&node).unwrap()),
            "assets": [{"id": "rig-1", "label": "Serveur (part)", "asset_type": "hardware", "email": "Alice@Example.org", "value_zen": 120.5, "amortized_zen": 20.25, "depreciable": true, "depreciation_weeks": 104, "status": "active"}],
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
    assert_eq!((ox["weekly_rent_cents"].as_i64(), ox["rent_28d_cents"].as_i64(), ox["total_28d_cents"].as_i64()), (Some(100), Some(100), Some(100)), "{ox}");
    assert_eq!((cx["zencard"].as_bool(), cx["value_cents"].as_i64(), cx["amortized_cents"].as_i64(), cx["net_cents"].as_i64()), (Some(false), Some(12_050), Some(2_025), Some(10_025)), "{cx}");
    assert_eq!(cx["assets"][0]["label"], "Serveur (part)");

    // Copie privée : sans le dépôt de médias branché, l'API le dit au lieu de mentir.
    let (s, c) = api(&b.app, Method::GET, "/api/v1/consent/private-copy", &alice, None).await;
    assert_eq!((s, c["accepted"].as_bool(), c["available"].as_bool(), c["article"].as_str()), (StatusCode::OK, Some(false), Some(false), Some("CPI L. 122-5")), "{c}");
    assert!(c["text"].as_str().unwrap().contains("usage strictement privé"));
    assert_eq!(api(&b.app, Method::POST, "/api/v1/consent/private-copy", &alice, Some(json!({ "version": 1 }))).await.0, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(call(&b.app, Method::GET, "/api/v1/consent/private-copy", None, &[], vec![]).await.0, StatusCode::UNAUTHORIZED);

    // Le Capex d'Alice n'est pas visible de Bob.
    let bob = basic("bob@example.org", &BOB);
    assert!(api(&b.app, Method::GET, "/api/v1/me/footprint", &bob, None).await.1["capex"]["assets"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_library_answers_have_search_timeline_and_duplicates_for_the_account_only() {
    let b = bench("lib", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    let sha = "ab".repeat(32);
    for (owner, path, taken, phash) in [(hex(&ALICE), "/Vacances/mer.jpg", 1_715_947_200_i64, Some(0x0F0F_i64)), (hex(&ALICE), "/Vacances/copie.jpg", 1_715_947_300, Some(0x0F0F)), (hex(&BOB), "/Vacances/bob.jpg", 1_715_947_200, None)] {
        b.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, sha_plain, phash, data) VALUES (?1, ?2, '/Vacances', 'file', ?3, 'image/jpeg', 100, 1, ?4, ?5, ?6, '{}')", vec![V::Text(owner), V::Text(path.into()), V::Text(format!("cid{path}")), V::Integer(taken), V::Text(sha.clone()), phash.map_or(V::Null, V::Integer)]).await.unwrap();
    }
    for uri in ["/api/v1/ucloud/search", "/api/v1/ucloud/timeline", "/api/v1/ucloud/memories", "/api/v1/ucloud/duplicates"] {
        assert_eq!(api(&b.app, Method::GET, uri, "", None).await.0, StatusCode::UNAUTHORIZED, "{uri} sans compte");
    }
    let (s, r) = api(&b.app, Method::GET, "/api/v1/ucloud/search?q=vacances&kind=image", &alice, None).await;
    assert_eq!((s, r["items"].as_array().map(Vec::len)), (StatusCode::OK, Some(2)), "{r}");
    let (_, r) = api(&b.app, Method::GET, "/api/v1/ucloud/search?q=vacances&limit=1", &alice, None).await;
    assert!(r["next"]["path"].is_string(), "{r}");
    assert_eq!(api(&b.app, Method::GET, "/api/v1/ucloud/search?bbox=1,2,3", &alice, None).await.0, StatusCode::BAD_REQUEST);
    let (_, r) = api(&b.app, Method::GET, "/api/v1/ucloud/timeline", &alice, None).await;
    assert_eq!(r["months"][0], json!({ "ym": "2024-05", "count": 2 }));
    let (_, r) = api(&b.app, Method::GET, "/api/v1/ucloud/timeline?ym=2024-05", &alice, None).await;
    assert_eq!(r["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(api(&b.app, Method::GET, "/api/v1/ucloud/timeline?ym=abc", &alice, None).await.0, StatusCode::BAD_REQUEST);
    let (_, r) = api(&b.app, Method::GET, "/api/v1/ucloud/memories?md=05-17&y=2026", &alice, None).await;
    assert_eq!(r["memories"][0]["year"], 2024, "souvenirs du jour donné par le navigateur : {r}");
    let (_, r) = api(&b.app, Method::POST, "/api/v1/ucloud/have", &alice, Some(json!({ "shas": [sha, "cd".repeat(32)] }))).await;
    assert_eq!(r["have"].as_array().map(Vec::len), Some(1));
    let (_, r) = api(&b.app, Method::POST, "/api/v1/ucloud/have", &bob, Some(json!({ "shas": [sha] }))).await;
    assert_eq!(r["have"][0]["path"], "/Vacances/bob.jpg", "chaque compte ne voit que le sien");
    assert_eq!(api(&b.app, Method::POST, "/api/v1/ucloud/have", &alice, Some(json!({ "shas": vec!["a"; 501] }))).await.0, StatusCode::BAD_REQUEST);
    let (_, r) = api(&b.app, Method::GET, "/api/v1/ucloud/duplicates", &alice, None).await;
    assert_eq!(r["exact"].as_array().map(Vec::len), Some(1), "{r}");
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

    // Constellation : triée par puissance, distance de Haversine depuis (45.76, 4.83).
    b.db.writer().execute("INSERT INTO identities(hex, role, peer_id, hosted) VALUES (?1, 'node', '12D3KooWnexus', 0)", vec![V::Text(hex(&[1; 32]))]).await.unwrap();
    let (s, c) = api(&b.app, Method::GET, "/api/v1/station/constellation", &bob, None).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let ids: Vec<&str> = c["nodes"].as_array().unwrap().iter().map(|n| n["node_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["12D3KooWTest", "12D3KooWnexus", "12D3KooWrasp"], "ce nœud d'abord, puis par Power-Score décroissant");
    let nexus = &c["nodes"][1];
    assert_eq!((nexus["tier"].as_str(), nexus["status"].as_str()), (Some("Brain"), Some("known")));
    assert!((nexus["distance_km"].as_f64().unwrap() - 277.0).abs() < 8.0, "Lyon → Marseille : {}", nexus["distance_km"]);
    assert_eq!(nexus["services"], json!(["ollama", "comfyui"]));
    assert_eq!(c["nodes"][0]["self"], true);
    assert_eq!(c["nodes"][0]["services"], json!(["comfyui", "ssh"]));

    // Code identique entre stations : l'attestation de chaque pair (kind 30853) est comparée au binaire local.
    let attest = |station: &str, sha: &str| json!({ "event_id": "e", "created_at": 1, "tags": { "d": "build", "version": "0.2.2-beta", "sha256": sha, "target": "x86_64-linux", "station": station } }).to_string();
    for (key, station, sha) in [("aa", "12D3KooWnexus", "ab".repeat(32)), ("bb", "12D3KooWrasp", "cd".repeat(32))] {
        b.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('station_build', ?1, ?2, 1)", vec![V::Text(key.into()), V::Text(attest(station, &sha))]).await.unwrap();
    }
    let (_, c) = api(&b.app, Method::GET, "/api/v1/station/constellation", &bob, None).await;
    let verdicts: Vec<&str> = c["nodes"].as_array().unwrap().iter().map(|n| n["build"]["verdict"].as_str().unwrap()).collect();
    assert_eq!(verdicts, ["self", "identical", "diverges"], "{c}");

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

#[tokio::test(flavor = "multi_thread")]
async fn replication_shows_where_and_by_whom_and_only_for_the_account_itself() {
    let b = bench("replication", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    let doc = json!({ "at": 1, "events": 30, "replicated": 28, "unreachable": 1, "peers": [{ "node_id": "12D3Ami", "name": "poppy", "captain": "ami@example.org", "level": 1, "have": 28, "error": null }], "media": null });
    b.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('replication', ?1, ?2, 0)", vec![V::Text(hex(&ALICE)), V::Text(doc.to_string())]).await.unwrap();
    b.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('crawl_status', 'last', '{\"at\":2,\"inserted\":5}', 0), ('crawl_status', 'routes', '[{\"target\":\"p2p://12D3Ami\",\"via\":\"p2p\"}]', 0)", vec![]).await.unwrap();

    let (status, mine) = api(&b.app, Method::GET, "/api/v1/replication", &alice, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((mine["mine"]["replicated"].as_u64(), mine["mine"]["peers"][0]["name"].as_str()), (Some(28), Some("poppy")), "{mine}");
    assert_eq!((mine["crawl"]["inserted"].as_u64(), mine["routes"][0]["via"].as_str()), (Some(5), Some("p2p")), "{mine}");
    // Bob n'a pas de bilan : il ne voit jamais celui d'Alice.
    let (_, theirs) = api(&b.app, Method::GET, "/api/v1/replication", &bob, None).await;
    assert!(theirs["mine"].is_null(), "{theirs}");
    assert_eq!(call(&b.app, Method::GET, "/api/v1/replication", None, &[], Vec::new()).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sorting_table_api_lists_groups_assigns_and_forgets_for_the_owner_only() {
    let b = bench("board", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    let owner = hex(&ALICE);
    // Trois visages inconnus de la même personne, un autre inconnu, une personne nommée.
    let vec_of = |axis: usize, tilt: usize| {
        let mut v = vec![0f32; 512];
        v[axis] = 1.0;
        v[tilt] += 0.15;
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect::<Vec<f32>>()
    };
    for (id, name, axis, tilt) in [(1i64, "Inconnu_a", 0usize, 10usize), (2, "Inconnu_b", 0, 11), (3, "Inconnu_c", 0, 12), (4, "Inconnu_d", 5, 20), (5, "Léa", 7, 30)] {
        b.db.writer()
            .execute(
                "INSERT INTO faces(face_id, owner_hex, cid, source_path, name, det_score, bbox_x1, bbox_y1, bbox_x2, bbox_y2, embed_hash, ts, ucloud_path) VALUES (?1, ?2, ?3, '/p', ?4, 0.9, 10, 20, 110, 140, ?5, 0, ?6)",
                vec![V::Integer(id), V::Text(owner.clone()), V::Text(format!("c{id}")), V::Text(name.into()), V::Text(format!("h{id}")), V::Text(format!("/Photos/{id}.jpg"))],
            )
            .await
            .unwrap();
        b.db.writer().upsert_vector(astro_db::VectorTable::Face, id, &owner, &vec_of(axis, tilt)).await.unwrap();
    }
    let (s, board) = api(&b.app, Method::GET, "/api/v1/faces/board", &alice, None).await;
    assert_eq!(s, StatusCode::OK, "{board}");
    assert_eq!((board["faces"].as_array().unwrap().len(), board["unknown"].as_i64(), board["people"][0]["name"].as_str()), (5, Some(4), Some("Léa")), "{board}");
    let face = |id: i64| board["faces"].as_array().unwrap().iter().find(|f| f["id"] == id).unwrap().clone();
    assert!(face(1)["group_id"].is_string() && face(1)["group_id"] == face(2)["group_id"] && face(1)["group_size"] == 3, "{board}");
    assert!(face(4)["group_id"].is_null() && face(5)["group_id"].is_null());
    assert!(face(1)["x"].as_f64().unwrap().abs() <= 1.0 && face(1)["bbox"] == json!([10.0, 20.0, 110.0, 140.0]));
    // Bob ne voit rien d'Alice et ne peut ni ranger ni oublier ses visages.
    assert_eq!(api(&b.app, Method::GET, "/api/v1/faces/board", &bob, None).await.1["faces"].as_array().unwrap().len(), 0);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/assign", &bob, Some(json!({ "ids": [1, 2, 3], "name": "Intrus" }))).await.1["assigned"], 0);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/forget", &bob, Some(json!({ "ids": [1] }))).await.1["forgotten"], 0);
    // Ranger le groupe d'un seul geste.
    let (s, r) = api(&b.app, Method::POST, "/api/v1/faces/assign", &alice, Some(json!({ "ids": [1, 2, 3], "name": " Maman " }))).await;
    assert_eq!((s, r["assigned"].as_i64()), (StatusCode::OK, Some(3)), "{r}");
    let (_, after) = api(&b.app, Method::GET, "/api/v1/faces/board", &alice, None).await;
    assert_eq!(after["unknown"], 1);
    assert!(after["people"].as_array().unwrap().iter().any(|p| p["name"] == "Maman" && p["faces"] == 3), "{after}");
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/forget", &alice, Some(json!({ "ids": [4] }))).await.1["forgotten"], 1);
    // Entrées invalides : refusées avant d'atteindre la base.
    for body in [json!({ "ids": [], "name": "x" }), json!({ "ids": [0], "name": "x" }), json!({ "ids": [-3], "name": "x" }), json!({ "ids": [1], "name": "  " }), json!({ "ids": [1], "name": "x", "pubkey": "pas-hex" })] {
        assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/assign", &alice, Some(body.clone())).await.0, StatusCode::BAD_REQUEST, "{body}");
    }
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/forget", &alice, Some(json!({ "ids": [] }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/thumbs", &alice, Some(json!({ "ids": [1], "size": 8 }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/faces/thumbs", &alice, Some(json!({ "ids": [1], "size": 96 }))).await.0, StatusCode::NOT_IMPLEMENTED, "sans service FaceCloud câblé dans ce banc d'essai");
    assert_eq!(call(&b.app, Method::GET, "/api/v1/faces/board", None, &[], Vec::new()).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cooperative_config_is_for_the_captain_only() {
    let b = bench("coop", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    assert_eq!(api(&b.app, Method::GET, "/api/v1/station/coop", &alice, None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/station/coop", &alice, Some(json!({ "values": { "PAF": "50" } }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(api(&b.app, Method::GET, "/api/v1/station/coop", &bob, None).await.0, StatusCode::OK);
    let (s, v) = api(&b.app, Method::POST, "/api/v1/station/coop", &bob, Some(json!({ "values": { "PAF": "50", "OCAPIKEY": null } }))).await;
    assert_eq!((s, v["saved"].as_array().map(Vec::len)), (StatusCode::OK, Some(2)));
    for bad in [json!({ "values": {} }), json!({ "values": { "paf": "1" } }), json!({ "values": { "A B": "1" } })] {
        assert_eq!(api(&b.app, Method::POST, "/api/v1/station/coop", &bob, Some(bad)).await.0, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_sections_are_for_the_captain_and_report_errors_properly() {
    let b = bench("admin", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    assert_eq!(api(&b.app, Method::GET, "/api/v1/station/admin/face", &alice, None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/station/admin/face", &alice, Some(json!({ "mode": "off" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(api(&b.app, Method::GET, "/api/v1/station/admin/face", &bob, None).await, (StatusCode::OK, json!({ "mode": "off" })));
    assert_eq!(api(&b.app, Method::POST, "/api/v1/station/admin/face", &bob, Some(json!({ "mode": "local" }))).await, (StatusCode::OK, json!({ "mode": "local" })));
    assert_eq!(api(&b.app, Method::POST, "/api/v1/station/admin/face", &bob, Some(json!({ "mode": "gpu" }))).await.0, StatusCode::BAD_REQUEST, "une erreur de validation est un 400");
    assert_eq!(api(&b.app, Method::GET, "/api/v1/station/admin/inconnue", &bob, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(api(&b.app, Method::GET, "/api/v1/station/admin/Bad-Name", &bob, None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_address_book_is_synced_listed_per_owner_and_linkable() {
    let b = bench("contacts", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Bob Ami\r\nEMAIL:bob@example.org\r\nEND:VCARD\r\n";
    // Dépôt par CardDAV (comme le ferait DAVx⁵), puis relecture du carnet.
    let (s, ..) = call(&b.app, Method::PUT, "/dav/addressbooks/alice%40example.org/contacts/bob.vcf", Some(&alice), &[("content-type", "text/vcard"), ("if-none-match", "*")], card.as_bytes().to_vec()).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, r) = api(&b.app, Method::POST, "/api/v1/contacts/sync", &alice, Some(json!({}))).await;
    assert_eq!((s, r["indexed"].as_u64()), (StatusCode::OK, Some(1)));
    let (s, v) = api(&b.app, Method::GET, "/api/v1/contacts", &alice, None).await;
    assert_eq!(s, StatusCode::OK);
    // L'email de Bob est celui d'un compte hébergé ici : la fiche est reliée sans clé explicite.
    assert_eq!((v["contacts"][0]["name"].as_str(), v["contacts"][0]["via"].as_str(), v["contacts"][0]["pubkey"].as_str()), (Some("Bob Ami"), Some("email"), Some(hex(&BOB).as_str())));
    assert_eq!(v["carddav"]["login"], "alice@example.org");
    // Chacun son carnet.
    assert_eq!(api(&b.app, Method::GET, "/api/v1/contacts", &bob, None).await.1["contacts"].as_array().unwrap().len(), 0);
    // Création depuis une personne, liaison manuelle, erreurs typées.
    let (s, c) = api(&b.app, Method::POST, "/api/v1/contacts/from-person", &alice, Some(json!({ "name": "Nina" }))).await;
    assert_eq!((s, c["path"].as_str()), (StatusCode::OK, Some("/Contacts/Nina.vcf")));
    assert_eq!(api(&b.app, Method::POST, "/api/v1/contacts/from-person", &alice, Some(json!({ "name": "nina" }))).await.0, StatusCode::CONFLICT);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/contacts/link", &alice, Some(json!({ "path": "/Contacts/Nina.vcf", "target": "bob@example.org" }))).await.0, StatusCode::OK);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/contacts/link", &alice, Some(json!({ "path": "/Contacts/Nina.vcf", "target": "inconnu@example.org" }))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(api(&b.app, Method::POST, "/api/v1/contacts/link", &alice, Some(json!({ "path": "/Photos/x.jpg" }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(api(&b.app, Method::GET, "/api/v1/contacts", "Basic bm9wZTpub3Bl", None).await.0, StatusCode::UNAUTHORIZED);
}

fn png_bytes() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(40, 30, |x, y| image::Rgba([(x * 6) as u8, (y * 8) as u8, 120, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[tokio::test]
async fn an_invitation_link_is_public_blurred_single_use_and_creates_the_account() {
    let b = bench("invite", None).await;
    let alice = basic("alice@example.org", &ALICE);
    put(&b.app, &alice, "/Photos/a.png", &png_bytes()).await;

    // Créer : authentification requise ; le lien est celui de la station, avec son QR.
    assert_eq!(api(&b.app, Method::POST, "/api/v1/invites", "Basic nope", Some(json!({ "paths": ["/Photos"] }))).await.0, StatusCode::UNAUTHORIZED);
    let (s, made) = api(&b.app, Method::POST, "/api/v1/invites", &alice, Some(json!({ "paths": ["/Photos"], "label": "Léa" }))).await;
    assert_eq!(s, StatusCode::OK, "{made}");
    let (token, url) = (made["token"].as_str().unwrap().to_owned(), made["url"].as_str().unwrap().to_owned());
    assert_eq!(url, format!("https://station.test/i/{token}"));
    assert!(made["qr_svg"].as_str().unwrap().contains("<svg"));

    // La page du lien : publique, jamais indexée, sans le jeton dans son contenu.
    let (s, h, body) = call(&b.app, Method::GET, &format!("/i/{token}"), None, &[("accept", "text/html")], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["x-robots-tag"].to_str().unwrap().contains("noindex") && String::from_utf8_lossy(&body).contains("invite.js") && !String::from_utf8_lossy(&body).contains(&token));
    // Son aperçu, sans compte : qui invite et combien.
    let (s, _, raw) = call(&b.app, Method::GET, &format!("/api/v1/invite/{token}"), None, &[], vec![]).await;
    let preview: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!((s, preview["from"].as_str(), preview["count"].as_u64()), (StatusCode::OK, Some("alice"), Some(1)), "{preview}");
    // La vignette (faite en tâche de fond) est un PNG de quelques pixels.
    let mut thumb = None;
    for _ in 0..100 {
        let (s, h, png) = call(&b.app, Method::GET, &format!("/api/v1/invite/{token}/thumb/0"), None, &[], vec![]).await;
        if s == StatusCode::OK {
            assert_eq!(h[header::CONTENT_TYPE], "image/png");
            thumb = Some(png);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let img = image::load_from_memory(&thumb.unwrap()).unwrap();
    assert!(img.width().max(img.height()) <= 16);

    // Réclamer : le compte naît, ses secrets sont remis une fois ; le lien ne sert plus.
    let (s, _, raw) = call(&b.app, Method::POST, &format!("/api/v1/invite/{token}/claim"), None, &[("content-type", "application/json")], json!({ "email": "lea@example.org", "lang": "fr" }).to_string().into_bytes()).await;
    let card: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!((s, card["email"].as_str(), card["shared"].as_u64(), card["from"].as_str()), (StatusCode::OK, Some("lea@example.org"), Some(1), Some("alice")), "{card}");
    assert!(card["nsec"].as_str().unwrap().starts_with("nsec1") && card["token"].as_str().is_some());
    let (s, ..) = call(&b.app, Method::POST, &format!("/api/v1/invite/{token}/claim"), None, &[("content-type", "application/json")], json!({ "email": "autre@example.org" }).to_string().into_bytes()).await;
    assert_eq!(s, StatusCode::GONE);
    assert_eq!(call(&b.app, Method::GET, &format!("/api/v1/invite/{token}"), None, &[], vec![]).await.0, StatusCode::GONE);
    let (_, list) = api(&b.app, Method::GET, "/api/v1/invites", &alice, None).await;
    assert_eq!((list["invites"][0]["state"].as_str(), list["invites"][0]["url"].as_str()), (Some("claimed"), Some(url.as_str())));
    // Elle se connecte avec ses identifiants et voit les photos reçues.
    let lea = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("lea@example.org:{}", card["token"].as_str().unwrap())));
    let (s, v) = api(&b.app, Method::GET, "/api/v1/ucloud/list?path=/Partages/De_alice", &lea, None).await;
    assert_eq!((s, v["entries"].as_array().map(Vec::len)), (StatusCode::OK, Some(1)), "{v}");
    let _ = &b.bus;
}

#[tokio::test]
async fn the_pass_logs_in_locks_progressively_and_the_captain_can_unlock() {
    let b = bench("pass", None).await;
    let post = |uri: String, body: Value, auth: Option<String>| {
        let app = b.app.clone();
        async move {
            let (s, _, raw) = call(&app, Method::POST, &uri, auth.as_deref(), &[("content-type", "application/json")], body.to_string().into_bytes()).await;
            (s, serde_json::from_slice::<Value>(&raw).unwrap_or(Value::Null))
        }
    };
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    // Le bon PASS rend un jeton qui ouvre la session ; la signature est déléguée à la station (la clé ne sort pas).
    let (s, ok) = post("/api/v1/account/unlock".into(), json!({ "email": "Alice@Example.org", "pass": "123456" }), None).await;
    assert_eq!((s, ok["hex"].as_str()), (StatusCode::OK, Some(hex(&ALICE).as_str())), "{ok}");
    let session = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("alice@example.org:{}", ok["token"].as_str().unwrap())));
    assert_eq!(api(&b.app, Method::GET, "/api/v1/me", &session, None).await.0, StatusCode::OK);
    let (s, ev) = post("/api/v1/account/sign".into(), json!({ "kind": 1, "content": "bonjour", "tags": [] }), Some(session.clone())).await;
    assert_eq!((s, ev["pubkey"].as_str(), ev["content"].as_str()), (StatusCode::OK, Some(hex(&ALICE).as_str()), Some("bonjour")), "{ev}");
    assert_eq!(post("/api/v1/account/sign".into(), json!({ "kind": 1 }), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(post("/api/v1/account/sign".into(), json!({ "kind": 30800, "content": "{}" }), Some(session)).await.0, StatusCode::BAD_REQUEST);

    // Quatre échecs : 401 avec les essais restants ; le cinquième verrouille (429), même pour le bon code.
    for _ in 0..4 {
        assert_eq!(post("/api/v1/account/unlock".into(), json!({ "email": "alice@example.org", "pass": "000000" }), None).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(post("/api/v1/account/unlock".into(), json!({ "email": "alice@example.org", "pass": "000000" }), None).await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(post("/api/v1/account/unlock".into(), json!({ "email": "alice@example.org", "pass": "123456" }), None).await.0, StatusCode::TOO_MANY_REQUESTS);
    // Le Capitaine voit le verrou et le lève ; un simple membre ne le peut pas.
    let (_, list) = api(&b.app, Method::GET, "/api/v1/station/accounts", &bob, None).await;
    let row = list["accounts"].as_array().unwrap().iter().find(|a| a["email"] == "alice@example.org").unwrap().clone();
    assert!(row["pass_locked_until"].as_u64().is_some(), "{row}");
    let url = format!("/api/v1/station/accounts/{}/unlock-pass", hex(&ALICE));
    assert_eq!(post(url.clone(), json!({}), Some(alice)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(post(url, json!({}), Some(bob)).await.0, StatusCode::OK);
    assert_eq!(post("/api/v1/account/unlock".into(), json!({ "email": "alice@example.org", "pass": "123456" }), None).await.0, StatusCode::OK);
    // Mal formé : refusé avant tout essai.
    assert_eq!(post("/api/v1/account/unlock".into(), json!({ "email": "alice@example.org", "pass": "12" }), None).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_guest_registers_and_claims_an_invitation_without_an_email() {
    let b = bench("guest", None).await;
    let post = |uri: String, body: Value| {
        let app = b.app.clone();
        async move {
            let (s, _, raw) = call(&app, Method::POST, &uri, None, &[("content-type", "application/json")], body.to_string().into_bytes()).await;
            (s, serde_json::from_slice::<Value>(&raw).unwrap_or(Value::Null))
        }
    };
    let (s, card) = post("/api/v1/register".into(), json!({ "guest": true, "alias": "Léa", "lang": "fr" })).await;
    let email = card["email"].as_str().unwrap().to_owned();
    assert_eq!((s, card["guest"].as_bool(), card["pass"].as_str().map(str::len)), (StatusCode::OK, Some(true), Some(6)), "{card}");
    assert!(email.starts_with("lea-") && email.ends_with("@station.test") || email.ends_with("@uplanet.local"), "{email}");
    // Le PASS remis à la création ouvre la session de cet invité.
    let (s, ok) = post("/api/v1/account/unlock".into(), json!({ "email": email, "pass": card["pass"] })).await;
    assert_eq!((s, ok["guest"].as_bool()), (StatusCode::OK, Some(true)), "{ok}");

    // Une invitation se réclame aussi sans email.
    let alice = basic("alice@example.org", &ALICE);
    put(&b.app, &alice, "/Photos/a.png", &png_bytes()).await;
    let (_, made) = api(&b.app, Method::POST, "/api/v1/invites", &alice, Some(json!({ "paths": ["/Photos"] }))).await;
    let token = made["token"].as_str().unwrap();
    let (s, claimed) = post(format!("/api/v1/invite/{token}/claim"), json!({ "guest": true, "alias": "Max" })).await;
    assert_eq!((s, claimed["guest"].as_bool(), claimed["shared"].as_u64()), (StatusCode::OK, Some(true), Some(1)), "{claimed}");
    assert!(claimed["email"].as_str().unwrap().starts_with("max-"));
}

#[tokio::test]
async fn a_logged_in_member_accepts_an_invitation_and_errors_carry_stable_codes() {
    let b = bench("accept", None).await;
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    put(&b.app, &alice, "/Photos/a.png", &png_bytes()).await;
    let (_, made) = api(&b.app, Method::POST, "/api/v1/invites", &alice, Some(json!({ "paths": ["/Photos"] }))).await;
    let token = made["token"].as_str().unwrap().to_owned();
    let uri = format!("/api/v1/invite/{token}/accept");
    // Il faut une session ; l'expéditeur ne peut pas accepter son propre lien.
    assert_eq!(api(&b.app, Method::POST, &uri, "Basic nope", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(api(&b.app, Method::POST, &uri, &alice, None).await.0, StatusCode::BAD_REQUEST);
    // Bob accepte : aucune création de compte, la photo arrive chez lui.
    let (s, ok) = api(&b.app, Method::POST, &uri, &bob, None).await;
    assert_eq!((s, ok["from"].as_str(), ok["shared"].as_u64()), (StatusCode::OK, Some("alice"), Some(1)), "{ok}");
    let (_, v) = api(&b.app, Method::GET, "/api/v1/ucloud/list?path=/Partages/De_alice", &bob, None).await;
    assert_eq!(v["entries"].as_array().map(Vec::len), Some(1), "{v}");
    // Lien consommé : 410 avec un code stable, que la page traduit ; un lien inconnu : 404.
    let (s, again) = api(&b.app, Method::POST, &uri, &bob, None).await;
    assert_eq!((s, again["code"].as_str()), (StatusCode::GONE, Some("invite_used")), "{again}");
    let (s, gone) = {
        let (s, _, raw) = call(&b.app, Method::GET, &format!("/api/v1/invite/{token}"), None, &[], vec![]).await;
        (s, serde_json::from_slice::<Value>(&raw).unwrap())
    };
    assert_eq!((s, gone["code"].as_str()), (StatusCode::GONE, Some("invite_used")));
    // Une adresse déjà inscrite ici : 409 « email_taken » sur une invitation neuve.
    let (_, made) = api(&b.app, Method::POST, "/api/v1/invites", &alice, Some(json!({ "paths": ["/Photos"] }))).await;
    let t2 = made["token"].as_str().unwrap();
    let (s, _, raw) = call(&b.app, Method::POST, &format!("/api/v1/invite/{t2}/claim"), None, &[("content-type", "application/json")], json!({ "email": "alice@example.org" }).to_string().into_bytes()).await;
    let taken: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!((s, taken["code"].as_str()), (StatusCode::CONFLICT, Some("email_taken")), "{taken}");
}

/// Sert le routeur d'une station de test sur un port local et rend son origine (`http://127.0.0.1:port`).
async fn serve_local(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    origin
}

#[tokio::test]
async fn an_invitation_from_another_station_is_accepted_with_a_signature_made_by_the_recipients_station() {
    // A : la station d'Alice (expéditrice). B : celle de Bob, qui n'y est pas hébergé.
    let a = bench("remote-a", None).await;
    let b = bench("remote-b", None).await;
    a.db.writer().execute("UPDATE identities SET hosted = 0 WHERE hex = ?1", vec![V::Text(hex(&BOB))]).await.unwrap();
    b.db.writer().execute("UPDATE identities SET hosted = 0 WHERE hex = ?1", vec![V::Text(hex(&ALICE))]).await.unwrap();
    let (alice, bob) = (basic("alice@example.org", &ALICE), basic("bob@example.org", &BOB));
    put(&a.app, &alice, "/Photos/a.png", &png_bytes()).await;
    let (_, made) = api(&a.app, Method::POST, "/api/v1/invites", &alice, Some(json!({ "paths": ["/Photos"], "label": "Bob" }))).await;
    let token = made["token"].as_str().unwrap().to_owned();
    let origin = serve_local(a.app.clone()).await;

    // L'aperçu public ne donne pas la clé de l'expéditeur ; la station de Bob la reçoit, en signant sa demande.
    let (_, _, raw) = call(&a.app, Method::GET, &format!("/api/v1/invite/{token}"), None, &[], vec![]).await;
    assert!(!String::from_utf8_lossy(&raw).contains(&hex(&ALICE)));

    // Sans session sur sa propre station, rien ne part ; une adresse en clair ou un jeton inconnu sont refusés.
    let go = |body: Value| { let (app, bob) = (b.app.clone(), bob.clone()); async move { api(&app, Method::POST, "/api/v1/invites/remote-accept", &bob, Some(body)).await } };
    assert_eq!(api(&b.app, Method::POST, "/api/v1/invites/remote-accept", "Basic nope", Some(json!({ "origin": origin, "token": token }))).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(go(json!({ "origin": "ftp://x", "token": token })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(go(json!({ "origin": origin, "token": "f".repeat(32) })).await.0, StatusCode::GONE, "lien inconnu de la station d'origine");

    let (s, ok) = go(json!({ "origin": origin, "token": token })).await;
    assert_eq!((s, ok["from"].as_str()), (StatusCode::OK, Some("alice")), "{ok}");

    // Station A : le lien est utilisé par Bob ; Alice le suit et détient sa liste de contacts signée.
    let (claimed_by, follows): (Option<String>, i64) = a.db.readers().read({ let (t, bob_hex) = (token.clone(), hex(&BOB)); move |c| Ok((
        c.query_row("SELECT claimed_by FROM invites WHERE token = ?1", [&t], |r| r.get(0))?,
        c.query_row("SELECT COUNT(*) FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", [hex(&ALICE), bob_hex], |r| r.get(0))?,
    )) }).await.unwrap();
    assert_eq!((claimed_by, follows), (Some(hex(&BOB)), 1));
    // Station B : Bob suit Alice, et la liste de contacts d'Alice (qui le suit) est rangée AVANT toute photo : sans elle, ShareInbox
    // écarterait l'enveloppe.
    let (bob_follows, alice_follows_bob): (i64, i64) = b.db.readers().read({ let bob_hex = hex(&BOB); move |c| Ok((
        c.query_row("SELECT COUNT(*) FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", [&bob_hex, &hex(&ALICE)], |r| r.get(0))?,
        c.query_row("SELECT COUNT(*) FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.id WHERE e.kind = 3 AND e.pubkey = ?1 AND t.name = 'p' AND t.value = ?2", [hex(&ALICE), bob_hex], |r| r.get(0))?,
    )) }).await.unwrap();
    assert_eq!((bob_follows, alice_follows_bob), (1, 1));

    // Usage unique, même à distance.
    let (s, again) = go(json!({ "origin": origin, "token": token })).await;
    assert_eq!((s, again["code"].as_str()), (StatusCode::GONE, Some("invite_gone")), "{again}");

    // L'envoi est réservé à celui qui a accepté, pendant un temps limité ; la signature est liée à l'URL, au corps, et à usage unique.
    let url = format!("http://station.test/api/v1/invite/{token}/deliver");
    let sign = |secret: &[u8; 32], url: &str, body: &str| astro_crypto::nip98::authorization_header(secret, "POST", url, Some(body.as_bytes()), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()).unwrap();
    let post = |url: String, auth: String, body: String| { let app = a.app.clone(); async move {
        let path = url.strip_prefix("http://station.test").unwrap().to_owned();
        let (s, _, raw) = call(&app, Method::POST, &path, Some(&auth), &[("content-type", "application/json")], body.into_bytes()).await;
        (s, serde_json::from_slice::<Value>(&raw).unwrap_or(Value::Null))
    } };
    let h = sign(&ALICE, &url, "{}");
    assert_eq!(post(url.clone(), h, "{}".into()).await.0, StatusCode::BAD_REQUEST, "pas celui qui a accepté");
    let h = sign(&BOB, &url, "{}");
    let (s, _) = post(url.clone(), h.clone(), "{\"x\":1}".into()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "le corps ne correspond pas à la signature");
    // Signature valide de celui qui a accepté : autorisé (ici, sans maillage configuré, la station le dit) ; rejouée : refusée.
    assert_eq!(post(url.clone(), h.clone(), "{}".into()).await.0, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(post(url, h, "{}".into()).await.0, StatusCode::UNAUTHORIZED, "une signature ne sert qu'une fois");
}
