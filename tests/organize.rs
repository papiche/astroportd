//! Rangement du uCloud par type : aperçu, déplacements sans toucher aux blobs, protections, collisions, sous-dossiers par date,
//! visages qui suivent leur photo, dossiers vides supprimés, annulation, rejeu et service en arrière-plan.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher};
use astro_db::{Db, SqliteReplayStore};
use astroportd::organize::{self, ByDate, Kind, Options, OrganizeService, classify};
use rusqlite::types::Value as V;

const ALICE: [u8; 32] = [0xA1; 32];

fn hex(s: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(s).unwrap()
}

struct Bench {
    dav: Arc<DavState<FsBlobStore>>,
    dir: std::path::PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn bench(tag: &str) -> Bench {
    let dir = std::env::temp_dir().join(format!("astroportd-org-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@example.org', 1)", vec![V::Text(hex(&ALICE))]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys, Duration::from_secs(3600));
    let dav = Arc::new(DavState { db, blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config: DavConfig::new(b"secret-de-station-pour-les-tests".to_vec()), on_upload: None });
    Bench { dav, dir }
}

/// 2003-06-15 12:00 UTC et 2018-11-02 08:00 UTC.
const JUNE_2003: i64 = 1_055_678_400;
const NOV_2018: i64 = 1_541_145_600;

async fn put_dir(b: &Bench, path: &str) {
    let parent = path.rsplit_once('/').map_or("/", |(p, _)| if p.is_empty() { "/" } else { p }).to_owned();
    b.dav.db.writer().execute("INSERT OR IGNORE INTO ucloud_entries(owner_hex, path, parent, type, data) VALUES (?1, ?2, ?3, 'dir', '{}')", vec![V::Text(hex(&ALICE)), V::Text(path.into()), V::Text(parent)]).await.unwrap();
}

async fn put(b: &Bench, path: &str, mime: &str, size: i64, when: i64) {
    let parent = path.rsplit_once('/').map_or("/", |(p, _)| if p.is_empty() { "/" } else { p }).to_owned();
    b.dav.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, data) VALUES (?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, ?7, '{}')", vec![V::Text(hex(&ALICE)), V::Text(path.into()), V::Text(parent), V::Text(format!("cid-{path}")), V::Text(mime.into()), V::Integer(size), V::Integer(when)]).await.unwrap();
}

async fn files(b: &Bench) -> Vec<String> {
    b.dav.db.readers().read(|c| Ok(c.prepare("SELECT path FROM ucloud_entries WHERE type = 'file' ORDER BY path")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap()
}

async fn dirs(b: &Bench) -> Vec<String> {
    b.dav.db.readers().read(|c| Ok(c.prepare("SELECT path FROM ucloud_entries WHERE type = 'dir' ORDER BY path")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap()
}

async fn fill(b: &Bench) {
    for d in ["/Imports", "/Imports/A", "/Imports/B", "/Coffre", "/Contacts", "/Partages", "/Partages/De_bob", "/.cache", "/Images"] {
        put_dir(b, d).await;
    }
    put(b, "/Imports/A/photo1.jpg", "image/jpeg", 1000, JUNE_2003).await;
    put(b, "/Imports/A/photo2.JPG", "application/octet-stream", 2000, NOV_2018).await;
    put(b, "/Imports/A/doc.pdf", "application/pdf", 300, NOV_2018).await;
    put(b, "/Imports/B/song.mp3", "audio/mpeg", 4000, NOV_2018).await;
    put(b, "/Imports/B/film.mkv", "video/x-matroska", 90_000, NOV_2018).await;
    put(b, "/Imports/B/backup.zip", "application/zip", 500, NOV_2018).await;
    put(b, "/Imports/B/mystery.xyz", "application/octet-stream", 9, NOV_2018).await;
    put(b, "/top.txt", "text/plain", 5, NOV_2018).await;
    put(b, "/Coffre/perso.kdbx", "application/x-keepass2", 7, NOV_2018).await;
    put(b, "/Contacts/x.vcf", "text/vcard", 7, NOV_2018).await;
    put(b, "/Partages/De_bob/p.jpg", "image/jpeg", 7, NOV_2018).await;
    put(b, "/.cache/x.jpg", "image/jpeg", 7, NOV_2018).await;
    put(b, "/Images/already.jpg", "image/jpeg", 7, NOV_2018).await;
}

#[test]
fn files_are_classified_by_mime_then_by_extension_and_unknown_ones_stay() {
    for (mime, name, want) in [
        ("image/jpeg", "a.jpg", Some(Kind::Images)), ("application/octet-stream", "IMG_1.HEIC", Some(Kind::Images)), ("audio/flac", "x", Some(Kind::Music)), ("video/mp4", "x.bin", Some(Kind::Videos)),
        ("application/pdf", "x", Some(Kind::Documents)), ("text/plain; charset=utf-8", "n.txt", Some(Kind::Documents)), ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "x", Some(Kind::Documents)),
        ("application/zip", "x.zip", Some(Kind::Archives)), ("application/octet-stream", "x.7z", Some(Kind::Archives)), ("application/octet-stream", "mystère.xyz", None), ("text/vcard", "c.vcf", None), ("", "sans-extension", None),
    ] {
        assert_eq!(classify(mime, name), want, "{mime} {name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_preview_counts_what_would_move_and_protects_what_must_stay() {
    let b = bench("plan").await;
    fill(&b).await;
    let p = organize::plan(&b.dav.db, &hex(&ALICE), &Options::default()).await.unwrap();
    assert_eq!(p.moves.len(), 7, "{:?}", p.moves);
    assert_eq!((p.unknown, p.protected, p.placed), (1, 4, 1), "inconnu, coffre/contacts/partages/caché, déjà rangé");
    let to: std::collections::BTreeMap<_, _> = p.moves.iter().cloned().collect();
    assert_eq!(to["/Imports/A/photo1.jpg"], "/Images/photo1.jpg");
    assert_eq!(to["/Imports/A/photo2.JPG"], "/Images/photo2.JPG");
    assert_eq!(to["/Imports/A/doc.pdf"], "/Documents/doc.pdf");
    assert_eq!(to["/top.txt"], "/Documents/top.txt");
    assert_eq!(to["/Imports/B/song.mp3"], "/Musique/song.mp3");
    assert_eq!(to["/Imports/B/film.mkv"], "/Vidéos/film.mkv");
    assert_eq!(to["/Imports/B/backup.zip"], "/Archives/backup.zip");
    assert_eq!(p.by_kind[&Kind::Images], (2, 3000));
    // Un aperçu n'écrit rien.
    assert_eq!(files(&b).await.len(), 13);

    // Réglages : un seul type, un seul dossier, dossier protégé refusé.
    let only = organize::plan(&b.dav.db, &hex(&ALICE), &Options { scope: "/Imports/B".into(), kinds: vec!["music".into()], ..Options::default() }).await.unwrap();
    assert_eq!(only.moves, [("/Imports/B/song.mp3".to_owned(), "/Musique/song.mp3".to_owned())]);
    for bad in [Options { scope: "/Coffre".into(), ..Options::default() }, Options { scope: "/.cache".into(), ..Options::default() }, Options { kinds: vec![], ..Options::default() }] {
        assert!(organize::plan(&b.dav.db, &hex(&ALICE), &bad).await.is_err(), "{bad:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn names_never_collide_and_dates_make_subfolders() {
    let b = bench("names").await;
    put_dir(&b, "/Images").await;
    put(&b, "/Images/IMG_1.jpg", "image/jpeg", 1, NOV_2018).await;
    put(&b, "/Vrac/a/IMG_1.jpg", "image/jpeg", 1, JUNE_2003).await;
    put(&b, "/Vrac/b/IMG_1.jpg", "image/jpeg", 1, NOV_2018).await;
    put(&b, "/Vrac/c/IMG_1.jpg", "image/jpeg", 1, 0).await;
    let flat = organize::plan(&b.dav.db, &hex(&ALICE), &Options::default()).await.unwrap();
    let mut got: Vec<String> = flat.moves.iter().map(|m| m.1.clone()).collect();
    got.sort();
    assert_eq!(got, ["/Images/IMG_1 (2).jpg", "/Images/IMG_1 (3).jpg", "/Images/IMG_1 (4).jpg"], "un nom pris reçoit un numéro, aucun écrasement");

    let month = organize::plan(&b.dav.db, &hex(&ALICE), &Options { by_date: ByDate::Month, ..Options::default() }).await.unwrap();
    let to: std::collections::BTreeMap<_, _> = month.moves.iter().cloned().collect();
    assert_eq!(to["/Vrac/a/IMG_1.jpg"], "/Images/2003/06/IMG_1.jpg");
    assert_eq!(to["/Vrac/b/IMG_1.jpg"], "/Images/2018/11/IMG_1.jpg");
    assert_eq!(to["/Vrac/c/IMG_1.jpg"], "/Images/Sans date/IMG_1.jpg", "sans date connue");
    // Sans date de prise de vue, le dossier d'origine fait foi (…/1980/01/…), avant la date de modification (celle de l'import).
    b.dav.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, data) VALUES (?1, '/Import/1980/01/PHOTO_1.jpg', '/Import/1980/01', 'file', 'c1', 'image/jpeg', 1, ?2, NULL, '{}'), (?1, '/Import/1999/vrac/PHOTO_2.jpg', '/Import/1999/vrac', 'file', 'c2', 'image/jpeg', 1, ?2, NULL, '{}'), (?1, '/Import/sans/date/PHOTO_3.jpg', '/Import/sans/date', 'file', 'c3', 'image/jpeg', 1, ?2, NULL, '{}')", vec![V::Text(hex(&ALICE)), V::Integer(NOV_2018)]).await.unwrap();
    let from_path = organize::plan(&b.dav.db, &hex(&ALICE), &Options { by_date: ByDate::Month, ..Options::default() }).await.unwrap();
    let to: std::collections::BTreeMap<_, _> = from_path.moves.iter().cloned().collect();
    assert_eq!(to["/Import/1980/01/PHOTO_1.jpg"], "/Images/1980/01/PHOTO_1.jpg", "année et mois du dossier");
    assert_eq!(to["/Import/1999/vrac/PHOTO_2.jpg"], "/Images/1999/Sans mois/PHOTO_2.jpg", "année seule");
    assert_eq!(to["/Import/sans/date/PHOTO_3.jpg"], "/Images/2018/11/PHOTO_3.jpg", "à défaut, la date de modification");
    let year = organize::plan(&b.dav.db, &hex(&ALICE), &Options { by_date: ByDate::Year, ..Options::default() }).await.unwrap();
    assert!(year.moves.iter().any(|m| m.1 == "/Images/2003/IMG_1.jpg"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_moves_entries_keeps_faces_prunes_empty_folders_and_can_be_undone() {
    let b = bench("run").await;
    fill(&b).await;
    let owner = hex(&ALICE);
    for (n, path) in ["/Imports/A/photo1.jpg", "/Imports/A/photo2.JPG"].into_iter().enumerate() {
        b.dav.db.writer().execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path, source_path) VALUES (?1, 'Marie', ?2, 1, ?3, ?3)", vec![V::Text(owner.clone()), V::Text(format!("h{n}")), V::Text(path.into())]).await.unwrap();
    }
    let opts = Options::default();
    let plan = organize::plan(&b.dav.db, &owner, &opts).await.unwrap();
    let mut seen = Vec::new();
    let report = organize::apply(&b.dav, &owner, &opts, &plan, None, |p| seen.push(p)).await.unwrap();
    assert_eq!((report.moved, report.errors.len(), report.stopped), (7, 0, false), "{report:?}");
    assert!(seen.last().is_some_and(|p| p.done == 7 && p.total == 7 && p.moved == 7), "{seen:?}");

    assert_eq!(files(&b).await, ["/.cache/x.jpg", "/Archives/backup.zip", "/Coffre/perso.kdbx", "/Contacts/x.vcf", "/Documents/doc.pdf", "/Documents/top.txt", "/Images/already.jpg", "/Images/photo1.jpg", "/Images/photo2.JPG", "/Imports/B/mystery.xyz", "/Musique/song.mp3", "/Partages/De_bob/p.jpg", "/Vidéos/film.mkv"]);
    // /Imports/A est vide : supprimé ; /Imports/B garde son fichier inconnu ; les dossiers protégés restent.
    let d = dirs(&b).await;
    assert!(!d.contains(&"/Imports/A".to_owned()) && d.contains(&"/Imports/B".to_owned()) && d.contains(&"/Coffre".to_owned()) && d.contains(&"/Images".to_owned()), "{d:?}");
    assert_eq!(report.pruned, 1);
    // Les entrées ont gardé leur blob : seul le chemin change.
    let cid: String = b.dav.db.readers().read(|c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE path = '/Documents/doc.pdf'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(cid, "cid-/Imports/A/doc.pdf");
    // Les visages suivent leur photo.
    let faces: Vec<String> = b.dav.db.readers().read(|c| Ok(c.prepare("SELECT ucloud_path || '|' || source_path FROM faces ORDER BY embed_hash")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
    assert_eq!(faces, ["/Images/photo1.jpg|/Images/photo1.jpg", "/Images/photo2.JPG|/Images/photo2.JPG"]);

    // Rejeu : plus rien à ranger.
    let again = organize::plan(&b.dav.db, &owner, &opts).await.unwrap();
    assert!(again.moves.is_empty() && again.placed >= 8, "{again:?}");

    // Annulation : chaque fichier retrouve sa place, les dossiers sont recréés, les visages suivent.
    let back = organize::undo(&b.dav, &owner, &report.run_id).await.unwrap();
    assert_eq!(back, 7);
    let f = files(&b).await;
    assert!(f.contains(&"/Imports/A/photo1.jpg".to_owned()) && f.contains(&"/top.txt".to_owned()) && f.contains(&"/Imports/B/film.mkv".to_owned()) && !f.contains(&"/Documents/doc.pdf".to_owned()), "{f:?}");
    assert!(dirs(&b).await.contains(&"/Imports/A".to_owned()), "dossier recréé");
    let faces: Vec<String> = b.dav.db.readers().read(|c| Ok(c.prepare("SELECT ucloud_path FROM faces ORDER BY embed_hash")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
    assert_eq!(faces, ["/Imports/A/photo1.jpg", "/Imports/A/photo2.JPG"]);
    assert!(organize::undo(&b.dav, &owner, &report.run_id).await.is_err(), "déjà annulé");
    assert!(organize::undo(&b.dav, &owner, "inconnu").await.is_err());
    // Une place reprise entre-temps : le fichier revient sous un numéro, rien n'est écrasé.
    let again = organize::plan(&b.dav.db, &owner, &opts).await.unwrap();
    let second = organize::apply(&b.dav, &owner, &opts, &again, None, |_| {}).await.unwrap();
    put(&b, "/Imports/A/photo1.jpg", "image/jpeg", 1, NOV_2018).await;
    organize::undo(&b.dav, &owner, &second.run_id).await.unwrap();
    let f = files(&b).await;
    assert!(f.contains(&"/Imports/A/photo1.jpg".to_owned()) && f.contains(&"/Imports/A/photo1 (2).jpg".to_owned()), "{f:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_service_runs_in_the_background_and_allows_one_pass_per_account() {
    let b = bench("service").await;
    fill(&b).await;
    let owner = hex(&ALICE);
    let bus = ActivityBus::new();
    let service = OrganizeService::new(b.dav.clone(), Some(bus.clone()));
    let preview = service.preview(&owner, &Options::default()).await.unwrap();
    assert_eq!(preview["total"], 7);
    let images = preview["by_kind"].as_array().unwrap().iter().find(|k| k["kind"] == "images").unwrap();
    assert_eq!((images["files"].as_u64(), images["folder"].as_str()), (Some(2), Some("Images")));
    assert!(images["sample"].as_array().unwrap().len() == 2, "{images}");
    assert!(service.stop(&owner).is_err(), "rien à arrêter");
    assert!(service.run(&owner, Options { kinds: vec![], ..Options::default() }).await.is_err());

    service.run(&owner, Options::default()).await.unwrap();
    let mut last = serde_json::Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        last = service.status(&owner).await.unwrap();
        if last["job"]["state"] == "done" {
            break;
        }
    }
    assert_eq!(last["job"]["state"], "done", "{last}");
    assert_eq!((last["job"]["report"]["moved"].as_u64(), last["job"]["report"]["errors"].as_u64()), (Some(7), Some(0)), "{last}");
    assert_eq!(last["runs"][0]["moved"], 7);
    let said: Vec<_> = bus.recent(10).into_iter().filter(|a| a.kind == "organize").collect();
    assert!(said.len() == 1 && said[0].message.contains("7 fichier(s) rangé(s)"), "{said:?}");

    // Annulation par le service.
    let run = last["runs"][0]["run_id"].as_str().unwrap().to_owned();
    assert_eq!(service.undo(&owner, &run).await.unwrap(), 7);
    assert!(files(&b).await.contains(&"/Imports/A/doc.pdf".to_owned()));
}
