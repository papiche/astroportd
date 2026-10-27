//! Banc d'essai FaceCloud : WebDAV réel, moteur simulé, boîte d'envoi capturante.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::auth::derive_dav_token;
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher, router};
use astro_db::events::insert_event;
use astro_db::{Db, SqliteReplayStore};
use astro_facecloud::{BBox, DIM, DetectedFace, EngineError, FaceCloud, FaceConfig, FaceEngine, Outbox, OutboxError, l2_normalize};
use astro_nostr_proto::{Event, UnsignedEvent, pubkey_of};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use base64::Engine;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

pub const SECRET: &[u8] = b"secret-de-station";
pub const ALICE: [u8; 32] = [0xA1; 32];
pub const BOB: [u8; 32] = [0xB2; 32]; // ami réciproque, hébergé ici
pub const CAROL: [u8; 32] = [0xC3; 32]; // amie réciproque, hébergée ailleurs
pub const DAVE: [u8; 32] = [0xD4; 32]; // suivi par Alice, ne la suit pas en retour

pub fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("astro-face-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Vecteur pseudo-aléatoire de 512 composantes, déterministe d'après `seed`.
pub fn random_vec(seed: &str) -> Vec<f32> {
    let mut state = u64::from_le_bytes(Sha256::digest(seed.as_bytes())[..8].try_into().unwrap());
    (0..DIM).map(|_| (splitmix(&mut state) >> 11) as f32 / (1u64 << 53) as f32 - 0.5).collect()
}

fn mix(a: &[f32], wa: f32, b: &[f32], wb: f32) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x * wa + y * wb).collect()
}

/// Moteur simulé. L'« image » est du texte : `FACES:bob,carol@0.5,stranger+a,bob~carol` ou `NOFACE`.
///
/// - `nom` : empreinte de base du visage ; `nom@0.5` : confiance de détection ;
/// - `nom+tag` : même personne, autre photo (cosinus ≈ 0,995) ;
/// - `nom~autre` : mélange 50/50 avec un autre visage (cosinus ≈ 0,71 : zone grise).
#[derive(Default)]
pub struct MockEngine {
    pub calls: AtomicUsize,
    pub running: AtomicUsize,
    pub max_running: AtomicUsize,
    pub fail: Mutex<bool>,
    pub delay_ms: AtomicU64,
}

impl FaceEngine for MockEngine {
    async fn detect(&self, image: &[u8]) -> Result<Vec<DetectedFace>, EngineError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_running.fetch_max(now, Ordering::SeqCst);
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        self.running.fetch_sub(1, Ordering::SeqCst);
        if *self.fail.lock().unwrap() {
            return Err(EngineError::Backend("GPU occupé".into()));
        }
        let text = String::from_utf8_lossy(image);
        let Some(list) = text.strip_prefix("FACES:") else { return Ok(Vec::new()) };
        Ok(list
            .split(',')
            .map(|spec| {
                let (spec, score) = spec.split_once('@').map_or((spec, 0.99), |(s, sc)| (s, sc.parse().unwrap()));
                let embedding = if let Some((a, b)) = spec.split_once('~') {
                    mix(&random_vec(a), 0.5, &random_vec(b), 0.5)
                } else if let Some((a, tag)) = spec.split_once('+') {
                    mix(&random_vec(a), 1.0, &random_vec(&format!("{a}/{tag}")), 0.1)
                } else {
                    random_vec(spec)
                };
                DetectedFace { bbox: BBox { x1: 10.0, y1: 20.0, x2: 110.0, y2: 140.0 }, det_score: score, embedding }
            })
            .collect())
    }
}

/// Boîte d'envoi qui garde les enveloppes (et peut échouer à la demande).
#[derive(Default)]
pub struct CaptureOutbox {
    pub sent: Mutex<Vec<(String, Event)>>,
    pub fail: Mutex<bool>,
}

impl Outbox for CaptureOutbox {
    async fn send(&self, recipient_hex: &str, wrap: Event) -> Result<(), OutboxError> {
        if *self.fail.lock().unwrap() {
            return Err(OutboxError("relai injoignable".into()));
        }
        self.sent.lock().unwrap().push((recipient_hex.to_owned(), wrap));
        Ok(())
    }
}

pub type Fc = FaceCloud<FsBlobStore, MockEngine, CaptureOutbox>;

pub struct Rig {
    pub dir: TempDir,
    pub db: Arc<Db>,
    pub blobs: Arc<FsBlobStore>,
    pub app: Router,
    pub engine: Arc<MockEngine>,
    pub outbox: Arc<CaptureOutbox>,
    pub fc: Arc<Fc>,
    pub publisher: ManifestPublisher,
}

pub fn keys() -> Arc<dyn KeySource> {
    Arc::new(StaticKeySource::new().with(ALICE).with(BOB).with(CAROL))
}

fn event(secret: &[u8; 32], kind: u32, ts: u64, tags: Vec<Vec<String>>) -> Event {
    UnsignedEvent { created_at: ts, kind, tags, content: String::new() }.sign(secret).unwrap()
}

pub async fn rig(tag: &str, tune: impl FnOnce(&mut FaceConfig)) -> Rig {
    let dir = TempDir::new(tag);
    let db = Arc::new(Db::open(&dir.0.join("uplanet.db"), 2).unwrap());
    // Alice et Bob sont hébergés ici ; Carol et Dave vivent ailleurs.
    for (s, email, hosted) in [(&ALICE, "alice@example.org", 1), (&BOB, "bob@example.org", 1), (&CAROL, "carol@example.org", 0), (&DAVE, "dave@example.org", 0)] {
        db.writer()
            .execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, ?3)", vec![hex(s).into(), email.to_owned().into(), i64::from(hosted).into()])
            .await
            .unwrap();
    }
    // Graphe social : Alice suit Bob, Carol, Dave ; Bob et Carol la suivent en retour, pas Dave.
    for who in [&BOB, &CAROL, &DAVE] {
        db.writer()
            .execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(who).into()])
            .await
            .unwrap();
    }
    for (secret, follows) in [(&ALICE, vec![&BOB, &CAROL, &DAVE]), (&BOB, vec![&ALICE]), (&CAROL, vec![&ALICE]), (&DAVE, vec![&CAROL])] {
        let ev = event(secret, 3, 100, follows.iter().map(|f| vec!["p".to_owned(), hex(f)]).collect());
        db.writer().transaction(move |tx| insert_event(tx, &ev)).await.unwrap();
    }
    let blobs = Arc::new(FsBlobStore::open(dir.0.join("blobs")).unwrap());
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys(), Duration::from_secs(3600));
    let engine = Arc::new(MockEngine::default());
    let outbox = Arc::new(CaptureOutbox::default());
    let mut config = FaceConfig::default();
    tune(&mut config);
    let fc = Arc::new(FaceCloud::new(db.clone(), blobs.clone(), engine.clone(), outbox.clone(), keys(), Some(publisher.clone()), config));
    let state = Arc::new(DavState {
        db: db.clone(),
        blobs: blobs.clone(),
        replay: Arc::new(SqliteReplayStore::in_memory().unwrap()),
        publisher: Some(publisher.clone()),
        config: DavConfig::new(SECRET.to_vec()),
        on_upload: Some(fc.upload_hook()),
    });
    Rig { dir, db, blobs, app: router(state), engine, outbox, fc, publisher }
}

pub fn basic(user: &str, owner: &[u8; 32]) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{}", derive_dav_token(SECRET, &hex(owner)))))
}

pub async fn dav(app: &Router, method: &str, path: &str, user: &str, owner: &[u8; 32], body: Vec<u8>) -> (u16, Vec<u8>) {
    let req = Request::builder().method(method).uri(path).header(header::HOST, "localhost").header(header::AUTHORIZATION, basic(user, owner)).body(Body::from(body)).unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    (r.status().as_u16(), axum::body::to_bytes(r.into_body(), 64 << 20).await.unwrap().to_vec())
}

pub async fn put_photo(rig: &Rig, name: &str, content: &str) {
    let (status, _) = dav(&rig.app, "PUT", &format!("/dav/{name}"), "alice@example.org", &ALICE, content.as_bytes().to_vec()).await;
    assert_eq!(status, 201);
}

/// Laisse les tâches `tokio::spawn` du rappel de dépôt s'exécuter, puis vide la file.
pub async fn drain(rig: &Rig) -> astro_facecloud::QueueStats {
    for _ in 0..50 {
        let n: i64 = rig.db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM tasks WHERE status = 'pending'", [], |r| r.get(0))?)).await.unwrap();
        if n > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rig.fc.run_pending(50).await.unwrap()
}

/// Enrôle Bob, Carol et Dave sous leur identité, depuis une photo de référence.
pub async fn enroll_friends(rig: &Rig) {
    for (secret, name, label) in [(&BOB, "Bob", "bob"), (&CAROL, "Carol", "carol"), (&DAVE, "Dave", "dave")] {
        assert_eq!(rig.fc.enroll(&hex(&ALICE), &hex(secret), name, format!("FACES:{label}").as_bytes()).await.unwrap(), 1);
    }
}

pub fn unit_check(v: &mut [f32]) {
    l2_normalize(v).unwrap();
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4);
}

pub fn unused(_: HashSet<u8>) {}
