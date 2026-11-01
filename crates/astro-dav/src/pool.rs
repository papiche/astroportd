//! Bassin de blobs commun à plusieurs `astroportd` d'une même machine (plans ORIGIN / ZEN*) : tous pointent leur
//! `--blobs` vers le **même dossier**, chacun garde sa base. Un blob n'y est supprimé que si **plus aucun plan ne le
//! référence** : avant de supprimer, la station interroge la base de chacun des **pairs du bassin** (jamais l'inverse),
//! en lecture seule.
//!
//! Un pair illisible (verrou, base corrompue) fait **garder** le blob : on préfère une place perdue à un fichier perdu.
//! Un pair dont la base n'existe pas encore ne référence rien.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// Colonnes qui désignent un blob : `cid`, `*_cid`, plus la miniature rangée dans le JSON des entrées du uCloud.
const COLUMNS_SQL: &str = "SELECT m.name, p.name FROM sqlite_master m, pragma_table_info(m.name) p \
    WHERE m.type = 'table' AND m.sql NOT LIKE 'CREATE VIRTUAL%' AND (p.name = 'cid' OR p.name LIKE '%\\_cid' ESCAPE '\\')";

/// Le blob `cid` est-il référencé dans la base `db` (ouverte en lecture seule) ?
pub fn referenced_in(db: &Path, cid: &str) -> Result<bool, String> {
    if !db.exists() {
        return Ok(false);
    }
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| format!("{} : {e}", db.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(COLUMNS_SQL).map_err(|e| format!("{} : {e}", db.display()))?;
    let cols = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    for (table, column) in cols {
        let sql = format!("SELECT 1 FROM \"{table}\" WHERE \"{column}\" = ?1 LIMIT 1");
        if conn.query_row(&sql, [cid], |_| Ok(())).map(|()| true).or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(false) } else { Err(e) }).map_err(|e| format!("{} : {e}", db.display()))? {
            return Ok(true);
        }
    }
    conn.query_row("SELECT 1 FROM ucloud_entries WHERE json_extract(data, '$.thumb.cid') = ?1 LIMIT 1", [cid], |_| Ok(()))
        .map(|()| true)
        .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(false) } else { Err(e) })
        .map_err(|e| format!("{} : {e}", db.display()))
}

/// Un des pairs référence-t-il `cid` ? Une erreur de lecture compte comme « oui » (on garde) et est dite sur stderr.
pub fn referenced_by_peers(peers: &[PathBuf], cid: &str) -> bool {
    peers.iter().any(|p| match referenced_in(p, cid) {
        Ok(found) => found,
        Err(why) => {
            eprintln!("bassin de blobs : pair illisible, blob {cid} conservé ({why})");
            true
        }
    })
}

/// Tous les blobs qu'une base référence (entrées, clés, miniatures, tables à colonne `cid` / `*_cid`).
pub fn referenced_set(db: &Path) -> Result<std::collections::HashSet<String>, String> {
    let mut out = std::collections::HashSet::new();
    if !db.exists() {
        return Ok(out);
    }
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| format!("{} : {e}", db.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(COLUMNS_SQL).map_err(|e| e.to_string())?;
    let cols = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    for (table, column) in cols {
        let mut q = conn.prepare(&format!("SELECT DISTINCT \"{column}\" FROM \"{table}\" WHERE \"{column}\" IS NOT NULL")).map_err(|e| e.to_string())?;
        for cid in q.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())? {
            out.insert(cid.map_err(|e| e.to_string())?);
        }
    }
    let mut q = conn.prepare("SELECT DISTINCT json_extract(data, '$.thumb.cid') FROM ucloud_entries WHERE json_extract(data, '$.thumb.cid') IS NOT NULL").map_err(|e| e.to_string())?;
    for cid in q.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())? {
        out.insert(cid.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// État du bassin : ce qu'il pèse, ce que chaque plan en référence, ce qui est partagé, ce qui n'est à personne.
#[derive(Debug, Default, Clone)]
pub struct Status {
    /// Blobs dans le dossier.
    pub blobs: u64,
    /// Octets qu'ils occupent.
    pub bytes: u64,
    /// Par plan : base, blobs référencés présents dans le bassin, octets.
    pub per_plane: Vec<(PathBuf, u64, u64)>,
    /// Blobs référencés par au moins deux plans (la place économisée par le partage).
    pub shared_blobs: u64,
    /// Octets de ces blobs : ce qu'il aurait fallu écrire une seconde fois sans le bassin.
    pub shared_bytes: u64,
    /// Blobs que personne ne référence : chemin, taille, ancienneté en secondes.
    pub orphans: Vec<(PathBuf, u64, u64)>,
}

/// Parcourt le dossier `blobs` et interroge les bases `dbs` (lecture seule).
pub fn status(blobs: &Path, dbs: &[PathBuf]) -> Result<Status, String> {
    let sets = dbs.iter().map(|d| referenced_set(d)).collect::<Result<Vec<_>, _>>()?;
    let mut st = Status { per_plane: dbs.iter().map(|d| (d.clone(), 0, 0)).collect(), ..Status::default() };
    let now = std::time::SystemTime::now();
    for entry in std::fs::read_dir(blobs).map_err(|e| format!("{} : {e}", blobs.display()))?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if name.starts_with('.') || !meta.is_file() {
            continue;
        }
        let size = meta.len();
        st.blobs += 1;
        st.bytes += size;
        let holders: Vec<usize> = (0..sets.len()).filter(|i| sets[*i].contains(&name)).collect();
        for i in &holders {
            st.per_plane[*i].1 += 1;
            st.per_plane[*i].2 += size;
        }
        match holders.len() {
            0 => st.orphans.push((entry.path(), size, meta.modified().ok().and_then(|m| now.duration_since(m).ok()).map_or(0, |d| d.as_secs()))),
            1 => {}
            _ => {
                st.shared_blobs += 1;
                st.shared_bytes += size;
            }
        }
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn peer_db(tag: &str) -> (PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("astro-pool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("uplanet.db");
        drop(astro_db::Db::open(&path, 1).unwrap());
        let conn = Connection::open(&path).unwrap();
        (path, conn)
    }

    #[test]
    fn a_blob_is_referenced_by_an_entry_its_key_or_its_thumbnail() {
        let (path, c) = peer_db("refs");
        c.execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, data) VALUES ('aa', '/a', '/', 'file', 'cid-entry', '{\"thumb\":{\"cid\":\"cid-thumb\"}}')", []).unwrap();
        c.execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex) VALUES ('aa', 'cid-key', ?1)", ["0".repeat(64)]).unwrap();
        for cid in ["cid-entry", "cid-thumb", "cid-key"] {
            assert!(referenced_in(&path, cid).unwrap(), "{cid}");
        }
        assert!(!referenced_in(&path, "cid-autre").unwrap());
        c.execute("DELETE FROM ucloud_entries", []).unwrap();
        c.execute("DELETE FROM ucloud_keyring", []).unwrap();
        assert!(!referenced_in(&path, "cid-entry").unwrap() && !referenced_in(&path, "cid-thumb").unwrap(), "plus de référence : libérable");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_status_counts_what_is_shared_and_what_belongs_to_nobody() {
        let (a, ca) = peer_db("st-a");
        let (b, cb) = peer_db("st-b");
        let blobs = std::env::temp_dir().join(format!("astro-pool-blobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&blobs);
        std::fs::create_dir_all(blobs.join(".tmp")).unwrap();
        for (name, len) in [("seul-a", 10), ("seul-b", 20), ("partage", 100), ("orphelin", 5)] {
            std::fs::write(blobs.join(name), vec![0u8; len]).unwrap();
        }
        for (c, cids) in [(&ca, vec!["seul-a", "partage"]), (&cb, vec!["seul-b", "partage"])] {
            for (i, cid) in cids.iter().enumerate() {
                c.execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, data) VALUES ('aa', ?1, '/', 'file', ?2, '{}')", rusqlite::params![format!("/{i}"), cid]).unwrap();
            }
        }
        let s = status(&blobs, &[a.clone(), b.clone()]).unwrap();
        assert_eq!((s.blobs, s.bytes, s.shared_blobs, s.shared_bytes), (4, 135, 1, 100));
        assert_eq!((s.per_plane[0].1, s.per_plane[0].2, s.per_plane[1].1, s.per_plane[1].2), (2, 110, 2, 120));
        assert_eq!(s.orphans.len(), 1);
        assert!(s.orphans[0].0.ends_with("orphelin"));
        let _ = (std::fs::remove_dir_all(&blobs), std::fs::remove_dir_all(a.parent().unwrap()), std::fs::remove_dir_all(b.parent().unwrap()));
    }

    #[test]
    fn a_missing_peer_references_nothing_and_an_unreadable_one_keeps_everything() {
        let dir = std::env::temp_dir().join(format!("astro-pool-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!referenced_by_peers(&[dir.join("absent.db")], "x"));
        let broken = dir.join("broken.db");
        std::fs::write(&broken, b"ceci n'est pas une base SQLite, loin de la").unwrap();
        assert!(referenced_by_peers(&[broken], "x"), "base illisible : on garde le blob");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
