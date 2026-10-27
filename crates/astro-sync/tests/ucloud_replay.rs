//! Rejouabilité de bout en bout : fichier déposé en WebDAV → kind 30078 → purge locale → replay → VFS et clés restaurés.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::auth::derive_dav_token;
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher, router};
use astro_db::{Db, SqliteReplayStore};
use astro_sync::{replay_local, replay_with_ucloud};
use axum::body::Body;
use axum::http::{Request, header};
use base64::Engine;
use common::*;
use tower::ServiceExt;

const SECRET: &[u8] = b"secret-de-station";

struct Rig {
    _dir: TempDir,
    db: Arc<Db>,
    blobs: Arc<FsBlobStore>,
    app: axum::Router,
    publisher: ManifestPublisher,
    keys: Arc<dyn KeySource>,
}

async fn rig() -> Rig {
    let dir = TempDir::new("ucloud");
    let db = open_db(&dir);
    host(&db, &ALICE).await;
    host(&db, &BOB).await;
    db.writer().execute("UPDATE identities SET email = 'alice@example.org' WHERE hex = ?1", vec![hex(&ALICE).into()]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.0.join("ipfs-blobs")).unwrap());
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BOB));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys.clone(), Duration::from_secs(3600));
    let state = Arc::new(DavState {
        db: db.clone(),
        blobs: blobs.clone(),
        replay: Arc::new(SqliteReplayStore::in_memory().unwrap()),
        publisher: Some(publisher.clone()),
        config: DavConfig::new(SECRET.to_vec()),
        on_upload: None,
    });
    Rig { _dir: dir, db, blobs, app: router(state), publisher, keys }
}

async fn dav(app: &axum::Router, method: &str, path: &str, user: &str, owner: &[u8; 32], body: Vec<u8>) -> (u16, Vec<u8>) {
    let token = derive_dav_token(SECRET, &hex(owner));
    let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}")));
    let req = Request::builder().method(method).uri(path).header(header::HOST, "localhost").header(header::AUTHORIZATION, auth).body(Body::from(body)).unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status().as_u16();
    (status, axum::body::to_bytes(r.into_body(), 64 << 20).await.unwrap().to_vec())
}

async fn dump(db: &Db) -> (Vec<String>, Vec<String>) {
    db.readers()
        .read(|c| {
            let rows = |sql: &str| -> Result<Vec<String>, astro_db::DbError> { Ok(c.prepare(sql)?.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?) };
            Ok((
                rows("SELECT owner_hex||'|'||path||'|'||parent||'|'||type||'|'||coalesce(cid,'')||'|'||coalesce(size,'')||'|'||coalesce(sha_plain,'')||'|'||data FROM ucloud_entries ORDER BY 1")?,
                rows("SELECT owner_hex||'|'||cid||'|'||key_hex FROM ucloud_keyring ORDER BY 1")?,
            ))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn webdav_files_survive_a_purge_of_the_local_database() {
    let r = rig().await;
    let photo = sample(200_000, 11);
    assert_eq!(dav(&r.app, "MKCOL", "/dav/Photos", "alice@example.org", &ALICE, vec![]).await.0, 201);
    assert_eq!(dav(&r.app, "PUT", "/dav/Photos/a.jpg", "alice@example.org", &ALICE, photo.clone()).await.0, 201);
    assert_eq!(dav(&r.app, "PUT", "/dav/note.txt", "alice@example.org", &ALICE, b"bonjour".to_vec()).await.0, 201);
    assert_eq!(dav(&r.app, "PUT", "/dav/bob.bin", &hex(&BOB), &BOB, vec![7u8; 3000]).await.0, 201);
    assert!(r.publisher.flush().await.is_empty());

    let before = dump(&r.db).await;
    assert_eq!((before.0.len(), before.1.len()), (4, 3));
    let manifests: i64 = r.db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM nostr_events WHERE kind = 30078 AND d_tag = 'ucloud:manifest'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(manifests, 2, "un manifeste par compte");

    // Purge : tables uCloud, états rejouables et journaux locaux. Ne restent que nostr_events et les blobs.
    r.db
        .writer()
        .batch(
            ["DELETE FROM ucloud_entries", "DELETE FROM ucloud_keyring", "DELETE FROM kv", "DELETE FROM done", "DELETE FROM social_graph"]
                .into_iter()
                .map(|s| astro_db::Statement::new(s, vec![]))
                .collect(),
        )
        .await
        .unwrap();
    assert!(dump(&r.db).await.0.is_empty());
    assert_eq!(dav(&r.app, "GET", "/dav/note.txt", "alice@example.org", &ALICE, vec![]).await.0, 404);

    // Sans clés ni blobs, le replay classique ne reconstruit pas le VFS (et ne casse rien).
    replay_local(&r.db, 100).await.unwrap();
    assert!(dump(&r.db).await.0.is_empty());

    let stats = replay_with_ucloud(&r.db, 100, r.blobs.as_ref(), r.keys.as_ref()).await.unwrap();
    assert_eq!((stats.manifests, stats.manifests_skipped), (2, 0), "{stats:?}");
    let after = dump(&r.db).await;
    assert_eq!(after.0, before.0, "VFS restauré ligne pour ligne");
    assert_eq!(after.1, before.1, "trousseau restauré à l'identique");

    // Les fichiers sont lisibles, octet pour octet, y compris en lecture partielle.
    let (status, body) = dav(&r.app, "GET", "/dav/Photos/a.jpg", "alice@example.org", &ALICE, vec![]).await;
    assert_eq!((status, body == photo), (200, true));
    let (_, body) = dav(&r.app, "GET", "/dav/bob.bin", &hex(&BOB), &BOB, vec![]).await;
    assert_eq!(body, vec![7u8; 3000]);
}

#[tokio::test]
async fn accounts_without_a_local_key_are_skipped_and_untouched_accounts_keep_their_rows() {
    let r = rig().await;
    dav(&r.app, "PUT", "/dav/a.txt", "alice@example.org", &ALICE, b"a".to_vec()).await;
    dav(&r.app, "PUT", "/dav/b.bin", &hex(&BOB), &BOB, b"b".to_vec()).await;
    r.publisher.flush().await;
    // Une station qui ne détient que la clé d'Alice : le manifeste de Bob est ignoré, ses lignes locales intactes.
    let only_alice = StaticKeySource::new().with(ALICE);
    r.db.writer().execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1", vec![hex(&ALICE).into()]).await.unwrap();
    let stats = replay_with_ucloud(&r.db, 100, r.blobs.as_ref(), &only_alice).await.unwrap();
    assert_eq!((stats.manifests, stats.manifests_skipped), (1, 1), "{stats:?}");
    let rows = dump(&r.db).await.0;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|l| l.contains("|/a.txt|")) && rows.iter().any(|l| l.contains("|/b.bin|")));
}

#[tokio::test]
async fn a_missing_blob_is_reported_and_leaves_the_existing_vfs_alone() {
    let r = rig().await;
    dav(&r.app, "PUT", "/dav/a.txt", "alice@example.org", &ALICE, b"a".to_vec()).await;
    r.publisher.flush().await;
    let cid: String = r.db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'ucloud_manifest_cid' AND key = ?1", [hex(&ALICE)], |r| r.get(0))?)).await.unwrap();
    std::fs::remove_file(r.blobs.root().join(&cid)).unwrap();
    let stats = replay_with_ucloud(&r.db, 100, r.blobs.as_ref(), r.keys.as_ref()).await.unwrap();
    assert_eq!((stats.manifests, stats.manifests_skipped), (0, 1));
    assert!(stats.errors.iter().any(|(_, e)| e.contains("blob")), "{stats:?}");
    assert_eq!(dump(&r.db).await.0.len(), 1, "le VFS existant n'est pas détruit par un manifeste illisible");
}
