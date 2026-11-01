//! Pont entre deux plans qui partagent un bassin de blobs : adoption sans copie, rejouable, sans écrasement, et un blob
//! n'est supprimé que quand plus aucun plan ne le référence.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use astro_crypto::{KeySource, StaticKeySource, UencKey, uenc};
use astro_dav::blob::BlobStore;
use astro_dav::{AutoBlobStore, DavConfig, DavState, FsBlobStore, ManifestPublisher};
use astro_db::{Db, SqliteReplayStore};
use astroportd::bridge::{self, Options};
use rusqlite::types::Value as V;

const ALICE: [u8; 32] = [0xA1; 32]; // la personne, dans le plan ORIGIN
const ALICE_ZEN: [u8; 32] = [0xA2; 32]; // la même personne, dans le plan ZEN

fn hex(s: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(s).unwrap()
}

struct Plane {
    dav: Arc<DavState<AutoBlobStore>>,
    db_path: PathBuf,
}

struct Rig {
    dir: PathBuf,
    pool: PathBuf,
    origin: Plane,
    zen: Plane,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn plane(dir: &std::path::Path, name: &str, secret: &[u8; 32], pool: &std::path::Path, peer: PathBuf) -> Plane {
    let root = dir.join(name);
    std::fs::create_dir_all(&root).unwrap();
    let db_path = root.join("uplanet.db");
    let db = Arc::new(Db::open(&db_path, 2).unwrap());
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@example.org', 1)", vec![V::Text(hex(secret))]).await.unwrap();
    let blobs = Arc::new(AutoBlobStore::new(None, FsBlobStore::open(pool).unwrap()).with_pool_peers(vec![peer]));
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(*secret));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys, Duration::from_secs(3600));
    let dav = Arc::new(DavState { db, blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config: DavConfig::new(b"secret-de-station-pour-les-tests".to_vec()), on_upload: None });
    Plane { dav, db_path }
}

async fn rig(tag: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("astroportd-bridge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let pool = dir.join("pool");
    let (a, b) = (dir.join("origin/uplanet.db"), dir.join("zen/uplanet.db"));
    let origin = plane(&dir, "origin", &ALICE, &pool, b).await;
    let zen = plane(&dir, "zen", &ALICE_ZEN, &pool, a).await;
    Rig { dir, pool, origin, zen }
}

fn blobs_in(pool: &std::path::Path) -> usize {
    std::fs::read_dir(pool).unwrap().filter_map(Result::ok).filter(|e| e.file_name() != ".tmp").count()
}

fn photo(n: u8) -> Vec<u8> {
    (0..200_000u32).map(|i| (i % 251) as u8 ^ n).collect()
}

async fn put(p: &Plane, owner: &str, dir: &str, name: &str, body: Vec<u8>) {
    astro_dav::ingest::ingest(&p.dav, owner, dir, name, Some("application/octet-stream"), std::io::Cursor::new(body)).await.unwrap();
}

fn opts(from: &[u8; 32], to: &[u8; 32], path: &str, dry_run: bool) -> Options {
    Options { from_owner: hex(from), to_owner: hex(to), path: path.into(), dest: "/Imports/ORIGIN".into(), dry_run }
}

async fn paths(p: &Plane, owner: &[u8; 32]) -> Vec<String> {
    let o = hex(owner);
    p.dav.db.readers().read(move |c| Ok(c.prepare("SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' ORDER BY path")?.query_map([o], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap()
}

#[tokio::test]
async fn files_are_adopted_without_one_more_byte_and_read_back_in_the_other_plane() {
    let r = rig("adopt").await;
    let owner = hex(&ALICE);
    put(&r.origin, &owner, "/Photos/2024", "a.bin", photo(1)).await;
    put(&r.origin, &owner, "/Photos", "b.bin", photo(2)).await;
    put(&r.origin, &owner, "/Docs", "hors-sujet.txt", b"pas dans /Photos".to_vec()).await;
    // Un ancien fichier (Kubo) et un fichier dont le blob manque : jamais copiés en silence.
    r.origin.dav.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, size, data) VALUES (?1, '/Photos/vieux.bin', '/Photos', 'file', 'QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG', 10, '{}')", vec![V::Text(owner.clone())]).await.unwrap();
    r.origin.dav.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, size, data) VALUES (?1, '/Photos/perdu.bin', '/Photos', 'file', 'bafkreiabsent', 10, '{}')", vec![V::Text(owner.clone())]).await.unwrap();
    r.origin.dav.db.writer().execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, 'bafkreiabsent', ?2, 'uenc2')", vec![V::Text(owner), V::Text("1".repeat(64))]).await.unwrap();
    let before = blobs_in(&r.pool);
    let pool = FsBlobStore::open(&r.pool).unwrap();
    let config = DavConfig::new(Vec::new());

    // À blanc : on compte, on n'écrit rien.
    let dry = bridge::adopt(&r.origin.db_path, &r.zen.dav.db, &pool, &config, &opts(&ALICE, &ALICE_ZEN, "/Photos", true)).await.unwrap();
    assert_eq!((dry.adopted, dry.legacy, dry.missing), (2, 1, 1));
    assert!(paths(&r.zen, &ALICE_ZEN).await.is_empty());

    // Un chemin déjà pris par un autre contenu n'est jamais écrasé.
    put(&r.zen, &hex(&ALICE_ZEN), "/Imports/ORIGIN", "b.bin", b"autre contenu".to_vec()).await;
    let before_zen = blobs_in(&r.pool);
    let real = bridge::adopt(&r.origin.db_path, &r.zen.dav.db, &pool, &config, &opts(&ALICE, &ALICE_ZEN, "/Photos", false)).await.unwrap();
    assert_eq!((real.adopted, real.conflicts, real.legacy, real.missing), (1, 1, 1, 1), "{real:?}");
    assert_eq!(blobs_in(&r.pool), before_zen, "l'adoption n'écrit aucun blob : le disque ne grossit pas");
    assert!(before_zen > before, "(le fichier déjà là dans ZEN avait, lui, sa place)");
    assert_eq!(paths(&r.zen, &ALICE_ZEN).await, ["/Imports/ORIGIN/2024/a.bin", "/Imports/ORIGIN/b.bin"]);

    // Le compte ZEN relit le fichier adopté avec la clé partagée.
    let (cid, key): (String, String) = r.zen.dav.db.readers().read(|c| Ok(c.query_row("SELECT e.cid, k.key_hex FROM ucloud_entries e JOIN ucloud_keyring k ON k.owner_hex = e.owner_hex AND k.cid = e.cid WHERE e.path = '/Imports/ORIGIN/2024/a.bin'", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
    let (mut reader, _) = pool.open(&cid).await.unwrap();
    let mut blob = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut blob).await.unwrap();
    assert_eq!(uenc::decrypt_to_vec(&UencKey::from_hex(&key).unwrap(), &blob).await.unwrap(), photo(1));

    // Rejouable : plus rien à adopter.
    let again = bridge::adopt(&r.origin.db_path, &r.zen.dav.db, &pool, &config, &opts(&ALICE, &ALICE_ZEN, "/Photos", false)).await.unwrap();
    assert_eq!((again.adopted, again.same, again.conflicts), (0, 1, 1));
}

#[tokio::test]
async fn a_shared_blob_is_freed_only_when_no_plane_references_it_any_more() {
    let r = rig("gc").await;
    let owner = hex(&ALICE);
    put(&r.origin, &owner, "/Photos", "a.bin", photo(3)).await;
    let pool = FsBlobStore::open(&r.pool).unwrap();
    bridge::adopt(&r.origin.db_path, &r.zen.dav.db, &pool, &DavConfig::new(Vec::new()), &opts(&ALICE, &ALICE_ZEN, "/Photos", false)).await.unwrap();
    let cid: String = r.origin.dav.db.readers().read(|c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE path = '/Photos/a.bin'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(blobs_in(&r.pool), 1);

    // ORIGIN supprime sa copie : ZEN s'en sert encore, le blob reste.
    r.origin.dav.db.writer().execute("DELETE FROM ucloud_entries WHERE path = '/Photos/a.bin'", vec![]).await.unwrap();
    r.origin.dav.db.writer().execute("DELETE FROM ucloud_keyring", vec![]).await.unwrap();
    r.origin.dav.blobs.remove(&cid).await.unwrap();
    assert_eq!(blobs_in(&r.pool), 1, "ZEN référence encore le blob");
    // ZEN supprime à son tour : le blob disparaît.
    r.zen.dav.db.writer().execute("DELETE FROM ucloud_entries WHERE type = 'file'", vec![]).await.unwrap();
    r.zen.dav.db.writer().execute("DELETE FROM ucloud_keyring", vec![]).await.unwrap();
    r.zen.dav.blobs.remove(&cid).await.unwrap();
    assert_eq!(blobs_in(&r.pool), 0, "plus aucun plan ne le référence : libéré");
}
