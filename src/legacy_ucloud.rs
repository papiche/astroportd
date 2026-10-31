//! Reprise du uCloud d'Astroport.ONE (`~/.zen/game/nostr/<email>/.ucloud/ucloud.db`) dans astroportd.
//!
//! Rien n'est recopié ni ré-encodé : le nouveau VFS **référence les mêmes blobs chiffrés** (UENC v1, déjà dans IPFS) avec
//! **les mêmes clés**. astroportd sait lire l'UENC v1 ; seule condition, un `--kubo` pour que `serve` atteigne ces CID.
//! L'ancienne base n'est ouverte qu'en lecture seule, dans une transaction (instantané cohérent) : elle n'est jamais modifiée.
//!
//! Un chemin qui ne passerait pas la validation du manifeste, ou un fichier sans clé, est **écarté et dit** (jamais
//! avalé) : un import de 30 000 photos ne doit pas échouer en bloc pour une entrée. Les dossiers parents manquants sont créés.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use astro_crypto::{UencKey, uenc};
use astro_dav::blob::BlobStore;
use astro_dav::manifest::{KeyringEntry, Manifest, ManifestEntry};
use astro_dav::vfs::{normalize_path, parent_of};
use astro_db::Db;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// Provenance des lignes importées (`src_cid`).
pub const SOURCE: &str = "legacy-import";
/// Algorithme inscrit au trousseau pour les clés reprises (celui d'Astroport.ONE).
const LEGACY_ENC: &str = "uenc-aes256gcm";

/// Échec de l'import.
#[derive(Debug, thiserror::Error)]
pub enum LegacyError {
    /// Ancienne base illisible.
    #[error("ancienne base : {0}")]
    Legacy(String),
    /// Base d'astroportd.
    #[error("base : {0}")]
    Db(String),
    /// Le compte a déjà des fichiers dans astroportd.
    #[error("le compte a déjà {0} entrées dans astroportd : relancer avec --replace pour les remplacer par l'ancien uCloud")]
    NotEmpty(i64),
    /// Manifeste refusé.
    #[error("validation : {0}")]
    Invalid(String),
}

/// Ce qui a été lu, repris ou écarté.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Fichiers repris.
    pub files: usize,
    /// Dossiers repris (dossiers parents créés compris).
    pub dirs: usize,
    /// Dossiers créés parce que l'ancienne base ne les listait pas.
    pub dirs_created: usize,
    /// Octets de clair des fichiers repris.
    pub bytes: u64,
    /// Entrées écartées : `(chemin, raison)`.
    pub skipped: Vec<(String, String)>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Lit l'ancien uCloud de `owner` et en fait un manifeste prêt à être appliqué.
///
/// # Errors
/// Ancienne base absente ou illisible.
pub fn read_legacy(legacy_db: &Path, owner: &str) -> Result<(Manifest, Report), LegacyError> {
    let conn = Connection::open_with_flags(legacy_db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| LegacyError::Legacy(format!("{} : {e}", legacy_db.display())))?;
    conn.busy_timeout(std::time::Duration::from_secs(30)).map_err(|e| LegacyError::Legacy(e.to_string()))?;
    conn.execute_batch("BEGIN").map_err(|e| LegacyError::Legacy(e.to_string()))?;
    let keys: HashMap<String, String> = {
        let mut s = conn.prepare("SELECT cid, data FROM keyring").map_err(|e| LegacyError::Legacy(e.to_string()))?;
        let rows = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| LegacyError::Legacy(e.to_string()))?;
        let mut m = HashMap::new();
        for row in rows {
            let (cid, data) = row.map_err(|e| LegacyError::Legacy(e.to_string()))?;
            if let Some(k) = serde_json::from_str::<serde_json::Value>(&data).ok().and_then(|v| v.get("key_hex").and_then(|k| k.as_str().map(str::to_owned))) {
                m.insert(cid, k);
            }
        }
        m
    };
    let mut s = conn
        .prepare("SELECT path, type, cid, mime, size, mtime, taken_at, sha_orig, sha_plain, faceid_status, origin, phash, lat, lon, desc_text, data FROM entries ORDER BY path")
        .map_err(|e| LegacyError::Legacy(e.to_string()))?;
    let rows = s
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                ManifestEntry {
                    path: String::new(),
                    parent: String::new(),
                    kind: String::new(),
                    cid: r.get(2)?,
                    mime: r.get(3)?,
                    size: r.get(4)?,
                    mtime: r.get(5)?,
                    taken_at: r.get(6)?,
                    sha_orig: r.get(7)?,
                    sha_plain: r.get(8)?,
                    faceid_status: r.get(9)?,
                    origin: r.get(10)?,
                    phash: r.get(11)?,
                    lat: r.get(12)?,
                    lon: r.get(13)?,
                    desc_text: r.get(14)?,
                    data: r.get::<_, Option<String>>(15)?.unwrap_or_else(|| "{}".into()),
                },
            ))
        })
        .map_err(|e| LegacyError::Legacy(e.to_string()))?;

    let mut report = Report::default();
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut used: HashSet<String> = HashSet::new();
    for row in rows {
        let (raw_path, kind, mut e) = row.map_err(|e| LegacyError::Legacy(e.to_string()))?;
        let skip = |report: &mut Report, why: &str| report.skipped.push((raw_path.clone(), why.to_owned()));
        if raw_path.is_empty() || raw_path == "/" {
            continue; // la racine existe déjà dans astroportd
        }
        let path = match normalize_path(&raw_path) {
            Ok(p) if p == raw_path => p,
            _ => {
                skip(&mut report, "chemin non normalisé");
                continue;
            }
        };
        if !seen.insert(path.clone()) {
            skip(&mut report, "chemin en double");
            continue;
        }
        if !matches!(kind.as_str(), "file" | "dir") {
            skip(&mut report, "type d'entrée inconnu");
            continue;
        }
        if kind == "file" {
            let Some(cid) = e.cid.clone().filter(|c| !c.is_empty()) else {
                skip(&mut report, "fichier sans CID");
                continue;
            };
            if !keys.get(&cid).is_some_and(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit())) {
                skip(&mut report, "fichier sans clé de déchiffrement au trousseau");
                continue;
            }
            used.insert(cid);
            report.files += 1;
            report.bytes += u64::try_from(e.size.unwrap_or(0)).unwrap_or(0);
        } else {
            e.cid = None; // un dossier n'a pas de blob
            report.dirs += 1;
        }
        e.parent = parent_of(&path);
        e.path = path;
        e.kind = kind;
        entries.push(e);
    }
    drop(s);
    conn.execute_batch("COMMIT").map_err(|e| LegacyError::Legacy(e.to_string()))?;

    // Tout parent doit exister, sinon le fichier serait introuvable en parcourant l'arborescence.
    let dirs: HashSet<String> = entries.iter().filter(|e| e.kind == "dir").map(|e| e.path.clone()).collect();
    let mut missing: Vec<String> = Vec::new();
    let mut known = dirs;
    for e in entries.clone() {
        let mut p = e.parent;
        while p != "/" && known.insert(p.clone()) {
            missing.push(p.clone());
            p = parent_of(&p);
        }
    }
    for path in missing {
        entries.push(ManifestEntry {
            parent: parent_of(&path),
            path,
            kind: "dir".into(),
            cid: None,
            mime: None,
            size: None,
            mtime: None,
            taken_at: None,
            sha_orig: None,
            sha_plain: None,
            faceid_status: None,
            origin: None,
            phash: None,
            lat: None,
            lon: None,
            desc_text: None,
            data: r#"{"type":"dir"}"#.into(),
        });
        report.dirs += 1;
        report.dirs_created += 1;
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let mut keyring: Vec<KeyringEntry> = used.into_iter().filter_map(|cid| keys.get(&cid).map(|k| KeyringEntry { cid, key_hex: k.to_ascii_lowercase(), enc: LEGACY_ENC.into() })).collect();
    keyring.sort_by(|a, b| a.cid.cmp(&b.cid));
    Ok((Manifest { v: astro_dav::manifest::VERSION, owner: owner.to_owned(), rev: 0, created_at: now(), entries, keyring }, report))
}

/// Applique `manifest` à la base d'astroportd, en une seule transaction. Refuse si le compte a déjà des entrées, sauf `replace`.
///
/// # Errors
/// Compte non vide sans `replace`, manifeste invalide ou base indisponible.
pub async fn apply(db: &Db, manifest: &Manifest, replace: bool) -> Result<(), LegacyError> {
    let owner = manifest.owner.clone();
    let existing: i64 = db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1", [owner], |r| r.get(0))?)).await.map_err(|e| LegacyError::Db(e.to_string()))?;
    if existing > 0 && !replace {
        return Err(LegacyError::NotEmpty(existing));
    }
    let statements = astro_dav::manifest::restore_statements(manifest, SOURCE).map_err(|e| LegacyError::Invalid(e.to_string()))?;
    db.writer().batch(statements).await.map_err(|e| LegacyError::Db(e.to_string()))?;
    Ok(())
}

/// Résultat du contrôle par échantillon.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Verified {
    /// Fichiers lus, déchiffrés et comparés.
    pub checked: usize,
    /// Écarts : `(chemin, raison)`.
    pub failures: Vec<(String, String)>,
}

/// Lit `sample` fichiers répartis sur tout le manifeste (petits et gros), les déchiffre avec leur clé et compare taille et SHA-256
/// du clair à ce que l'ancien uCloud annonçait. Prouve que les blobs sont atteignables **et** que les clés reprises sont les bonnes.
pub async fn verify_sample<B: BlobStore>(blobs: &B, manifest: &Manifest, sample: usize) -> Verified {
    let keys: HashMap<&str, &str> = manifest.keyring.iter().map(|k| (k.cid.as_str(), k.key_hex.as_str())).collect();
    let files: Vec<&ManifestEntry> = manifest.entries.iter().filter(|e| e.kind == "file").collect();
    let mut out = Verified::default();
    if files.is_empty() || sample == 0 {
        return out;
    }
    let step = (files.len() / sample).max(1);
    for e in files.iter().step_by(step).take(sample) {
        let fail = |out: &mut Verified, why: String| out.failures.push((e.path.clone(), why));
        let (Some(cid), Some(key_hex)) = (e.cid.as_deref(), e.cid.as_deref().and_then(|c| keys.get(c))) else {
            fail(&mut out, "CID ou clé absents".into());
            continue;
        };
        let key = match UencKey::from_hex(key_hex) {
            Ok(k) => k,
            Err(err) => {
                fail(&mut out, format!("clé illisible : {err}"));
                continue;
            }
        };
        let (mut reader, _) = match blobs.open(cid).await {
            Ok(r) => r,
            Err(err) => {
                fail(&mut out, format!("blob inaccessible : {err}"));
                continue;
            }
        };
        let mut sink = HashingSink::default();
        match uenc::decrypt_stream(&key, &mut reader, &mut sink).await {
            Ok(n) => {
                let sha = hex::encode(sink.hasher.clone().finalize());
                if e.size.is_some_and(|s| u64::try_from(s).ok() != Some(n)) {
                    fail(&mut out, format!("taille déchiffrée {n} ≠ {:?}", e.size));
                } else if e.sha_plain.as_deref().is_some_and(|s| !s.eq_ignore_ascii_case(&sha)) {
                    fail(&mut out, "SHA-256 du clair différent de celui de l'ancien uCloud".into());
                } else {
                    out.checked += 1;
                }
            }
            Err(err) => fail(&mut out, format!("déchiffrement : {err}")),
        }
    }
    out
}

/// Écrivain qui ne garde que l'empreinte du clair.
#[derive(Default)]
struct HashingSink {
    hasher: Sha256,
}

impl tokio::io::AsyncWrite for HashingSink {
    fn poll_write(mut self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        self.hasher.update(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// Écrit `skipped` dans `path`, une ligne `chemin<TAB>raison`, pour qu'aucun écart ne reste invisible.
///
/// # Errors
/// Fichier non écrivable.
pub async fn write_skipped(path: &Path, skipped: &[(String, String)]) -> std::io::Result<()> {
    let mut f = tokio::fs::File::create(path).await?;
    for (p, why) in skipped {
        f.write_all(format!("{p}\t{why}\n").as_bytes()).await?;
    }
    f.flush().await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use astro_crypto::uenc::v1;
    use astro_dav::FsBlobStore;
    use astro_dav::blob::BlobWriter;

    const OWNER: &str = "71b4b56ff54c677a0ab096e583953b6b1700710f5e2cad62e8baf2afad67b7b4";

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("astroportd-legacy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Un uCloud d'Astroport.ONE minimal : un dossier, une photo UENC v1 avec sa clé, et des entrées à écarter.
    async fn legacy_fixture(dir: &Path, blobs: &FsBlobStore) -> (std::path::PathBuf, String, String) {
        let key = UencKey::from_bytes([9; 32]);
        let plain = b"contenu de la photo".to_vec();
        let blob = v1::encrypt(&key, &plain).unwrap();
        let mut w = blobs.begin().await.unwrap();
        w.write_all(&blob).await.unwrap();
        let cid = w.commit().await.unwrap().cid;
        let sha = hex::encode(Sha256::digest(&plain));
        let db = dir.join("ucloud.db");
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE entries(path TEXT PRIMARY KEY, parent TEXT NOT NULL, type TEXT NOT NULL, cid TEXT, mime TEXT, size INTEGER, mtime INTEGER, taken_at INTEGER, sha_orig TEXT, sha_plain TEXT, faceid_status TEXT, origin TEXT, data TEXT, phash INTEGER, lat REAL, lon REAL, desc_text TEXT);
             CREATE TABLE keyring(cid TEXT PRIMARY KEY, data TEXT NOT NULL) WITHOUT ROWID;",
        )
        .unwrap();
        let ins = |path: &str, parent: &str, kind: &str, cid: Option<&str>, size: Option<i64>, sha: Option<&str>| {
            c.execute("INSERT INTO entries(path, parent, type, cid, mime, size, mtime, sha_plain, data) VALUES (?1, ?2, ?3, ?4, 'image/jpeg', ?5, 1700000000, ?6, '{\"enc\":\"uenc-aes256gcm\"}')", rusqlite::params![path, parent, kind, cid, size, sha]).unwrap();
        };
        ins("/Photos", "/", "dir", None, None, None);
        ins("/Photos/a.jpg", "/Photos", "file", Some(&cid), Some(plain.len() as i64), Some(&sha));
        ins("/Orphelin/b.jpg", "/Orphelin", "file", Some(&cid), Some(plain.len() as i64), Some(&sha)); // dossier non listé
        ins("/Photos/sans-cle.jpg", "/Photos", "file", Some("QmInconnu"), Some(1), None);
        ins("/Photos/sans-cid.jpg", "/Photos", "file", None, Some(1), None);
        ins("/Photos/../evade.jpg", "/Photos", "file", Some(&cid), Some(1), None);
        ins("/", "", "dir", None, None, None);
        c.execute("INSERT INTO keyring(cid, data) VALUES (?1, ?2)", rusqlite::params![cid, format!("{{\"key_hex\":\"{}\"}}", key.to_hex())]).unwrap();
        (db, cid, sha)
    }

    #[tokio::test]
    async fn legacy_files_keep_their_blob_and_key_and_every_skip_is_reported() {
        let dir = tmp("map");
        let blobs = FsBlobStore::open(dir.join("blobs")).unwrap();
        let (legacy, cid, _) = legacy_fixture(&dir, &blobs).await;
        let (m, report) = read_legacy(&legacy, OWNER).unwrap();

        assert_eq!((report.files, report.dirs, report.dirs_created), (2, 2, 1), "{report:?}");
        let paths: Vec<&str> = m.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/Orphelin", "/Orphelin/b.jpg", "/Photos", "/Photos/a.jpg"], "dossier manquant créé, racine omise");
        let reasons: HashMap<&str, &str> = report.skipped.iter().map(|(p, r)| (p.as_str(), r.as_str())).collect();
        assert_eq!(reasons.len(), 3, "{reasons:?}");
        assert!(reasons["/Photos/sans-cle.jpg"].contains("clé") && reasons["/Photos/sans-cid.jpg"].contains("CID") && reasons["/Photos/../evade.jpg"].contains("normalisé"));
        assert_eq!(m.keyring.len(), 1);
        assert_eq!((m.keyring[0].cid.as_str(), m.keyring[0].key_hex.len(), m.keyring[0].enc.as_str()), (cid.as_str(), 64, "uenc-aes256gcm"));
        let a = m.entries.iter().find(|e| e.path == "/Photos/a.jpg").unwrap();
        assert_eq!((a.cid.as_deref(), a.parent.as_str()), (Some(cid.as_str()), "/Photos"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn apply_refuses_a_non_empty_account_unless_replace_and_the_sample_decrypts_v1_blobs() {
        let dir = tmp("apply");
        let blobs = FsBlobStore::open(dir.join("blobs")).unwrap();
        let (legacy, _, _) = legacy_fixture(&dir, &blobs).await;
        let (m, _) = read_legacy(&legacy, OWNER).unwrap();
        let db = Db::open(&dir.join("u.db"), 2).unwrap();

        apply(&db, &m, false).await.unwrap();
        let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND src_cid = ?2", [OWNER, SOURCE], |r| r.get(0))?)).await.unwrap();
        let k: i64 = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_keyring WHERE owner_hex = ?1", [OWNER], |r| r.get(0))?)).await.unwrap();
        assert_eq!((n, k), (4, 1));
        assert!(matches!(apply(&db, &m, false).await, Err(LegacyError::NotEmpty(4))), "second import refusé");
        apply(&db, &m, true).await.unwrap();
        let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1", [OWNER], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 4, "remplacement sans doublon");

        // Le contrôle par échantillon déchiffre vraiment le blob v1 avec la clé reprise, et voit une clé fausse.
        let ok = verify_sample(&blobs, &m, 10).await;
        assert_eq!((ok.checked, ok.failures.len()), (2, 0), "{ok:?}");
        let mut bad = m.clone();
        bad.keyring[0].key_hex = "00".repeat(32);
        let ko = verify_sample(&blobs, &bad, 10).await;
        assert_eq!((ko.checked, ko.failures.len()), (0, 2), "{ko:?}");
        let mut tampered = m.clone();
        tampered.entries.iter_mut().for_each(|e| if e.kind == "file" { e.sha_plain = Some("ab".repeat(32)) });
        assert!(verify_sample(&blobs, &tampered, 10).await.failures.iter().all(|(_, why)| why.contains("SHA-256")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_legacy_database_is_an_error_not_an_empty_import() {
        assert!(matches!(read_legacy(Path::new("/nonexistent/ucloud.db"), OWNER), Err(LegacyError::Legacy(_))));
    }
}
