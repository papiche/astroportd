//! Contrôle d'intégrité nocturne : blobs présents, absents et corrompus ; reprise d'une nuit à l'autre.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use astro_db::Db;
use astro_dav::blob::{BlobStore, BlobWriter, FsBlobStore};
use astroportd::integrity::Integrity;
use tokio::io::AsyncWriteExt;

async fn put(store: &FsBlobStore, bytes: &[u8]) -> String {
    let mut w = store.begin().await.unwrap();
    w.write_all(bytes).await.unwrap();
    w.shutdown().await.unwrap();
    w.commit().await.unwrap().cid
}

async fn reference(db: &Db, cid: &str, n: usize) {
    db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, data) VALUES ('aa', ?1, '/', 'file', ?2, '{}')", vec![format!("/f{n}").into(), cid.to_owned().into()]).await.unwrap();
}

#[tokio::test]
async fn it_reports_missing_and_corrupt_blobs_and_resumes_where_it_stopped() {
    let dir = std::env::temp_dir().join(format!("astroportd-integrity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Arc::new(Db::open(&dir.join("u.db").tap_create(), 2).unwrap());
    let store = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let good = put(&store, b"contenu sain").await;
    let bad = put(&store, b"contenu qui va etre abime").await;
    let gone = put(&store, b"contenu supprime").await;
    // Corruption silencieuse (un bit) et disparition.
    let bad_path = store.root().join(&bad);
    let mut bytes = std::fs::read(&bad_path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&bad_path, bytes).unwrap();
    std::fs::remove_file(store.root().join(&gone)).unwrap();
    for (n, cid) in [&good, &bad, &gone].into_iter().enumerate() {
        reference(&db, cid, n).await;
    }
    let bus = astro_core::ActivityBus::new();
    let check = Integrity::new(db.clone(), store.clone(), Some(bus.clone()));
    let err = check.run(1_000).await.unwrap_err();
    assert!(err.contains("1 blob(s) absent(s) et 1 corrompu(s)"), "{err}");
    let report = check.last_report().await;
    assert_eq!((report["checked"].as_u64(), report["verified"].as_u64(), report["pass_complete"].as_bool()), (Some(3), Some(2), Some(true)));
    assert_eq!(report["missing"][0], gone);
    assert_eq!(report["corrupt"][0], bad);
    // Une fois réparé (blob corrompu retiré des références, absent retiré aussi), la nuit suivante est propre.
    db.writer().execute("DELETE FROM ucloud_entries WHERE cid IN (?1, ?2)", vec![bad.clone().into(), gone.clone().into()]).await.unwrap();
    let ok = check.run(2_000).await.unwrap();
    assert!(ok.contains("0 absent(s), 0 corrompu(s)") && ok.contains("passage complet"), "{ok}");
}

trait TapCreate {
    fn tap_create(self) -> std::path::PathBuf;
}
impl TapCreate for std::path::PathBuf {
    fn tap_create(self) -> std::path::PathBuf {
        std::fs::create_dir_all(self.parent().unwrap()).unwrap();
        self
    }
}
