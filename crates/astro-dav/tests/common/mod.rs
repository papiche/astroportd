//! Banc d'essai WebDAV : base réelle, magasin de blobs sur disque, requêtes sans réseau.

#![allow(dead_code, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use astro_crypto::KeySource;
use astro_crypto::StaticKeySource;
use astro_dav::auth::derive_dav_token;
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher, router};
use astro_db::{Db, SqliteReplayStore};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, header};
use base64::Engine;
use tower::ServiceExt;

pub const STATION_SECRET: &[u8] = b"secret-de-station-pour-les-tests";
pub const ALICE: [u8; 32] = [0xA1; 32]; // MULTIPASS
pub const BOB: [u8; 32] = [0xB2; 32]; // ZenCard
pub const STRANGER: [u8; 32] = [0xC3; 32]; // non hébergé

pub fn hex(secret: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(secret).unwrap()
}

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("astro-dav-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Bench {
    pub dir: TempDir,
    pub db: Arc<Db>,
    pub blobs: Arc<FsBlobStore>,
    pub app: Router,
    pub publisher: ManifestPublisher,
}

pub fn keys() -> Arc<dyn KeySource> {
    Arc::new(StaticKeySource::new().with(ALICE).with(BOB))
}

/// Serveur sur un magasin de blobs quelconque (base, identités Alice/Bob, publieur de manifeste).
pub async fn serve_on<B: astro_dav::BlobStore>(dir: &TempDir, blobs: Arc<B>, tune: impl FnOnce(&mut DavConfig)) -> (Arc<Db>, Router, ManifestPublisher) {
    let db = Arc::new(Db::open(&dir.0.join("uplanet.db"), 2).unwrap());
    for (secret, role, email) in [(&ALICE, "multipass", "alice@example.org"), (&BOB, "zencard", "bob@example.org")] {
        db.writer()
            .execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, ?2, ?3, 1)", vec![hex(secret).into(), role.to_owned().into(), email.to_owned().into()])
            .await
            .unwrap();
    }
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys(), std::time::Duration::from_secs(3600));
    let mut config = DavConfig::new(STATION_SECRET.to_vec());
    tune(&mut config);
    let state = Arc::new(DavState {
        db: db.clone(),
        blobs,
        replay: Arc::new(SqliteReplayStore::in_memory().unwrap()),
        publisher: Some(publisher.clone()),
        config,
        on_upload: None,
    });
    (db, router(state), publisher)
}

pub async fn bench_with(tag: &str, tune: impl FnOnce(&mut DavConfig)) -> Bench {
    let dir = TempDir::new(tag);
    let blobs = Arc::new(FsBlobStore::open(dir.0.join("blobs")).unwrap());
    let (db, app, publisher) = serve_on(&dir, blobs.clone(), tune).await;
    Bench { dir, db, blobs, app, publisher }
}

pub async fn bench(tag: &str) -> Bench {
    bench_with(tag, |_| {}).await
}

pub fn basic(user: &str, owner_hex: &str) -> String {
    let token = derive_dav_token(STATION_SECRET, owner_hex);
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}")))
}

pub fn alice() -> String {
    basic("alice@example.org", &hex(&ALICE))
}

pub fn bob() -> String {
    basic(&hex(&BOB), &hex(&BOB))
}

pub fn nip98(secret: &[u8; 32], method: &str, path: &str, payload: Option<&str>) -> String {
    let mut tags = vec![vec!["u".to_owned(), format!("http://localhost{path}")], vec!["method".to_owned(), method.to_owned()]];
    if let Some(p) = payload {
        tags.push(vec!["payload".into(), p.to_owned()]);
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let ev = astro_nostr_proto::UnsignedEvent { created_at: now, kind: 27_235, tags, content: String::new() }.sign(secret).unwrap();
    format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&ev).unwrap()))
}

pub async fn call(app: &Router, method: &str, path: &str, auth: Option<&str>, extra: &[(&str, &str)], body: Body) -> Response<Body> {
    let mut b = Request::builder().method(method).uri(path).header(header::HOST, "localhost");
    if let Some(a) = auth {
        b = b.header(header::AUTHORIZATION, a);
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    app.clone().oneshot(b.body(body).unwrap()).await.unwrap()
}

pub async fn text(r: Response<Body>) -> (u16, String) {
    let status = r.status().as_u16();
    let bytes = axum::body::to_bytes(r.into_body(), 64 << 20).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn bytes(r: Response<Body>) -> (u16, axum::http::HeaderMap, Vec<u8>) {
    let (parts, body) = r.into_parts();
    (parts.status.as_u16(), parts.headers, axum::body::to_bytes(body, 64 << 20).await.unwrap().to_vec())
}

/// Corps en plusieurs morceaux, pour exercer le chemin en flux.
pub fn streamed(data: Vec<u8>, piece: usize) -> Body {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = data.chunks(piece).map(|c| Ok(c.to_vec())).collect();
    Body::from_stream(futures_util::stream::iter(chunks))
}

/// Données pseudo-aléatoires reproductibles, sans motif lisible.
pub fn sample(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}
