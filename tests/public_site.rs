//! Site public d'un compte (`/Public`) de bout en bout : un fichier déposé dans `/Public` est servi en clair sans authentification,
//! un autre dossier de l'uCloud ne l'est jamais, et chaque réponse porte la politique qui isole le code du membre.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use astro_bro::publisher::Publisher;
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher, ingest};
use astro_db::{Db, SqliteReplayStore};
use astro_nostr_proto::Event;
use astroportd::public_site::{Site, name_of};
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use tower::ServiceExt;

const ALICE: [u8; 32] = [0xA1; 32];
const EMAIL: &str = "alice.martin@exemple.org";
const UP: &str = "UPlanetTestName";

struct Store(Arc<Db>);

impl Publisher for Store {
    async fn publish(&self, event: Event) -> Result<(), String> {
        self.0.writer().transaction(move |tx| astro_db::events::insert_event(tx, &event).map(|_| ())).await.map_err(|e| e.to_string())
    }
}

struct Rig {
    dir: std::path::PathBuf,
    db: Arc<Db>,
    dav: Arc<DavState<FsBlobStore>>,
    site: Arc<Site<FsBlobStore>>,
    hex: String,
    name: String,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn rig(tag: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("astroportd-site-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    let hex = astro_nostr_proto::pubkey_of(&ALICE).unwrap();
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex.clone().into(), EMAIL.to_owned().into()]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys, Duration::from_secs(3600));
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config: DavConfig::new(b"secret-de-station-pour-les-tests".to_vec()), on_upload: None });
    let site = Arc::new(Site::new(dav.clone(), UP.to_owned()));
    Rig { dir, db, dav, site, hex, name: name_of(EMAIL) }
}

async fn put(r: &Rig, dir: &str, name: &str, mime: &str, text: &str) {
    ingest::replace(&r.dav, &r.hex, dir, name, Some(mime), Cursor::new(text.as_bytes().to_vec())).await.unwrap();
}

async fn get(r: &Rig, path: &str) -> (StatusCode, HeaderMap, String) {
    let resp = r.site.clone().router().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn a_file_in_public_is_served_in_clear_to_anyone_and_the_rest_of_the_ucloud_never_is() {
    let r = rig("serve").await;
    put(&r, "/Public", "index.html", "text/html", "<h1>Bonjour</h1><script>1</script>").await;
    put(&r, "/Public", "style.css", "text/css", "body{}").await;
    put(&r, "/Public/photos", "a.txt", "text/plain", "dans un sous-dossier").await;
    put(&r, "/Public", ".cache", "text/plain", "caché").await;
    put(&r, "/Photos", "prive.txt", "text/plain", "SECRET PRIVÉ").await;

    let (s, h, body) = get(&r, &format!("/site/{}/", r.name)).await;
    assert_eq!((s, body.as_str()), (StatusCode::OK, "<h1>Bonjour</h1><script>1</script>"), "index.html en clair, sans authentification");
    let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.starts_with("sandbox ") && !csp.contains("allow-same-origin"), "le code du membre tourne en origine opaque : {csp}");
    assert_eq!((h[header::X_CONTENT_TYPE_OPTIONS].to_str().unwrap(), h[header::REFERRER_POLICY].to_str().unwrap()), ("nosniff", "no-referrer"));
    assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    let (s, h, body) = get(&r, &format!("/site/{}/style.css", r.name)).await;
    assert_eq!((s, body.as_str(), h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "body{}", "text/css"));
    assert_eq!(get(&r, &format!("/site/{}/photos/a.txt", r.name)).await.2, "dans un sous-dossier");
    assert_eq!(get(&r, &format!("/site/{}", r.name)).await.0, StatusCode::PERMANENT_REDIRECT, "le slash final garde les liens relatifs");
    assert_eq!(get(&r, &format!("/SITE/{}/", r.name)).await.0, StatusCode::NOT_FOUND);

    // Ce qui n'est pas dans /Public n'est jamais lisible, quelle que soit la façon de le demander.
    for path in ["/site/NAME/../Photos/prive.txt", "/site/NAME/..%2FPhotos%2Fprive.txt", "/site/NAME/%2e%2e/Photos/prive.txt", "/site/NAME/Photos/prive.txt", "/site/NAME/.cache", "/site/NAME/photos/../.cache"] {
        let (s, _, body) = get(&r, &path.replace("NAME", &r.name)).await;
        assert!(s == StatusCode::NOT_FOUND || s == StatusCode::PERMANENT_REDIRECT || s == StatusCode::BAD_REQUEST, "{path} → {s}");
        assert!(!body.contains("SECRET") && !body.contains("caché"), "{path}");
    }
    assert_eq!(get(&r, "/site/personne.exemple.org/").await.0, StatusCode::NOT_FOUND, "nom inconnu");
}

#[tokio::test]
async fn without_an_index_the_station_shows_a_default_page_with_the_profile_and_the_files() {
    let r = rig("default").await;
    let (s, _, body) = get(&r, &format!("/site/{}/", r.name)).await;
    assert!(s == StatusCode::OK && body.contains("Rien n'est publié"), "pas encore de /Public : page par défaut");
    let profile = serde_json::json!({ "name": "Alice <b>", "about": "Boulangère & rêveuse", "picture": "https://exemple.org/a.jpg" });
    let ev = astro_nostr_proto::UnsignedEvent { created_at: 5, kind: 0, tags: vec![], content: profile.to_string() }.sign(&ALICE).unwrap();
    Store(r.db.clone()).publish(ev).await.unwrap();
    put(&r, "/Public", "recette.pdf", "application/pdf", "%PDF").await;
    put(&r, "/Public/photos", "pain.jpg", "image/jpeg", "jpeg").await;
    let (_, _, body) = get(&r, &format!("/site/{}/", r.name)).await;
    assert!(body.contains("Alice &lt;b&gt;") && body.contains("Boulangère &amp; rêveuse") && body.contains("https://exemple.org/a.jpg"), "profil échappé : {body}");
    assert!(body.contains(&format!("/site/{}/recette.pdf", r.name)) && body.contains(&format!("/site/{}/photos/", r.name)));
    let (_, _, sub) = get(&r, &format!("/site/{}/photos/", r.name)).await;
    assert!(sub.contains("pain.jpg"), "liste d'un sous-dossier : {sub}");
}

#[tokio::test]
async fn the_example_site_is_installed_once_and_never_overwrites_a_homepage() {
    let r = rig("example").await;
    let w = r.site.install_example(&r.hex, false).await.unwrap();
    assert_eq!(w, ["/Public/index.html", "/Public/style.css", "/Public/LISEZMOI.txt"]);
    let (s, _, body) = get(&r, &format!("/site/{}/", r.name)).await;
    assert!(s == StatusCode::OK && body.contains("<h1>Bonjour") && body.contains("style.css"));
    put(&r, "/Public", "index.html", "text/html", "ma page à moi").await;
    assert!(r.site.install_example(&r.hex, false).await.unwrap_err().contains("existe déjà"));
    assert_eq!(get(&r, &format!("/site/{}/", r.name)).await.2, "ma page à moi", "la page du membre est intacte");
    r.site.install_example(&r.hex, true).await.unwrap();
    assert!(get(&r, &format!("/site/{}/", r.name)).await.2.contains("<h1>Bonjour"), "force : on repart de l'exemple");
    let info = r.site.info(&r.hex, EMAIL).await;
    assert_eq!((info["name"].as_str(), info["has_index"].as_bool(), info["path_url"].as_str()), (Some(r.name.as_str()), Some(true), Some(format!("/site/{}/", r.name).as_str())));
    assert!(info["host_url"].is_null(), "pas de zone DNS : pas d'adresse par nom d'hôte");
}

#[tokio::test]
async fn a_host_name_of_the_form_name_dot_zone_gets_the_site() {
    let r = rig("host").await;
    put(&r, "/Public", "index.html", "text/html", "accueil par nom d'hôte").await;
    let headers = HeaderMap::new();
    assert!(r.site.by_host(&format!("{}.coop.net", r.name), "/", &headers, false).await.is_none(), "sans zone configurée, aucun nom d'hôte n'est reconnu");
    let mut patch = BTreeMap::new();
    patch.insert("OVH_ZONE".to_owned(), Some("coop.net".to_owned()));
    astroportd::coop_config::save(&r.db, &Arc::new(Store(r.db.clone())), UP, &patch).await.unwrap();
    let site = Arc::new(Site::new(r.dav.clone(), UP.to_owned())); // le cache de zone se renouvelle chaque minute : un site neuf relit tout de suite
    let resp = site.by_host(&format!("{}.coop.net:443", r.name), "/", &headers, false).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    assert!(site.by_host("station.coop.net", "/", &headers, false).await.is_none(), "nom d'hôte de la station : pas un site");
    assert!(site.by_host(&format!("{}.autre.org", r.name), "/", &headers, false).await.is_none(), "autre zone");
    assert!(site.by_host("inconnu.coop.net", "/", &headers, false).await.is_none());
    assert_eq!(site.info(&r.hex, EMAIL).await["host_url"].as_str(), Some(format!("https://{}.coop.net/", r.name).as_str()));
}

#[tokio::test]
async fn publishing_is_a_move_into_public_and_withdrawing_is_a_move_out() {
    let r = rig("move").await;
    put(&r, "/Photos", "pain.jpg", "image/jpeg", "jpeg").await;
    put(&r, "/Photos", "autre.jpg", "image/jpeg", "jpeg2").await;
    put(&r, "/Docs", "notes.txt", "text/plain", "privées").await;
    let url = format!("/site/{}/pain.jpg", r.name);
    assert_eq!(get(&r, &url).await.0, StatusCode::NOT_FOUND, "pas publié");

    let out = r.site.set_public(&r.hex, &["/Photos/pain.jpg".into(), "/Photos/autre.jpg".into(), "/Photos/absent.jpg".into()], true).await;
    assert_eq!(out["moved"].as_array().unwrap().len(), 2, "{out}");
    assert_eq!(out["refused"].as_array().unwrap().len(), 1, "un fichier absent est refusé sans bloquer les autres");
    assert_eq!(get(&r, &url).await.2, "jpeg", "le fichier déplacé est public");
    let (hex, db) = (r.hex.clone(), r.db.clone());
    let at: i64 = db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/Photos/pain.jpg'", [&hex], |x| x.get(0))?)).await.unwrap();
    assert_eq!(at, 0, "déplacé, pas copié : un fichier n'existe qu'à un endroit");

    let again = r.site.set_public(&r.hex, &["/Public/pain.jpg".into()], true).await;
    assert_eq!(again["refused"][0]["why"], "déjà public");
    let not_public = r.site.set_public(&r.hex, &["/Docs/notes.txt".into()], false).await;
    assert_eq!(not_public["refused"][0]["why"], "pas dans Public");
    // Un nom déjà pris dans Public n'écrase rien.
    put(&r, "/Photos", "pain.jpg", "image/jpeg", "un autre pain").await;
    let clash = r.site.set_public(&r.hex, &["/Photos/pain.jpg".into()], true).await;
    assert_eq!(clash["moved"].as_array().unwrap().len(), 0, "{clash}");
    assert_eq!(get(&r, &url).await.2, "jpeg", "le fichier déjà public est intact");

    let back = r.site.set_public(&r.hex, &["/Public/pain.jpg".into()], false).await;
    assert_eq!(back["moved"][0]["to"], "/Retirés de Public/pain.jpg");
    assert_eq!(get(&r, &url).await.0, StatusCode::NOT_FOUND, "retiré : plus public");
    assert!(r.site.set_public(&r.hex, &["/".into(), "/Public".into(), "../x".into()], true).await["moved"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn only_public_images_can_be_picked_for_the_profile() {
    let r = rig("pick").await;
    put(&r, "/Public", "banniere.jpg", "image/jpeg", "BANNIERE").await;
    put(&r, "/Public/sous", "icone.png", "image/png", "ICONE").await;
    put(&r, "/Public", "page.html", "text/html", "x").await;
    put(&r, "/Photos", "prive.jpg", "image/jpeg", "PRIVÉ").await;
    let list = r.site.images(&r.hex).await;
    let names: Vec<&str> = list["images"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 2, "{list}");
    assert!(names.contains(&"banniere.jpg") && names.contains(&"icone.png"));
    assert_eq!(r.site.read_public(&r.hex, "/Public/banniere.jpg").await.unwrap(), b"BANNIERE");
    for bad in ["/Photos/prive.jpg", "/Public/../Photos/prive.jpg", "/Public/.cache.jpg", "/Public", "Photos/prive.jpg"] {
        assert!(r.site.read_public(&r.hex, bad).await.is_err(), "{bad}");
    }
}

#[tokio::test]
async fn a_station_that_does_not_host_the_site_redirects_to_the_one_that_does() {
    let r = rig("redirect").await;
    // Bob est hébergé ailleurs : son DID (répliqué) dit sa station d'accueil, et cette station publie son adresse dans sa santé économique.
    let (bob, node) = ("bob.durand@exemple.org", "ab".repeat(32));
    r.db.writer().execute("INSERT INTO identities(hex, role, email, hosted, home_station) VALUES (?1, 'multipass', ?2, 0, ?3)", vec!["bb".repeat(32).into(), bob.to_owned().into(), format!("12D3KooWautre:{node}").into()]).await.unwrap();
    let health = serde_json::json!({ "event_id": "e", "created_at": 1, "tags": { "url": "https://autre.exemple", "station": "12D3KooWautre" } }).to_string();
    r.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('economic_health', ?1, ?2, 1)", vec![node.clone().into(), health.into()]).await.unwrap();
    let name = name_of(bob);

    let resp = r.site.clone().router().oneshot(Request::builder().uri(format!("/site/{name}/photos/a.jpg")).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(resp.headers()[header::LOCATION], format!("https://autre.exemple/site/{name}/photos/a.jpg").as_str());

    // Par nom d'hôte (DNS en tourniquet) : même redirection, vers l'adresse par chemin de la bonne station.
    let mut patch = BTreeMap::new();
    patch.insert("OVH_ZONE".to_owned(), Some("coop.net".to_owned()));
    astroportd::coop_config::save(&r.db, &Arc::new(Store(r.db.clone())), UP, &patch).await.unwrap();
    let site = Arc::new(Site::new(r.dav.clone(), UP.to_owned()));
    let resp = site.by_host(&format!("{name}.coop.net"), "/index.html", &HeaderMap::new(), false).await.unwrap();
    assert_eq!((resp.status(), resp.headers()[header::LOCATION].to_str().unwrap()), (StatusCode::TEMPORARY_REDIRECT, format!("https://autre.exemple/site/{name}/index.html").as_str()));

    // Station d'accueil sans adresse connue, ou membre inconnu : pas de redirection devinée.
    r.db.writer().execute("DELETE FROM kv WHERE scope = 'economic_health'", vec![]).await.unwrap();
    assert_eq!(get(&r, &format!("/site/{name}/")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&r, "/site/inconnu.exemple.org/").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn page_views_are_counted_per_day_and_page_without_any_visitor_identity() {
    let r = rig("visits").await;
    r.site.install_example(&r.hex, false).await.unwrap();
    let base = format!("/site/{}", r.name);
    for _ in 0..2 {
        assert_eq!(get(&r, &format!("{base}/")).await.0, StatusCode::OK);
    }
    assert_eq!(get(&r, &format!("{base}/style.css")).await.0, StatusCode::OK, "une feuille de style n'est pas une visite");
    assert_eq!(get(&r, &format!("{base}/absent.html")).await.0, StatusCode::NOT_FOUND, "une page absente non plus");
    // Le compteur s'écrit en tâche de fond : on attend qu'il rattrape.
    let mut v = r.site.visits(&r.hex).await;
    for _ in 0..40 {
        if v["today"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        v = r.site.visits(&r.hex).await;
    }
    assert_eq!((v["today"].as_i64(), v["week"].as_i64(), v["days90"].as_i64()), (Some(2), Some(2), Some(2)), "{v}");
    assert_eq!(v["top"][0], serde_json::json!({ "path": "/", "hits": 2 }));
    assert_eq!(v["per_day"].as_array().unwrap().len(), 1);
    // Rien d'identifiant n'est gardé : la table n'a que le compte, le jour, la page et le nombre.
    let cols: Vec<String> = r.db.readers().read(|c| Ok(c.prepare("SELECT name FROM pragma_table_info('site_visits')")?.query_map([], |x| x.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap();
    assert_eq!(cols, ["hex", "day", "path", "hits"]);
    assert_eq!(r.site.info(&r.hex, EMAIL).await["visits"]["today"], 2);
}
