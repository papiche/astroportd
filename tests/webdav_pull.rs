//! Reprise depuis un autre serveur WebDAV (mode « pull ») contre un faux Nextcloud : arborescence reprise telle quelle,
//! passage rejouable (même après un déplacement par le rangement), serveur hostile (chemins hors dossier, noms piégés,
//! redirection vers un autre serveur), sources chiffrées, adresses privées refusées, service en arrière-plan.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::ActivityBus;
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher};
use astro_db::{Db, SqliteReplayStore};
use astroportd::webdav_pull::{self as pull, Draft, PullOptions, PullService, Source, Sources};
use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use rusqlite::types::Value as V;

const ALICE: [u8; 32] = [0xA1; 32];
const ROOT: &str = "/remote.php/dav/files/fred";
const PASS: &str = "mot-de-passe-d-application";

fn hex(s: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(s).unwrap()
}

struct Fake {
    files: Vec<(String, Vec<u8>)>,
    hostile: bool,
    other: String,
    auth_seen: Mutex<Vec<bool>>,
}

fn enc(seg: &str) -> String {
    utf8_percent_encode(seg, NON_ALPHANUMERIC).to_string()
}

fn entry(href: &str, dir: bool, size: usize, mime: &str) -> String {
    let kind = if dir { "<d:resourcetype><d:collection/></d:resourcetype>".to_owned() } else { format!("<d:resourcetype/><d:getcontentlength>{size}</d:getcontentlength><d:getcontenttype>{mime}</d:getcontenttype>") };
    format!("<d:response><d:href>{href}</d:href><d:propstat><d:prop>{kind}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>")
}

async fn serve(State(f): State<Arc<Fake>>, req: Request) -> Response {
    let want = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("fred:{PASS}")));
    if req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some(want.as_str()) {
        return (StatusCode::UNAUTHORIZED, "no").into_response();
    }
    let path = percent_decode_str(req.uri().path()).decode_utf8_lossy().into_owned();
    let Some(rel) = path.strip_prefix(ROOT) else { return StatusCode::NOT_FOUND.into_response() };
    let rel = rel.trim_end_matches('/').to_owned();
    if req.method().as_str() == "PROPFIND" {
        let prefix = format!("{}/", rel.trim_start_matches('/'));
        let prefix = if rel.is_empty() { String::new() } else { prefix };
        let (mut dirs, mut files) = (Vec::<String>::new(), Vec::new());
        for (p, body) in &f.files {
            let Some(rest) = p.strip_prefix(&prefix) else { continue };
            match rest.split_once('/') {
                Some((d, _)) => {
                    if !dirs.contains(&d.to_owned()) {
                        dirs.push(d.to_owned());
                    }
                }
                None => files.push((rest.to_owned(), body.len())),
            }
        }
        if dirs.is_empty() && files.is_empty() && !rel.is_empty() && !f.files.iter().any(|(p, _)| p.starts_with(&prefix)) {
            return StatusCode::NOT_FOUND.into_response();
        }
        let base: String = format!("{ROOT}{}", rel.split('/').filter(|s| !s.is_empty()).map(|s| format!("/{}", enc(s))).collect::<String>());
        let mut xml = String::from(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">"#);
        xml += &entry(&format!("{base}/"), true, 0, "");
        for d in &dirs {
            xml += &entry(&format!("{base}/{}/", enc(d)), true, 0, "");
        }
        for (n, size) in &files {
            xml += &entry(&format!("{base}/{}", enc(n)), false, *size, if n.ends_with(".jpg") { "image/jpeg" } else { "application/octet-stream" });
        }
        if f.hostile && rel == "/Photos" {
            // Un serveur qui désigne des chemins hors du dossier demandé, ou des noms piégés.
            xml += &entry(&format!("{base}/../../../../etc/passwd"), false, 3, "text/plain");
            xml += &entry("/remote.php/dav/files/autre/secret.jpg", false, 3, "image/jpeg");
            xml += &entry(&format!("{base}/%2e%2e"), false, 3, "text/plain");
            xml += &entry(&format!("{base}/a%5Cb.jpg"), false, 3, "image/jpeg");
            xml += &entry(&format!("{base}/fuite.jpg"), false, 3, "image/jpeg");
        }
        xml += "</d:multistatus>";
        return Response::builder().status(207).header(header::CONTENT_TYPE, "application/xml").body(Body::from(xml)).unwrap();
    }
    if rel.ends_with("/fuite.jpg") {
        return Response::builder().status(302).header(header::LOCATION, format!("{}/vole", f.other)).body(Body::empty()).unwrap();
    }
    match f.files.iter().find(|(p, _)| format!("/{p}") == rel) {
        Some((_, body)) => Response::builder().status(200).header(header::CONTENT_TYPE, "application/octet-stream").body(Body::from(body.clone())).unwrap(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn spy(State(f): State<Arc<Fake>>, req: Request) -> Response {
    f.auth_seen.lock().unwrap().push(req.headers().contains_key(header::AUTHORIZATION));
    StatusCode::OK.into_response()
}

async fn listen(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

struct Bench {
    dav: Arc<DavState<FsBlobStore>>,
    keys: Arc<dyn KeySource>,
    dir: std::path::PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn bench(tag: &str) -> Bench {
    let dir = std::env::temp_dir().join(format!("astroportd-pull-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@example.org', 1)", vec![V::Text(hex(&ALICE))]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys.clone(), Duration::from_secs(3600));
    let dav = Arc::new(DavState { db, blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config: DavConfig::new(b"secret-de-station-pour-les-tests".to_vec()), on_upload: None });
    Bench { dav, keys, dir }
}

async fn nextcloud(hostile: bool) -> (String, Arc<Fake>, String) {
    let files: Vec<(String, Vec<u8>)> = vec![
        ("Photos/a.jpg".into(), b"photo A".to_vec()),
        ("Photos/2003/06/b.jpg".into(), b"photo B, juin 2003".to_vec()),
        ("Photos/notes d'été.txt".into(), "à relire".as_bytes().to_vec()),
        ("Musique/morceau.mp3".into(), vec![7; 2000]),
    ];
    let probe = Arc::new(Fake { files: Vec::new(), hostile: false, other: String::new(), auth_seen: Mutex::new(Vec::new()) });
    let other = listen(Router::new().fallback(axum::routing::any(spy)).with_state(probe.clone())).await;
    let fake = Arc::new(Fake { files, hostile, other, auth_seen: Mutex::new(Vec::new()) });
    let url = listen(Router::new().fallback(axum::routing::any(serve)).with_state(fake.clone())).await;
    (format!("{url}{ROOT}"), probe, "x".into())
}

fn source(url: &str, remote: &str, dest: &str) -> Source {
    Source { id: "src1".into(), label: "Nextcloud d'essai".into(), url: url.into(), username: "fred".into(), password: PASS.into(), remote_path: remote.into(), dest_prefix: dest.into(), allow_private: true, created_at: 0 }
}

async fn paths(b: &Bench) -> Vec<String> {
    b.dav.db.readers().read(|c| Ok(c.prepare("SELECT path FROM ucloud_entries WHERE type = 'file' ORDER BY path")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nextcloud_tree_is_pulled_as_is_and_the_pass_can_be_replayed_even_after_a_move() {
    let b = bench("tree").await;
    let (url, _, _) = nextcloud(false).await;
    let src = source(&url, "Photos", "/Imports/Nextcloud");
    let owner = hex(&ALICE);

    // À blanc : annonce, n'écrit rien.
    let r = pull::pull(&b.dav, &owner, &src, &PullOptions { dry_run: true, ..PullOptions::default() }, |_| {}).await.unwrap();
    assert_eq!((r.remote, r.imported, r.skipped, r.errors.len()), (3, 3, 0, 0), "{r:?}");
    assert!(paths(&b).await.is_empty(), "rien n'est écrit à blanc");

    // Reprise : même arborescence sous le dossier de dépôt, aucun tri par dates.
    let mut steps = Vec::new();
    let r = pull::pull(&b.dav, &owner, &src, &PullOptions::default(), |p| steps.push(p)).await.unwrap();
    assert_eq!((r.imported, r.skipped, r.errors.len()), (3, 0, 0), "{r:?}");
    assert_eq!(paths(&b).await, ["/Imports/Nextcloud/2003/06/b.jpg", "/Imports/Nextcloud/a.jpg", "/Imports/Nextcloud/notes d'été.txt"]);
    assert!(steps.first().is_some_and(|p| p.done == 0 && p.total == 3) && steps.last().is_some_and(|p| p.done == 3 && p.imported == 3), "progression : {steps:?}");
    let origin: String = b.dav.db.readers().read(|c| Ok(c.query_row("SELECT origin FROM ucloud_entries WHERE path = '/Imports/Nextcloud/a.jpg'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(origin, "pull:src1", "la provenance est notée sur l'entrée");
    let size: i64 = b.dav.db.readers().read(|c| Ok(c.query_row("SELECT size FROM ucloud_entries WHERE path = '/Imports/Nextcloud/a.jpg'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(size, 7);

    // Rejeu : rien de repris, rien de dupliqué.
    let r = pull::pull(&b.dav, &owner, &src, &PullOptions::default(), |_| {}).await.unwrap();
    assert_eq!((r.imported, r.skipped, r.changed), (0, 3, 0), "{r:?}");

    // Le rangement déplace un fichier ailleurs : le passage suivant ne le reprend pas.
    b.dav.db.writer().execute("UPDATE ucloud_entries SET path = '/Photos/2003/b.jpg', parent = '/Photos/2003' WHERE path = '/Imports/Nextcloud/2003/06/b.jpg'", vec![]).await.unwrap();
    let r = pull::pull(&b.dav, &owner, &src, &PullOptions::default(), |_| {}).await.unwrap();
    assert_eq!((r.imported, r.skipped), (0, 3), "un fichier rangé ailleurs n'est pas repris en double : {r:?}");
    assert_eq!(paths(&b).await.len(), 3);

    // Un fichier repris, puis modifié sur le serveur : signalé, jamais dupliqué ni écrasé.
    let mut grown = src.clone();
    grown.remote_path = "Photos".into();
    b.dav.db.writer().execute("UPDATE pull_imported SET size = 1 WHERE remote_path = 'a.jpg'", vec![]).await.unwrap();
    let r = pull::pull(&b.dav, &owner, &grown, &PullOptions::default(), |_| {}).await.unwrap();
    assert_eq!((r.imported, r.changed), (0, 1), "{r:?}");

    // Une autre source du même serveur (dossier Musique) ne se confond pas avec la première.
    let mut music = source(&url, "Musique", "/Imports/Musique");
    music.id = "src2".into();
    let r = pull::pull(&b.dav, &owner, &music, &PullOptions { limit: 1, ..PullOptions::default() }, |_| {}).await.unwrap();
    assert_eq!(r.imported, 1);
    assert!(paths(&b).await.contains(&"/Imports/Musique/morceau.mp3".to_owned()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hostile_server_cannot_write_outside_the_destination_nor_receive_the_credentials() {
    let b = bench("hostile").await;
    let (url, probe, _) = nextcloud(true).await;
    let src = source(&url, "Photos", "/Imports/Hostile");
    let r = pull::pull(&b.dav, &hex(&ALICE), &src, &PullOptions::default(), |_| {}).await.unwrap();
    // Les trois vrais fichiers sont repris ; les chemins hors dossier, `..` et `\` sont ignorés ; la redirection est refusée.
    let got = paths(&b).await;
    assert!(got.iter().all(|p| p.starts_with("/Imports/Hostile/")), "rien hors du dossier de dépôt : {got:?}");
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(!got.iter().any(|p| p.contains("passwd") || p.contains("secret") || p.contains("..")), "{got:?}");
    assert_eq!(r.errors.len(), 1, "seule la redirection est un échec, et il est dit : {:?}", r.errors);
    assert!(r.errors[0].1.contains("autre serveur") && r.errors[0].0 == "fuite.jpg", "{:?}", r.errors);
    assert!(probe.auth_seen.lock().unwrap().is_empty(), "l'autre serveur n'a reçu aucune requête portant les identifiants");

    // Mauvais mot de passe : refus clair dès le premier niveau, rien n'est écrit.
    let mut bad = source(&url, "Photos", "/Imports/Autre");
    bad.password = "faux".into();
    let e = pull::pull(&b.dav, &hex(&ALICE), &bad, &PullOptions::default(), |_| {}).await.unwrap_err();
    assert!(e.to_string().contains("identifiants refusés"), "{e}");
    // Dossier distant absent.
    let e = pull::check(&source(&url, "Inexistant", "/Imports/Autre")).await.unwrap_err();
    assert!(e.to_string().contains("introuvable"), "{e}");
    // Plafond par fichier.
    let r = pull::pull(&b.dav, &hex(&ALICE), &source(&url, "Musique", "/Imports/Gros"), &PullOptions { max_file: 100, ..PullOptions::default() }, |_| {}).await.unwrap();
    assert_eq!((r.too_large, r.imported), (1, 0), "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sources_are_stored_encrypted_and_the_interface_never_reaches_private_addresses() {
    let b = bench("sources").await;
    let sources = Sources::new(b.dav.db.clone(), b.keys.clone());
    let owner = hex(&ALICE);
    let saved = sources.add(&owner, source("https://nextcloud.example/remote.php/dav/files/fred/", "/Photos/", "/Imports/N")).await.unwrap();
    assert_eq!(saved.id.len(), 16);
    let raw: String = b.dav.db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'webdav_sources'", [], |r| r.get(0))?)).await.unwrap();
    assert!(!raw.contains(PASS) && !raw.contains("nextcloud.example") && !raw.contains("fred"), "rangées chiffrées pour leur titulaire seul");
    let back = sources.list(&owner).await.unwrap();
    assert_eq!((back[0].password.as_str(), back[0].url.as_str(), back[0].remote_path.as_str()), (PASS, "https://nextcloud.example/remote.php/dav/files/fred", "Photos"));

    // Saisies refusées.
    for bad in [
        Source { url: "ftp://x.example/dav".into(), ..source("", "", "/Imports/X") },
        Source { url: "https://u:p@x.example/dav".into(), ..source("", "", "/Imports/X") },
        Source { remote_path: "a/../../b".into(), ..source("https://x.example/dav", "", "/Imports/X") },
        source("https://x.example/dav", "", "/"),
        source("https://x.example/dav", "", "/Coffre/vol"),
        Source { username: "a:b".into(), ..source("https://x.example/dav", "", "/Imports/X") },
    ] {
        assert!(sources.add(&owner, bad.clone()).await.is_err(), "{bad:?}");
    }
    sources.remove(&owner, &saved.id).await.unwrap();
    assert!(sources.list(&owner).await.unwrap().is_empty());

    // Depuis l'interface (le service), une adresse privée ou locale est refusée : jamais d'`allow_private` par ce chemin.
    let service = PullService::new(b.dav.clone(), b.keys.clone(), None);
    for private in ["http://127.0.0.1:9/remote.php/dav/files/fred", "http://192.168.1.10/dav", "http://169.254.169.254/latest", "http://localhost/dav"] {
        let e = service.add(&owner, Draft { url: private.into(), username: "fred".into(), password: PASS.into(), ..Draft::default() }).await.unwrap_err();
        assert!(e.to_string().contains("refusée") || e.to_string().contains("résolution") || e.to_string().contains("adresse"), "{private} : {e}");
    }
    assert!(sources.list(&owner).await.unwrap().is_empty(), "une source refusée n'est pas enregistrée");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_service_runs_in_the_background_reports_progress_and_allows_one_pass_per_account() {
    let b = bench("service").await;
    let (url, _, _) = nextcloud(false).await;
    let owner = hex(&ALICE);
    let bus = ActivityBus::new();
    let service = PullService::new(b.dav.clone(), b.keys.clone(), Some(bus.clone()));
    // La station enregistre la source (adresse privée admise : le faux serveur est local) comme le ferait la ligne de commande.
    let saved = Sources::new(b.dav.db.clone(), b.keys.clone()).add(&owner, source(&url, "Photos", "/Imports/Service")).await.unwrap();

    let listed = service.list(&owner).await.unwrap();
    assert_eq!(listed["sources"][0]["job"], serde_json::Value::Null);
    assert!(!listed.to_string().contains(PASS), "le mot de passe ne sort jamais");
    assert!(service.run(&owner, "inconnue", false).await.is_err());
    assert!(service.stop(&owner).is_err(), "aucune reprise à arrêter");

    service.run(&owner, &saved.id, false).await.unwrap();
    assert!(service.run(&owner, &saved.id, false).await.is_err(), "une seule reprise à la fois par compte");
    assert!(service.remove(&owner, &saved.id).await.is_err() || service.list(&owner).await.unwrap()["sources"][0]["job"]["state"] != "running", "pas de retrait pendant la reprise");
    let mut last = serde_json::Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = service.list(&owner).await.unwrap()["sources"][0].clone();
        if last["job"]["state"] == "done" {
            break;
        }
    }
    assert_eq!(last["job"]["state"], "done", "{last}");
    assert_eq!((last["job"]["report"]["imported"].as_u64(), last["job"]["report"]["errors"].as_u64(), last["job"]["progress"]["total"].as_u64()), (Some(3), Some(0), Some(3)), "{last}");
    assert_eq!(last["last"]["report"]["imported"], 3, "le bilan survit au passage : {last}");
    assert_eq!(paths(&b).await.len(), 3);
    let said: Vec<_> = bus.recent(10).into_iter().filter(|a| a.kind == "pull").collect();
    assert!(said.len() == 1 && said[0].message.contains("3 fichier(s) repris"), "{said:?}");

    // Un second passage, à blanc, n'a plus rien à reprendre.
    service.run(&owner, &saved.id, true).await.unwrap();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = service.list(&owner).await.unwrap()["sources"][0].clone();
        if last["job"]["state"] == "done" {
            break;
        }
    }
    assert_eq!((last["job"]["dry_run"].as_bool(), last["job"]["report"]["imported"].as_u64(), last["job"]["report"]["skipped"].as_u64()), (Some(true), Some(0), Some(3)), "{last}");
    let _: HashMap<(), ()> = HashMap::new();
    service.remove(&owner, &saved.id).await.unwrap();
}
