//! Dépôt de films et de séries de bout en bout, avec les vrais `ffprobe`/`ffmpeg` : copie privée attestée, plafond,
//! piste audio française, encodage H.264/AAC, uCloud chiffré, événement NIP-71 (kind 21/22), rejeu.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_bro::broker::Publisher;
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::media::MediaMeta;
use astro_dav::{DavConfig, DavState, FsBlobStore};
use astro_db::{Db, SqliteReplayStore};
use astro_nostr_proto::{Event, kind, pubkey_of};
use astroportd::media::{MediaFailure, MediaPipeline, Tools, Transcode};
use rusqlite::types::Value as V;
use serde_json::json;

const ALICE: [u8; 32] = [0xA1; 32];

#[derive(Default)]
struct Sink(Mutex<Vec<Event>>);

impl Publisher for Sink {
    async fn publish(&self, event: Event) -> Result<(), String> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

fn have_ffmpeg() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|t| Command::new(t).arg("-version").output().is_ok_and(|o| o.status.success()))
        && Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("libx264"))
}

/// Un MKV de 2 s : vidéo MPEG-4, deux pistes MP3 (anglais puis français).
fn make_video(path: &Path) {
    let st = Command::new("ffmpeg")
        .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=2:size=160x120:rate=10", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-f", "lavfi", "-i", "sine=frequency=880:duration=2"])
        .args(["-map", "0:v", "-map", "1:a", "-map", "2:a", "-c:v", "mpeg4", "-c:a", "libmp3lame", "-metadata:s:a:0", "language=eng", "-metadata:s:a:1", "language=fra"])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
}

struct Bench {
    db: Arc<Db>,
    dav: Arc<DavState<FsBlobStore>>,
    keys: Arc<dyn KeySource>,
    sink: Arc<Sink>,
    dir: std::path::PathBuf,
    owner: String,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn bench(tag: &str) -> Bench {
    let dir = std::env::temp_dir().join(format!("astroportd-media-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    let owner = pubkey_of(&ALICE).unwrap();
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@example.org', 1)", vec![V::Text(owner.clone())]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: None, config: DavConfig::new(b"secret".to_vec()), on_upload: None });
    Bench { db, dav, keys: Arc::new(StaticKeySource::new().with(ALICE)), sink: Arc::new(Sink::default()), dir, owner }
}

impl Bench {
    fn pipeline(&self, mode: Transcode, tools: Tools) -> MediaPipeline<FsBlobStore, Sink> {
        MediaPipeline { dav: self.dav.clone(), keys: self.keys.clone(), publisher: Some(self.sink.clone()), activity: None, tools, mode, work_dir: self.dir.join("work") }
    }

    async fn count(&self, sql: &'static str) -> i64 {
        self.db.readers().read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?)).await.unwrap()
    }
}

fn episode() -> MediaMeta {
    MediaMeta::from_tmdb(&json!({ "series_name": "Test", "title": "Test", "season_number": 1, "episode_number": 2, "episode_name": "Pilote", "genres": ["Science-Fiction"], "id": 99, "overview": "Un épisode." })).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_deposited_or_published_without_the_private_copy_attestation() {
    let b = bench("consent").await;
    let p = b.pipeline(Transcode::Notify, Tools::default());
    let err = p.submit(&b.owner, Path::new("/nonexistent.mkv"), &episode()).await.unwrap_err();
    assert!(matches!(err, MediaFailure::NoConsent), "{err}");
    assert!(b.sink.0.lock().unwrap().is_empty());

    // Attestation : événement 31903 signé du titulaire, stocké, projeté, publié.
    let ev = p.record_consent(&b.owner).await.unwrap();
    assert_eq!((ev.kind, ev.pubkey.as_str(), ev.tag_value("d")), (kind::APP_PARAMS, b.owner.as_str(), Some("private_copy")));
    assert!(ev.content.contains("L. 122-5"));
    assert!(p.has_consent(&b.owner).await.unwrap());
    assert_eq!(b.sink.0.lock().unwrap().len(), 1);

    // La base est un cache : après un replay complet, l'attestation revient depuis l'événement Nostr.
    let stats = astro_sync::replay_local(&b.db, 100).await.unwrap();
    assert_eq!(stats.skipped, 0, "{:?}", stats.errors);
    assert!(p.has_consent(&b.owner).await.unwrap(), "rejouable");

    // Un faux : texte légal réécrit, ou signé d'un autre compte, ne vaut rien.
    let forged = astro_nostr_proto::UnsignedEvent { created_at: 1_800_000_001, kind: kind::APP_PARAMS, tags: astro_dav::media::consent_tags(), content: json!({ "v": 1, "text": "ok" }).to_string() }.sign(&[0xC3; 32]).unwrap();
    astro_sync::ingest_event(b.db.writer(), forged, &b.owner).await.unwrap();
    assert_eq!(b.count("SELECT COUNT(*) FROM private_copy_consent").await, 1);
    // Sans clé détenue ici, pas d'attestation possible.
    assert!(matches!(p.record_consent(&pubkey_of(&[0xC3; 32]).unwrap()).await.unwrap_err(), MediaFailure::NoKey));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_series_episode_is_encoded_with_the_french_track_deposited_encrypted_and_published_to_n1() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg/libx264 absent : test ignoré");
        return;
    }
    let b = bench("e2e").await;
    // `ffmpeg` enveloppé : on journalise ses arguments pour prouver le choix de la piste française.
    let log = b.dir.join("ffmpeg.args");
    let wrapper = b.dir.join("ffmpeg-wrapper.sh");
    std::fs::write(&wrapper, format!("#!/bin/sh\necho \"$@\" >> '{}'\nexec ffmpeg \"$@\"\n", log.display())).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let tools = Tools { ffmpeg: wrapper.to_string_lossy().into_owned(), ..Tools::default() };
    let p = b.pipeline(Transcode::Auto, tools);
    p.record_consent(&b.owner).await.unwrap();

    let src = b.dir.join("source.mkv");
    make_video(&src);
    let report = p.submit(&b.owner, &src, &episode()).await.unwrap();
    assert!(report.transcoded, "{:?}", report.notes);
    assert_eq!(report.path, "/Vidéos/Séries/Test/Saison 01/Test S01E02.mp4");
    assert_eq!(report.kind, 22, "2 s : vidéo courte");
    let args = std::fs::read_to_string(&log).unwrap();
    assert!(args.contains("-map 0:v:0 -map 0:a:1") && args.contains("libx264") && args.contains("-c:a aac"), "{args}");
    assert!(std::fs::read_dir(b.dir.join("work")).unwrap().next().is_none(), "le fichier de travail est supprimé");

    // uCloud : fichier chiffré (blob IPFS), type MP4, dossiers créés.
    let row: (String, String, i64) = b.db.readers().read(|c| Ok(c.query_row("SELECT mime, cid, size FROM ucloud_entries WHERE path = '/Vidéos/Séries/Test/Saison 01/Test S01E02.mp4'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?)).await.unwrap();
    assert_eq!(row.0, "video/mp4");
    assert_eq!(b.count("SELECT COUNT(*) FROM ucloud_entries WHERE type = 'dir' AND path IN ('/Vidéos', '/Vidéos/Séries', '/Vidéos/Séries/Test', '/Vidéos/Séries/Test/Saison 01')").await, 4);

    // Événement publié : le dernier de la sonde (attestation d'abord, vidéo ensuite), signé du titulaire.
    let events = b.sink.0.lock().unwrap().clone();
    let video = events.iter().find(|e| e.kind == 22).unwrap();
    assert_eq!(video.pubkey, b.owner);
    assert!(video.verify().is_ok());
    for (k, v) in [("series_name", "Test"), ("season_number", "1"), ("episode_number", "2"), ("episode_name", "Pilote"), ("duration", "2"), ("encrypted", "uenc-v2")] {
        assert_eq!(video.tag_value(k), Some(v), "{k}");
    }
    assert!(video.tags.iter().any(|t| t == &["l", "Science-Fiction", "genre"]) && video.tags.iter().any(|t| t == &["-"]));
    assert_eq!(video.tag_value("url"), Some(format!("/ipfs/{}", row.1).as_str()));
    assert_eq!(video.content, "Un épisode.");
    assert_eq!(b.count("SELECT COUNT(*) FROM nostr_events WHERE kind = 22").await, 1, "stocké sur le relai local");
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_mode_deposits_as_is_recommends_encoding_and_never_deposits_twice() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg/libx264 absent : test ignoré");
        return;
    }
    let b = bench("notify").await;
    let p = b.pipeline(Transcode::Notify, Tools::default());
    p.record_consent(&b.owner).await.unwrap();
    let src = b.dir.join("film.mkv");
    make_video(&src);
    let film = MediaMeta::from_tmdb(&json!({ "title": "Le Grand Bleu", "release_date": "1988-05-11", "genres": [{ "id": 18, "name": "Drame" }] })).unwrap();
    let report = p.submit(&b.owner, &src, &film).await.unwrap();
    assert!(!report.transcoded);
    assert!(report.notes.iter().any(|n| n.contains("recommandé") && n.contains("H.264")), "{:?}", report.notes);
    assert_eq!(report.path, "/Vidéos/Films/Le Grand Bleu (1988).mkv");
    assert!(!b.dir.join("work").exists(), "aucun encodage, aucun fichier de travail");
    let again = p.submit(&b.owner, &src, &film).await.unwrap_err();
    assert!(matches!(again, MediaFailure::Duplicate(ref path) if path == "/Vidéos/Films/Le Grand Bleu (1988).mkv"), "{again}");
    assert_eq!(b.count("SELECT COUNT(*) FROM nostr_events WHERE kind IN (21, 22)").await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_frugal_cap_and_missing_tools_are_refused_loudly() {
    let b = bench("cap").await;
    let p = b.pipeline(Transcode::Notify, Tools::default());
    p.record_consent(&b.owner).await.unwrap();
    let big = b.dir.join("enorme.mkv");
    std::fs::File::create(&big).unwrap().set_len((1024 << 20) + 1).unwrap(); // fichier creux : rien n'est écrit sur disque
    let err = p.submit(&b.owner, &big, &episode()).await.unwrap_err();
    assert!(err.to_string().contains("plafond de 1024 Mo"), "{err}");

    let small = b.dir.join("petit.mkv");
    std::fs::write(&small, "pas une vidéo".as_bytes()).unwrap();
    let lost = b.pipeline(Transcode::Notify, Tools { ffprobe: "/nonexistent/ffprobe".into(), ..Tools::default() });
    let err = lost.submit(&b.owner, &small, &episode()).await.unwrap_err();
    assert!(matches!(err, MediaFailure::Tool(ref m) if m.contains("introuvable")), "{err}");
    if have_ffmpeg() {
        let err = p.submit(&b.owner, &small, &episode()).await.unwrap_err();
        assert!(matches!(err, MediaFailure::Tool(_) | MediaFailure::Media(_)), "un fichier qui n'est pas une vidéo est refusé : {err}");
    }
    // Délai : un outil qui ne rend jamais la main est tué.
    let sleeper = b.dir.join("sleeper.sh");
    std::fs::write(&sleeper, "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&sleeper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let slow = b.pipeline(Transcode::Notify, Tools { ffprobe: sleeper.to_string_lossy().into_owned(), probe_timeout: Duration::from_millis(300), ..Tools::default() });
    let started = std::time::Instant::now();
    let err = slow.submit(&b.owner, &small, &episode()).await.unwrap_err();
    assert!(err.to_string().contains("délai"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(b.count("SELECT COUNT(*) FROM ucloud_entries WHERE type = 'file'").await, 0, "rien de déposé");
}
