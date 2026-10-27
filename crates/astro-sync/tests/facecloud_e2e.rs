//! FaceCloud entre deux stations : photo déposée chez Alice → visage de Carol reconnu → enveloppe NIP-59 publiée
//! sur le relai de Carol → Carol l'ouvre, récupère le blob IPFS et le déchiffre.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use astro_crypto::nip59;
use astro_crypto::{KeySource, StaticKeySource, UencKey, uenc};
use astro_dav::auth::derive_dav_token;
use astro_dav::{BlobStore, DavConfig, DavState, FsBlobStore, ManifestPublisher, router};
use astro_db::events::insert_event;
use astro_db::{Db, SqliteReplayStore};
use astro_facecloud::{BBox, DIM, DetectedFace, EngineError, FaceCloud, FaceConfig, FaceEngine};
use astro_nostr_proto::{Filter, UnsignedEvent};
use astro_relay::{DbCircle, Relay, RelayConfig, serve};
use astro_sync::{RelayOutbox, Signer, WsClient};
use axum::body::Body;
use axum::http::{Request, header};
use base64::Engine;
use common::*;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

struct TextEngine;

impl FaceEngine for TextEngine {
    async fn detect(&self, image: &[u8]) -> Result<Vec<DetectedFace>, EngineError> {
        let text = String::from_utf8_lossy(image);
        let Some(list) = text.strip_prefix("FACES:") else { return Ok(vec![]) };
        Ok(list
            .split(',')
            .map(|label| {
                let seed = label.split('+').next().unwrap_or(label);
                let mut s = u64::from_le_bytes(Sha256::digest(seed.as_bytes())[..8].try_into().unwrap());
                let mut next = move || {
                    s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
                    let mut z = s;
                    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                    z ^ (z >> 31)
                };
                let mut embedding: Vec<f32> = (0..DIM).map(|_| (next() >> 11) as f32 / (1u64 << 53) as f32 - 0.5).collect();
                if label.contains('+') {
                    embedding[0] += 0.01; // autre cliché de la même personne
                }
                DetectedFace { bbox: BBox { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 }, det_score: 0.99, embedding }
            })
            .collect())
    }
}

const SECRET: &[u8] = b"secret-station-a";

async fn host_row(db: &Db, secret: &[u8; 32], email: &str, hosted: i64) {
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, ?3)", vec![hex(secret).into(), email.to_owned().into(), hosted.into()]).await.unwrap();
}

#[tokio::test]
async fn photo_on_station_a_reaches_carol_on_station_b_through_a_nip59_envelope() {
    // Station B : le relai de Carol, qui l'héberge.
    let dir_b = TempDir::new("station-b");
    let db_b = open_db(&dir_b);
    host_row(&db_b, &CAROL, "carol@example.org", 1).await;
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db_b).await.unwrap();
    let relay = Relay::new(db_b.clone(), circle, RelayConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, relay));

    // Station A : Alice, hébergée, suit Carol qui la suit en retour (liste de contacts de Carol connue d'Alice).
    let dir_a = TempDir::new("station-a");
    let db_a = open_db(&dir_a);
    host_row(&db_a, &ALICE, "alice@example.org", 1).await;
    host_row(&db_a, &CAROL, "carol@example.org", 0).await;
    db_a.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(&CAROL).into()]).await.unwrap();
    for (s, who) in [(&ALICE, &CAROL), (&CAROL, &ALICE)] {
        let ev = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(who)]], content: String::new() }.sign(s).unwrap();
        db_a.writer().transaction(move |tx| insert_event(tx, &ev)).await.unwrap();
    }
    // « IPFS » partagé entre les deux stations (un dossier de blobs).
    let ipfs = Arc::new(FsBlobStore::open(dir_a.0.join("ipfs")).unwrap());
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
    let publisher = ManifestPublisher::spawn(db_a.clone(), ipfs.clone(), keys.clone(), Duration::from_secs(3600));
    let outbox = Arc::new(RelayOutbox::new(WsClient::new(Duration::from_secs(10)), vec![relay_url.clone()]));
    let fc = Arc::new(FaceCloud::new(db_a.clone(), ipfs.clone(), Arc::new(TextEngine), outbox, keys, Some(publisher.clone()), FaceConfig::default()));
    let app = router(Arc::new(DavState {
        db: db_a.clone(),
        blobs: ipfs.clone(),
        replay: Arc::new(SqliteReplayStore::in_memory().unwrap()),
        publisher: Some(publisher),
        config: DavConfig::new(SECRET.to_vec()),
        on_upload: Some(fc.upload_hook()),
    }));

    fc.enroll(&hex(&ALICE), &hex(&CAROL), "Carol", b"FACES:carol").await.unwrap();
    let photo = b"FACES:carol+mariage,inconnu".to_vec();
    let token = derive_dav_token(SECRET, &hex(&ALICE));
    let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("alice@example.org:{token}")));
    let put = Request::builder().method("PUT").uri("/dav/mariage.jpg").header(header::HOST, "localhost").header(header::AUTHORIZATION, auth).body(Body::from(photo.clone())).unwrap();
    assert_eq!(app.clone().oneshot(put).await.unwrap().status(), 201);

    // Le dépôt déclenche l'analyse (file) puis l'envoi, sans autre intervention.
    for _ in 0..100 {
        if fc.run_pending(10).await.unwrap().done > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Côté station B : le relai a stocké l'enveloppe adressée à Carol.
    let stored: i64 = db_b.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM nostr_events WHERE kind = 1059", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(stored, 1, "enveloppe acceptée par l'exception d'amorçage NIP-59 (expéditeur inconnu, destinataire hébergé)");

    // Les curieux ne la lisent pas : un anonyme est refusé, un étranger authentifié ne reçoit rien.
    let wrap_filter = Filter { kinds: Some(vec![1059]), tags: [('p', vec![hex(&CAROL)])].into(), ..Filter::default() };
    use astro_sync::RelayClient;
    assert!(WsClient::new(Duration::from_secs(10)).fetch(&relay_url, &wrap_filter).await.is_err(), "anonyme : auth-required");

    // Carol s'authentifie (NIP-42), récupère l'enveloppe et l'ouvre.
    let carol_client = WsClient::new(Duration::from_secs(10)).with_signer(Signer::new(CAROL));
    let wraps = carol_client.fetch(&relay_url, &wrap_filter).await.unwrap();
    assert_eq!(wraps.len(), 1);
    let opened = nip59::unwrap(&CAROL, &wraps[0]).unwrap();
    assert_eq!(opened.sender, hex(&ALICE));
    assert_eq!(opened.rumor.kind, 15);
    assert_eq!(opened.rumor.tag_value("name"), Some("mariage.jpg"));
    let cid = opened.rumor.tag_value("cid").unwrap();
    assert_eq!(opened.rumor.content, format!("ipfs://{cid}"));

    // Avec le CID et la clé du message : blob IPFS → photo en clair.
    let key = UencKey::from_hex(opened.rumor.tag_value("decryption-key").unwrap()).unwrap();
    let (mut blob, _) = ipfs.open(cid).await.unwrap();
    let mut plain = Vec::new();
    uenc::decrypt_stream(&key, &mut blob, &mut plain).await.unwrap();
    assert_eq!(plain, photo);
    assert_eq!(opened.rumor.tag_value("ox"), Some(hex::encode(Sha256::digest(&photo)).as_str()));
}

#[tokio::test]
async fn the_outbox_reports_relay_refusals() {
    use astro_facecloud::Outbox;
    let dir = TempDir::new("refusal");
    let db = open_db(&dir);
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await.unwrap();
    let relay = Relay::new(db.clone(), circle, RelayConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, relay));
    // Destinataire non hébergé par ce relai : refusé.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let rumor = nip59::Rumor { pubkey: hex(&ALICE), created_at: now, kind: 15, tags: vec![], content: "x".into() };
    let wrap = nip59::gift_wrap(&ALICE, &hex(&CAROL), &rumor, now).unwrap();
    let ob = RelayOutbox::new(WsClient::new(Duration::from_secs(5)), vec!["ws://127.0.0.1:1".into(), url]);
    let err = ob.send(&hex(&CAROL), wrap).await.unwrap_err().to_string();
    assert!(err.contains("restricted") && err.contains("127.0.0.1:1"), "{err}");
}
