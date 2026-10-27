//! `#upload` de bout en bout : la tâche `bro.upload` est traitée par le travailleur — rapatriement d'une URL (HTTP,
//! HTTPS, redirections) ou d'un CID, chiffrement UENC v2, dépôt dans `/Uploads` — avec les garde-fous SSRF, taille,
//! quota, reprises et notification du demandeur.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::ActivityBus;
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::auth::derive_dav_token;
use astro_dav::blob::{BlobStore, BlobWriter, cid_raw_sha256};
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher};
use astro_db::{Db, SqliteReplayStore};
use astroportd::upload_worker::{Outcome, UploadWorker};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use rusqlite::types::Value as V;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tower::ServiceExt;

const SECRET: &[u8] = b"secret-de-station-pour-les-tests";
const ALICE: [u8; 32] = [0xA1; 32];
const T: i64 = 1_800_000_000;

fn hex(s: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(s).unwrap()
}

struct Bench {
    dav: Arc<DavState<FsBlobStore>>,
    db: Arc<Db>,
    notes: Arc<Mutex<Vec<(String, String)>>>,
    dir: std::path::PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn bench(tag: &str, tune: impl FnOnce(&mut DavConfig)) -> Bench {
    let dir = std::env::temp_dir().join(format!("astroportd-upw-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@example.org', 1)", vec![V::Text(hex(&ALICE))]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys, Duration::from_secs(3600));
    let mut config = DavConfig::new(SECRET.to_vec());
    tune(&mut config);
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config, on_upload: None });
    Bench { dav, db, notes: Arc::new(Mutex::new(Vec::new())), dir }
}

impl Bench {
    fn worker(&self, roots: Vec<rustls::pki_types::CertificateDer<'static>>, local_ok: bool) -> UploadWorker<FsBlobStore> {
        let notes = self.notes.clone();
        let notify: astroportd::upload_worker::Notify = Arc::new(move |owner, text| {
            let notes = notes.clone();
            Box::pin(async move { notes.lock().unwrap().push((owner, text)) })
        });
        let w = UploadWorker::new(self.dav.clone(), ActivityBus::new(), Some(notify));
        if local_ok { w.for_tests(roots) } else { w }
    }

    async fn enqueue(&self, source: &str, name: Option<&str>) -> i64 {
        let payload = json!({ "owner": hex(&ALICE), "source": source, "name": name }).to_string();
        self.db.writer().execute("INSERT INTO tasks(kind, payload, status, created_at) VALUES ('bro.upload', ?1, 'pending', ?2)", vec![V::Text(payload), V::Integer(T)]).await.unwrap().last_insert_rowid
    }

    async fn status(&self, id: i64) -> (String, i64) {
        self.db.readers().read(move |c| Ok(c.query_row("SELECT status, attempts FROM tasks WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap()
    }

    async fn read(&self, path: &str) -> Vec<u8> {
        let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("alice@example.org:{}", derive_dav_token(SECRET, &hex(&ALICE)))));
        let url: String = format!("/dav{path}").split('/').map(|s| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>()).collect::<Vec<_>>().join("/");
        let app = astro_dav::router(self.dav.clone());
        let resp = app.oneshot(axum::http::Request::builder().uri(url).header(header::AUTHORIZATION, auth).header(header::HOST, "localhost").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), 200, "{path}");
        http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes().to_vec()
    }

    fn last_note(&self) -> String {
        self.notes.lock().unwrap().last().map(|n| n.1.clone()).unwrap_or_default()
    }
}

#[derive(Clone, Default)]
struct Hits(Arc<AtomicU32>);

async fn web() -> (String, Hits) {
    let hits = Hits::default();
    async fn doc(State(h): State<Hits>, axum::extract::Path(name): axum::extract::Path<String>) -> Response {
        h.0.fetch_add(1, Ordering::SeqCst);
        let ct = if name.ends_with(".pdf") { "application/pdf" } else { "text/plain" };
        ([(header::CONTENT_TYPE, ct)], format!("contenu de {name}")).into_response()
    }
    async fn redirect() -> Response {
        (StatusCode::FOUND, [(header::LOCATION, "/docs/x.txt")]).into_response()
    }
    async fn looping() -> Response {
        (StatusCode::FOUND, [(header::LOCATION, "/loop")]).into_response()
    }
    async fn big(State(h): State<Hits>) -> Response {
        h.0.fetch_add(1, Ordering::SeqCst);
        vec![b'x'; 5000].into_response()
    }
    async fn flaky(State(h): State<Hits>, headers: HeaderMap) -> Response {
        let n = h.0.fetch_add(1, Ordering::SeqCst);
        let _ = headers;
        if n < 1 { StatusCode::SERVICE_UNAVAILABLE.into_response() } else { "enfin".into_response() }
    }
    async fn down() -> Response {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
    async fn missing() -> Response {
        StatusCode::NOT_FOUND.into_response()
    }
    let app = Router::new().route("/docs/{name}", get(doc)).route("/r", get(redirect)).route("/loop", get(looping)).route("/big", get(big)).route("/flaky", get(flaky)).route("/down", get(down)).route("/missing", get(missing)).with_state(hits.clone());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, hits)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_url_is_fetched_encrypted_and_filed_under_uploads_without_overwriting() {
    let b = bench("url", |_| {}).await;
    let (base, _) = web().await;
    let w = b.worker(vec![], true);
    let id = b.enqueue(&format!("{base}/docs/rapport%20annuel.pdf"), None).await;
    let out = w.run_once(T).await;
    assert_eq!(out, vec![(id, Outcome::Done("/Uploads/rapport annuel.pdf".into()))]);
    assert_eq!(b.read("/Uploads/rapport annuel.pdf").await, b"contenu de rapport annuel.pdf");
    assert_eq!(b.status(id).await, ("done".into(), 1));
    assert!(b.last_note().starts_with("✅ Document ajouté à votre uCloud : /Uploads/rapport annuel.pdf"), "{}", b.last_note());
    // Le fichier est bien chiffré sur disque : aucun octet du clair dans les blobs.
    let on_disk: Vec<u8> = std::fs::read_dir(b.dir.join("blobs")).unwrap().flatten().flat_map(|e| std::fs::read(e.path()).unwrap_or_default()).collect();
    assert!(!on_disk.windows(10).any(|w| w == b"contenu de"), "clair retrouvé sur le disque");
    // Mime déduit du Content-Type ; même URL redemandée : le nom est numéroté.
    let mime: String = b.db.readers().read(|c| Ok(c.query_row("SELECT mime FROM ucloud_entries WHERE path = '/Uploads/rapport annuel.pdf'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(mime, "application/pdf");
    b.enqueue(&format!("{base}/docs/rapport%20annuel.pdf"), None).await;
    assert_eq!(w.run_once(T).await[0].1, Outcome::Done("/Uploads/rapport annuel (2).pdf".into()));
    // Un nom demandé est assaini ; une redirection est suivie.
    b.enqueue(&format!("{base}/r"), Some("../../évasion.txt")).await;
    assert_eq!(w.run_once(T).await[0].1, Outcome::Done("/Uploads/_.._évasion.txt".into()));
    assert_eq!(b.read("/Uploads/_.._évasion.txt").await, b"contenu de x.txt");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_station_never_reaches_private_addresses_or_loops_and_says_so() {
    let b = bench("ssrf", |_| {}).await;
    let (base, hits) = web().await;
    let strict = b.worker(vec![], false); // production : adresses locales interdites
    for url in [format!("{base}/docs/a.txt"), "http://127.0.0.1:1/".into(), "http://[::1]:80/".into(), "http://169.254.169.254/latest/meta-data/".into(), "http://192.168.1.1/".into(), "http://10.0.0.5/x".into(), "ftp://exemple.org/x".into(), "file:///etc/passwd".into(), "http://user:mdp@exemple.org/".into()] {
        let id = b.enqueue(&url, None).await;
        let out = strict.run_once(T).await;
        assert!(matches!(&out[0].1, Outcome::Failed(why) if why.contains("adresse refusée") || why.contains("URL invalide")), "{url} : {out:?}");
        assert_eq!(b.status(id).await.0, "failed", "{url}");
    }
    assert_eq!(hits.0.load(Ordering::SeqCst), 0, "le serveur local n'a jamais été contacté");
    assert!(b.last_note().starts_with("❌ Document non ajouté"), "{}", b.last_note());

    // Redirections : boucle refusée.
    let w = b.worker(vec![], true);
    b.enqueue(&format!("{base}/loop"), None).await;
    assert!(matches!(&w.run_once(T).await[0].1, Outcome::Failed(why) if why.contains("redirection")));
}

#[tokio::test(flavor = "multi_thread")]
async fn size_and_quota_limits_are_enforced_before_anything_is_kept() {
    let b = bench("limits", |c| {
        c.max_file_multipass = 1000;
        c.quota_multipass = 2500;
    })
    .await;
    let (base, _) = web().await;
    let w = b.worker(vec![], true);
    b.enqueue(&format!("{base}/big"), Some("gros.bin")).await;
    let out = w.run_once(T).await;
    assert!(matches!(&out[0].1, Outcome::Failed(why) if why.contains("plafond")), "{out:?}");
    let files: i64 = b.db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_entries WHERE type = 'file'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(files, 0, "rien n'est conservé");
    assert!(std::fs::read_dir(b.dir.join("blobs")).unwrap().flatten().all(|e| e.path().is_dir() || e.metadata().unwrap().len() == 0), "aucun blob orphelin");
    // Quota : trois fichiers de 900 octets dépassent 2 500.
    for i in 0..3 {
        b.enqueue(&format!("{base}/docs/{}.txt", "a".repeat(880 + i)), None).await;
    }
    let outs: Vec<Outcome> = w.run_once(T).await.into_iter().map(|o| o.1).collect();
    assert!(matches!(outs[0], Outcome::Done(_)) && matches!(outs[1], Outcome::Done(_)), "{outs:?}");
    assert!(matches!(&outs[2], Outcome::Failed(why) if why.contains("quota")), "{outs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn transient_failures_are_retried_with_a_delay_then_abandoned_after_three_attempts() {
    let b = bench("retry", |_| {}).await;
    let (base, hits) = web().await;
    let w = b.worker(vec![], true);
    let id = b.enqueue(&format!("{base}/flaky"), Some("peu-fiable.txt")).await;
    assert_eq!(w.run_once(T).await[0].1, Outcome::Retry("le serveur répond 503".into()));
    assert_eq!(b.status(id).await, ("pending".into(), 1));
    assert!(w.run_once(T + 30).await.is_empty(), "pas avant le délai (60 s)");
    assert_eq!(hits.0.load(Ordering::SeqCst), 1);
    assert_eq!(w.run_once(T + 61).await[0].1, Outcome::Done("/Uploads/peu-fiable.txt".into()));
    assert_eq!(b.status(id).await, ("done".into(), 2));

    let down = b.enqueue(&format!("{base}/down"), None).await;
    assert!(matches!(w.run_once(T).await[0].1, Outcome::Retry(_)));
    assert!(matches!(w.run_once(T + 61).await[0].1, Outcome::Retry(_)));
    assert!(matches!(&w.run_once(T + 61 + 241).await[0].1, Outcome::Failed(_)), "troisième échec : abandon");
    assert_eq!(b.status(down).await, ("failed".into(), 3));
    assert!(b.last_note().starts_with("❌"), "le demandeur est prévenu de l'abandon");
    // 404 : définitif, aucun nouvel essai.
    let gone = b.enqueue(&format!("{base}/missing"), None).await;
    assert!(matches!(w.run_once(T).await[0].1, Outcome::Failed(_)));
    assert_eq!(b.status(gone).await, ("failed".into(), 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cid_is_pulled_from_the_blob_store_but_station_ciphertext_is_refused() {
    let b = bench("cid", |_| {}).await;
    // Un document en clair déjà dans le magasin (IPFS local) : on le rapatrie.
    let data = b"%PDF-1.7 un contrat en clair";
    let mut w = b.dav.blobs.begin().await.unwrap();
    w.write_all(data).await.unwrap();
    let cid = w.commit().await.unwrap().cid;
    assert_eq!(cid, cid_raw_sha256(&Sha256::digest(data).into()));
    let worker = b.worker(vec![], false);
    b.enqueue(&cid, Some("contrat.pdf")).await;
    assert_eq!(worker.run_once(T).await[0].1, Outcome::Done("/Uploads/contrat.pdf".into()));
    assert_eq!(b.read("/Uploads/contrat.pdf").await, data);
    // Le blob chiffré de ce fichier (UENC) n'est pas un document : refus explicite.
    let encrypted: String = b.db.readers().read(|c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE path = '/Uploads/contrat.pdf'", [], |r| r.get(0))?)).await.unwrap();
    b.enqueue(&encrypted, None).await;
    assert!(matches!(&worker.run_once(T).await[0].1, Outcome::Failed(why) if why.contains("UENC")));
    // CID inconnu : introuvable, définitif.
    b.enqueue(&cid_raw_sha256(&[7; 32]), None).await;
    assert!(matches!(&worker.run_once(T).await[0].1, Outcome::Failed(why) if why.contains("introuvable")));
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_tasks_are_recovered_and_https_is_really_validated() {
    let b = bench("https", |_| {}).await;
    // Une tâche `running` depuis 20 minutes (démon arrêté) revient en file.
    let id = b.enqueue("http://exemple.invalid/x", None).await;
    b.db.writer().execute("UPDATE tasks SET status = 'running', not_before = ?2 WHERE id = ?1", vec![V::Integer(id), V::Integer(T - 1200)]).await.unwrap();
    let w0 = b.worker(vec![], false);
    assert_eq!(w0.recover(T).await, 1);
    assert_eq!(b.status(id).await.0, "pending");
    b.db.writer().execute("DELETE FROM tasks", vec![]).await.unwrap();

    // Serveur HTTPS à certificat auto-signé pour 127.0.0.1.
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
    let (cert_der, key_der) = (cert.cert.der().clone(), rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_cfg = rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap().with_no_client_auth().with_single_cert(vec![cert_der.clone()], key_der).unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = l.accept().await else { return };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                let mut buf = [0u8; 2048];
                let _ = tokio::io::AsyncReadExt::read(&mut tls, &mut buf).await;
                let body = "document en HTTPS";
                let _ = tls.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    let url = format!("https://127.0.0.1:{port}/doc.txt");
    // Sans la racine de confiance : certificat refusé, définitif.
    let untrusting = b.worker(vec![], true);
    b.enqueue(&url, None).await;
    assert!(matches!(&untrusting.run_once(T).await[0].1, Outcome::Failed(why) if why.contains("TLS")), "un certificat inconnu doit être refusé");
    // Avec la racine : le document arrive.
    let trusting = b.worker(vec![cert_der], true);
    b.enqueue(&url, None).await;
    assert_eq!(trusting.run_once(T).await[0].1, Outcome::Done("/Uploads/doc.txt".into()));
    assert_eq!(b.read("/Uploads/doc.txt").await, b"document en HTTPS");
}
