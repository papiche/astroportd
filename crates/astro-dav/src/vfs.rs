//! Système de fichiers virtuel d'un compte, stocké dans `ucloud_entries` / `ucloud_keyring`.
//!
//! Chemins normalisés : absolus, sans `.`/`..`, sans segment vide, `/` pour la racine (implicite, jamais stockée).
//! Les fonctions `*_tx` s'exécutent dans une transaction de l'écrivain unique d'`astro-db`.

use astro_db::DbError;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// Longueur maximale d'un chemin et d'un segment.
pub const MAX_PATH: usize = 1024;
/// Longueur maximale d'un nom.
pub const MAX_SEGMENT: usize = 255;

/// Erreurs du VFS.
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    /// Chemin refusé.
    #[error("chemin invalide")]
    BadPath,
    /// Entrée absente.
    #[error("introuvable")]
    NotFound,
    /// L'entrée existe déjà.
    #[error("existe déjà")]
    Exists,
    /// Le dossier parent n'existe pas.
    #[error("dossier parent absent")]
    ParentMissing,
    /// Opération de fichier sur un dossier.
    #[error("c'est un dossier")]
    IsCollection,
    /// Opération de dossier sur un fichier.
    #[error("ce n'est pas un dossier")]
    NotCollection,
    /// Fichier plus gros que le plafond unitaire.
    #[error("fichier trop volumineux")]
    TooLarge,
    /// Quota total dépassé.
    #[error("quota dépassé")]
    QuotaExceeded,
    /// Déplacement d'un dossier dans lui-même, ou de la racine.
    #[error("déplacement invalide")]
    BadMove,
    /// Erreur de base.
    #[error("base : {0}")]
    Db(#[from] DbError),
    /// Erreur SQLite brute.
    #[error("sqlite : {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Nature d'une entrée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Fichier chiffré.
    File,
    /// Dossier virtuel.
    Dir,
}

/// Entrée du VFS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Chemin normalisé.
    pub path: String,
    /// Fichier ou dossier.
    pub kind: EntryKind,
    /// CID du blob chiffré (fichiers).
    pub cid: Option<String>,
    /// Type MIME (fichiers).
    pub mime: Option<String>,
    /// Taille du clair en octets (0 pour un dossier).
    pub size: u64,
    /// Dernière modification, secondes UNIX.
    pub mtime: i64,
    /// Propriétés mortes WebDAV `(espace de noms, nom, valeur XML)` posées par PROPPATCH.
    pub dead_props: Vec<DeadProp>,
}

/// Propriété morte : `(espace de noms, nom local, contenu XML)`.
pub type DeadProp = (String, String, String);

/// Fichier à enregistrer après écriture du blob.
#[derive(Debug, Clone)]
pub struct NewFile {
    /// CID du blob UENC.
    pub cid: String,
    /// Taille du blob chiffré.
    pub size_cid: u64,
    /// Taille du clair.
    pub size_plain: u64,
    /// sha256 du clair (hex).
    pub sha_plain: String,
    /// Type MIME.
    pub mime: String,
    /// Clé de fichier (64 hex).
    pub key_hex: String,
    /// Date de modification.
    pub now: i64,
}

/// Limites appliquées à une écriture.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Plafond d'un fichier.
    pub max_file: u64,
    /// Quota total du compte.
    pub quota_total: u64,
}

/// Normalise un chemin de requête (déjà décodé).
pub fn normalize_path(raw: &str) -> Result<String, VfsError> {
    if raw.len() > MAX_PATH || raw.contains('\0') || raw.contains('\\') {
        return Err(VfsError::BadPath);
    }
    let mut out = String::new();
    for seg in raw.split('/').filter(|s| !s.is_empty()) {
        if seg == "." || seg == ".." || seg.len() > MAX_SEGMENT || seg.chars().any(char::is_control) {
            return Err(VfsError::BadPath);
        }
        out.push('/');
        out.push_str(seg);
    }
    Ok(if out.is_empty() { "/".to_owned() } else { out })
}

/// Dossier parent (`/` pour un chemin de premier niveau).
pub fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(i) => path[..i].to_owned(),
    }
}

/// Nom du dernier segment.
pub fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

/// Type MIME deviné d'après l'extension.
pub fn guess_mime(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("pdf") => "application/pdf",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

fn like_children(path: &str) -> String {
    let escaped: String = path.chars().flat_map(|c| if matches!(c, '%' | '_' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
    if path == "/" { "/%".to_owned() } else { format!("{escaped}/%") }
}

const COLS: &str = "path, type, cid, mime, size, mtime, data";

fn parse_props(data: &str) -> Vec<DeadProp> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { return Vec::new() };
    v.get("props")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p.get("ns")?.as_str()?.to_owned(), p.get("name")?.as_str()?.to_owned(), p.get("value")?.as_str()?.to_owned())))
        .collect()
}

fn row_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    let kind: String = r.get(1)?;
    Ok(Entry {
        path: r.get(0)?,
        kind: if kind == "dir" { EntryKind::Dir } else { EntryKind::File },
        cid: r.get(2)?,
        mime: r.get(3)?,
        size: u64::try_from(r.get::<_, Option<i64>>(4)?.unwrap_or(0)).unwrap_or(0),
        mtime: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
        dead_props: parse_props(&r.get::<_, String>(6).unwrap_or_default()),
    })
}

/// Entrée à `path` (la racine est un dossier implicite).
pub fn get_entry(conn: &Connection, owner: &str, path: &str) -> Result<Option<Entry>, VfsError> {
    if path == "/" {
        return Ok(Some(Entry { path: "/".into(), kind: EntryKind::Dir, cid: None, mime: None, size: 0, mtime: 0, dead_props: Vec::new() }));
    }
    Ok(conn
        .query_row(&format!("SELECT {COLS} FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2"), params![owner, path], row_entry)
        .optional()?)
}

/// Enfants directs de `path`, triés par nom.
pub fn list_children(conn: &Connection, owner: &str, path: &str) -> Result<Vec<Entry>, VfsError> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM ucloud_entries WHERE owner_hex = ?1 AND parent = ?2 ORDER BY path"))?;
    let rows = stmt.query_map(params![owner, path], row_entry)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Toute la descendance de `path`, triée, au plus `cap + 1` entrées (le dépassement signale un arbre trop grand).
pub fn list_subtree(conn: &Connection, owner: &str, path: &str, cap: usize) -> Result<Vec<Entry>, VfsError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE ?2 ESCAPE '\\' ORDER BY path LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![owner, like_children(path), i64::try_from(cap + 1).unwrap_or(i64::MAX)], row_entry)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Exécute `f` dans une transaction de l'écrivain ; toute erreur du VFS **annule** la transaction.
pub async fn vfs_tx<T, F>(db: &astro_db::Db, f: F) -> Result<T, VfsError>
where
    T: Send + 'static,
    F: FnOnce(&Transaction<'_>) -> Result<T, VfsError> + Send + 'static,
{
    use std::sync::{Arc, Mutex};
    let slot: Arc<Mutex<Option<VfsError>>> = Arc::new(Mutex::new(None));
    let inner = slot.clone();
    let out = db
        .writer()
        .transaction(move |tx| {
            f(tx).map_err(|e| {
                *inner.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                DbError::Aborted
            })
        })
        .await;
    match out {
        Ok(v) => Ok(v),
        Err(DbError::Aborted) => Err(slot.lock().unwrap_or_else(|p| p.into_inner()).take().unwrap_or(VfsError::Db(DbError::Aborted))),
        Err(e) => Err(VfsError::Db(e)),
    }
}

/// Octets de clair occupés par le compte.
pub fn used_bytes(conn: &Connection, owner: &str) -> Result<u64, VfsError> {
    let n: Option<i64> = conn.query_row("SELECT SUM(size) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", [owner], |r| r.get(0))?;
    Ok(u64::try_from(n.unwrap_or(0)).unwrap_or(0))
}

fn require_dir_parent(tx: &Transaction<'_>, owner: &str, path: &str) -> Result<(), VfsError> {
    match get_entry(tx, owner, &parent_of(path))? {
        None => Err(VfsError::ParentMissing),
        Some(e) if e.kind == EntryKind::File => Err(VfsError::NotCollection),
        Some(_) => Ok(()),
    }
}

/// Enregistre un fichier (création ou remplacement) et sa clé. Retourne l'ancienne entrée remplacée.
pub fn commit_file_tx(tx: &Transaction<'_>, owner: &str, path: &str, f: &NewFile, limits: Limits) -> Result<Option<Entry>, VfsError> {
    if path == "/" {
        return Err(VfsError::IsCollection);
    }
    require_dir_parent(tx, owner, path)?;
    let old = get_entry(tx, owner, path)?;
    if old.as_ref().is_some_and(|e| e.kind == EntryKind::Dir) {
        return Err(VfsError::IsCollection);
    }
    if f.size_plain > limits.max_file {
        return Err(VfsError::TooLarge);
    }
    let old_thumb: Option<String> = tx
        .query_row("SELECT json_extract(data, '$.thumb.cid') FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", params![owner, path], |r| r.get(0))
        .optional()?
        .flatten();
    let used_without_old = used_bytes(tx, owner)?.saturating_sub(old.as_ref().map_or(0, |e| e.size));
    if used_without_old.saturating_add(f.size_plain) > limits.quota_total {
        return Err(VfsError::QuotaExceeded);
    }
    let data = serde_json::json!({
        "type": "file", "cid": f.cid, "enc": "uenc2", "mime": f.mime,
        "size_plain": f.size_plain, "size_cid": f.size_cid, "sha256_plain": f.sha_plain, "mtime": f.now,
    })
    .to_string();
    tx.execute(
        "INSERT OR REPLACE INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, sha_orig, sha_plain, origin, data) \
         VALUES (?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, ?8, ?8, 'webdav', ?9)",
        params![owner, path, parent_of(path), f.cid, f.mime, f.size_plain as i64, f.now, f.sha_plain, data],
    )?;
    tx.execute("INSERT OR REPLACE INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", params![owner, f.cid, f.key_hex])?;
    if let Some(prev) = old.as_ref().and_then(|e| e.cid.as_deref()) {
        if prev != f.cid {
            drop_unreferenced_key(tx, owner, prev)?;
        }
    }
    if let Some(thumb) = &old_thumb {
        drop_unreferenced_key(tx, owner, thumb)?;
    }
    Ok(old)
}

/// Supprime la clé d'un blob que plus aucune entrée du compte ne référence (une copie partage le CID et la clé).
fn drop_unreferenced_key(tx: &Transaction<'_>, owner: &str, cid: &str) -> Result<(), VfsError> {
    tx.execute(
        "DELETE FROM ucloud_keyring WHERE owner_hex = ?1 AND cid = ?2 AND NOT EXISTS (SELECT 1 FROM ucloud_entries WHERE owner_hex = ?1 AND (cid = ?2 OR json_extract(data, '$.thumb.cid') = ?2))",
        params![owner, cid],
    )?;
    Ok(())
}

/// Crée un dossier.
pub fn mkdir_tx(tx: &Transaction<'_>, owner: &str, path: &str, now: i64) -> Result<(), VfsError> {
    if path == "/" || get_entry(tx, owner, path)?.is_some() {
        return Err(VfsError::Exists);
    }
    require_dir_parent(tx, owner, path)?;
    let data = serde_json::json!({ "type": "dir", "mtime": now, "created_at": now }).to_string();
    tx.execute(
        "INSERT INTO ucloud_entries(owner_hex, path, parent, type, mtime, origin, data) VALUES (?1, ?2, ?3, 'dir', ?4, 'webdav', ?5)",
        params![owner, path, parent_of(path), now, data],
    )?;
    Ok(())
}

/// Supprime une entrée et toute sa descendance, avec leurs clés. Retourne les CID des blobs à supprimer.
pub fn delete_tree_tx(tx: &Transaction<'_>, owner: &str, path: &str) -> Result<Vec<String>, VfsError> {
    if path == "/" {
        return Err(VfsError::BadMove);
    }
    if get_entry(tx, owner, path)?.is_none() {
        return Err(VfsError::NotFound);
    }
    // Les blobs des fichiers et ceux de leurs miniatures.
    let cids: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND cid IS NOT NULL AND (path = ?2 OR path LIKE ?3 ESCAPE '\\') \
             UNION SELECT json_extract(data, '$.thumb.cid') FROM ucloud_entries WHERE owner_hex = ?1 AND json_extract(data, '$.thumb.cid') IS NOT NULL AND (path = ?2 OR path LIKE ?3 ESCAPE '\\')",
        )?;
        let rows = stmt.query_map(params![owner, path, like_children(path)], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    tx.execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND (path = ?2 OR path LIKE ?3 ESCAPE '\\')", params![owner, path, like_children(path)])?;
    for cid in &cids {
        drop_unreferenced_key(tx, owner, cid)?;
    }
    Ok(cids)
}

/// Renomme `from` en `to` (avec sa descendance). Retourne les CID écrasés si `overwrite`.
pub fn move_tx(tx: &Transaction<'_>, owner: &str, from: &str, to: &str, overwrite: bool, now: i64) -> Result<Vec<String>, VfsError> {
    if from == "/" || to == "/" || to == from || to.starts_with(&format!("{from}/")) {
        return Err(VfsError::BadMove);
    }
    if get_entry(tx, owner, from)?.is_none() {
        return Err(VfsError::NotFound);
    }
    require_dir_parent(tx, owner, to)?;
    let mut overwritten = Vec::new();
    if get_entry(tx, owner, to)?.is_some() {
        if !overwrite {
            return Err(VfsError::Exists);
        }
        overwritten = delete_tree_tx(tx, owner, to)?;
    }
    tx.execute(
        "UPDATE ucloud_entries SET path = ?2 || substr(path, ?3), parent = ?2 || substr(parent, ?3) \
         WHERE owner_hex = ?1 AND path LIKE ?4 ESCAPE '\\'",
        params![owner, to, i64::try_from(from.chars().count()).unwrap_or(0) + 1, like_children(from)],
    )?;
    tx.execute("UPDATE ucloud_entries SET path = ?3, parent = ?4, mtime = ?5 WHERE owner_hex = ?1 AND path = ?2", params![owner, from, to, parent_of(to), now])?;
    Ok(overwritten)
}

/// Options d'une copie.
#[derive(Debug, Clone, Copy)]
pub struct CopyOpts {
    /// Remplacer une destination existante.
    pub overwrite: bool,
    /// Copier toute la descendance d'un dossier.
    pub recursive: bool,
    /// Date de la copie.
    pub now: i64,
    /// Quota total du compte.
    pub quota_total: u64,
}

/// Copie `from` vers `to` : les copies de fichiers partagent le CID et la clé du blob (déduplication, aucun ré-encodage).
///
/// `recursive` copie toute la descendance d'un dossier ; sinon seul le dossier lui-même. Le quota est vérifié sur
/// la taille logique ajoutée. Retourne les CID écrasés si `overwrite`.
pub fn copy_tx(tx: &Transaction<'_>, owner: &str, from: &str, to: &str, opts: CopyOpts) -> Result<Vec<String>, VfsError> {
    let CopyOpts { overwrite, recursive, now, quota_total } = opts;
    if from == "/" || to == "/" || to == from || to.starts_with(&format!("{from}/")) {
        return Err(VfsError::BadMove);
    }
    let Some(src) = get_entry(tx, owner, from)? else { return Err(VfsError::NotFound) };
    require_dir_parent(tx, owner, to)?;
    let mut overwritten = Vec::new();
    let dest_size = match get_entry(tx, owner, to)? {
        Some(d) => {
            if !overwrite {
                return Err(VfsError::Exists);
            }
            let size = if d.kind == EntryKind::File { d.size } else { list_subtree(tx, owner, to, usize::MAX / 2)?.iter().map(|e| e.size).sum() };
            overwritten = delete_tree_tx(tx, owner, to)?;
            size
        }
        None => 0,
    };
    let added: u64 = if src.kind == EntryKind::File {
        src.size
    } else if recursive {
        list_subtree(tx, owner, from, usize::MAX / 2)?.iter().map(|e| e.size).sum()
    } else {
        0
    };
    if used_bytes(tx, owner)?.saturating_sub(dest_size).saturating_add(added) > quota_total {
        return Err(VfsError::QuotaExceeded);
    }
    let cols = "type, cid, mime, size, mtime, taken_at, sha_orig, sha_plain, faceid_status, origin, phash, lat, lon, desc_text, data";
    if recursive && src.kind == EntryKind::Dir {
        tx.execute(
            &format!(
                "INSERT INTO ucloud_entries(owner_hex, path, parent, {cols}) \
                 SELECT owner_hex, ?2 || substr(path, ?3), ?2 || substr(parent, ?3), {cols} \
                 FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE ?4 ESCAPE '\\'"
            ),
            params![owner, to, i64::try_from(from.chars().count()).unwrap_or(0) + 1, like_children(from)],
        )?;
    }
    tx.execute(
        &format!(
            "INSERT INTO ucloud_entries(owner_hex, path, parent, {cols}) \
             SELECT owner_hex, ?3, ?4, type, cid, mime, size, ?5, taken_at, sha_orig, sha_plain, faceid_status, origin, phash, lat, lon, desc_text, data \
             FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2"
        ),
        params![owner, from, to, parent_of(to), now],
    )?;
    Ok(overwritten)
}

/// Crée ou met à jour (`Some`) / supprime (`None`) des propriétés mortes d'une entrée.
pub fn set_props_tx(tx: &Transaction<'_>, owner: &str, path: &str, changes: &[(String, String, Option<String>)]) -> Result<(), VfsError> {
    if path == "/" {
        return Err(VfsError::BadMove);
    }
    let data: String = tx
        .query_row("SELECT data FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", params![owner, path], |r| r.get(0))
        .optional()?
        .ok_or(VfsError::NotFound)?;
    let mut doc: serde_json::Value = serde_json::from_str(&data).unwrap_or_else(|_| serde_json::json!({}));
    let mut props = parse_props(&data);
    for (ns, name, value) in changes {
        props.retain(|(n, l, _)| !(n == ns && l == name));
        if let Some(v) = value {
            props.push((ns.clone(), name.clone(), v.clone()));
        }
    }
    if props.len() > 64 {
        return Err(VfsError::TooLarge);
    }
    if let Some(obj) = doc.as_object_mut() {
        if props.is_empty() {
            obj.remove("props");
        } else {
            obj.insert("props".into(), props.iter().map(|(ns, name, value)| serde_json::json!({ "ns": ns, "name": name, "value": value })).collect());
        }
    }
    tx.execute("UPDATE ucloud_entries SET data = ?3 WHERE owner_hex = ?1 AND path = ?2", params![owner, path, doc.to_string()])?;
    Ok(())
}

/// Le blob `cid` est-il encore référencé par une entrée (de n'importe quel compte) ?
pub fn cid_in_use(conn: &Connection, cid: &str) -> Result<bool, VfsError> {
    Ok(conn.query_row("SELECT 1 FROM ucloud_entries WHERE cid = ?1 OR json_extract(data, '$.thumb.cid') = ?1 LIMIT 1", [cid], |_| Ok(())).optional()?.is_some())
}

#[cfg(test)]
mod tests {
    use astro_db::Db;

    use super::*;

    const OWNER: &str = "aa";
    const LIMITS: Limits = Limits { max_file: 1000, quota_total: 1500 };

    struct Tmp(std::path::PathBuf, Db);

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup(tag: &str) -> Tmp {
        let dir = std::env::temp_dir().join(format!("astro-vfs-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 1).unwrap();
        Tmp(dir, db)
    }

    fn file(cid: &str, size: u64) -> NewFile {
        NewFile { cid: cid.into(), size_cid: size + 30, size_plain: size, sha_plain: "ab".repeat(32), mime: "text/plain".into(), key_hex: "cd".repeat(32), now: 1_800_000_000 }
    }

    async fn tx<T: Send + 'static>(t: &Tmp, f: impl FnOnce(&Transaction<'_>) -> Result<T, VfsError> + Send + 'static) -> Result<T, VfsError> {
        vfs_tx(&t.1, f).await
    }

    #[test]
    fn paths_are_normalized_and_hostile_ones_refused() {
        assert_eq!(normalize_path("").unwrap(), "/");
        assert_eq!(normalize_path("//a///b/").unwrap(), "/a/b");
        for bad in ["/a/../b", "/a/./b", "/a\0b", "/a\\b", &format!("/{}", "x".repeat(256)), &"/a".repeat(600)] {
            assert!(normalize_path(bad).is_err(), "{bad:?}");
        }
        assert_eq!((parent_of("/a/b"), parent_of("/a"), name_of("/a/b.txt")), ("/a".to_owned(), "/".to_owned(), "b.txt"));
        assert_eq!(guess_mime("/x/Photo.JPG"), "image/jpeg");
        assert_eq!(guess_mime("/x/inconnu"), "application/octet-stream");
    }

    #[tokio::test]
    async fn files_dirs_quota_and_replacement() {
        let t = setup("files");
        assert!(tx(&t, |x| commit_file_tx(x, OWNER, "/a.txt", &file("c1", 10), LIMITS)).await.unwrap().is_none());
        assert!(matches!(tx(&t, |x| commit_file_tx(x, OWNER, "/d/a.txt", &file("c2", 10), LIMITS)).await, Err(VfsError::ParentMissing)));
        tx(&t, |x| mkdir_tx(x, OWNER, "/d", 5)).await.unwrap();
        assert!(matches!(tx(&t, |x| mkdir_tx(x, OWNER, "/d", 5)).await, Err(VfsError::Exists)));
        assert!(matches!(tx(&t, |x| mkdir_tx(x, OWNER, "/a.txt/x", 5)).await, Err(VfsError::NotCollection)));
        tx(&t, |x| commit_file_tx(x, OWNER, "/d/a.txt", &file("c2", 900), LIMITS)).await.unwrap();
        assert!(matches!(tx(&t, |x| commit_file_tx(x, OWNER, "/big", &file("c3", 1001), LIMITS)).await, Err(VfsError::TooLarge)));
        assert!(matches!(tx(&t, |x| commit_file_tx(x, OWNER, "/q", &file("c4", 700), LIMITS)).await, Err(VfsError::QuotaExceeded)), "10 + 900 + 700 > 1500");
        // Remplacer /d/a.txt (900) par 1000 : 10 + 1000 ≤ 1500, et l'ancienne clé disparaît.
        let old = tx(&t, |x| commit_file_tx(x, OWNER, "/d/a.txt", &file("c5", 1000), LIMITS)).await.unwrap().unwrap();
        assert_eq!(old.cid.as_deref(), Some("c2"));
        assert!(matches!(tx(&t, |x| commit_file_tx(x, OWNER, "/d", &file("c6", 1), LIMITS)).await, Err(VfsError::IsCollection)));
        let (keys, used, kids): (i64, u64, Vec<String>) = t
            .1
            .readers()
            .read(|c| {
                let keys: i64 = c.query_row("SELECT count(*) FROM ucloud_keyring WHERE cid = 'c2'", [], |r| r.get(0))?;
                Ok((keys, used_bytes(c, OWNER).unwrap(), list_children(c, OWNER, "/").unwrap().into_iter().map(|e| e.path).collect()))
            })
            .await
            .unwrap();
        assert_eq!((keys, used), (0, 1010));
        assert_eq!(kids, ["/a.txt", "/d"]);
    }

    #[tokio::test]
    async fn delete_and_move_trees() {
        let t = setup("tree");
        tx(&t, |x| {
            mkdir_tx(x, OWNER, "/a", 1)?;
            mkdir_tx(x, OWNER, "/a/b", 1)?;
            commit_file_tx(x, OWNER, "/a/b/f.txt", &file("c1", 5), LIMITS)?;
            commit_file_tx(x, OWNER, "/a/g_%.txt", &file("c2", 5), LIMITS)?;
            commit_file_tx(x, OWNER, "/ab.txt", &file("c3", 5), LIMITS).map(|_| ())
        })
        .await
        .unwrap();
        // Déplacer /a vers /z : descendance renommée, sans toucher /ab.txt (préfixe voisin).
        tx(&t, |x| move_tx(x, OWNER, "/a", "/z", false, 9)).await.unwrap();
        let paths: Vec<String> = t.1.readers().read(|c| Ok(list_subtree(c, OWNER, "/", 100).unwrap().into_iter().map(|e| e.path).collect())).await.unwrap();
        assert_eq!(paths, ["/ab.txt", "/z", "/z/b", "/z/b/f.txt", "/z/g_%.txt"]);
        let parents: Vec<String> = t
            .1
            .readers()
            .read(|c| Ok(c.prepare("SELECT parent FROM ucloud_entries WHERE path IN ('/z','/z/b','/z/b/f.txt') ORDER BY path")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?))
            .await
            .unwrap();
        assert_eq!(parents, ["/", "/z", "/z/b"]);
        assert!(matches!(tx(&t, |x| move_tx(x, OWNER, "/z", "/z/b/inside", false, 9)).await, Err(VfsError::BadMove)));
        assert!(matches!(tx(&t, |x| move_tx(x, OWNER, "/z", "/ab.txt", false, 9)).await, Err(VfsError::Exists)));
        let over = tx(&t, |x| move_tx(x, OWNER, "/z/b/f.txt", "/ab.txt", true, 9)).await.unwrap();
        assert_eq!(over, ["c3"], "l'écrasé est rendu pour suppression de son blob");
        // Suppression récursive : clés et entrées partent ensemble.
        let mut gone = tx(&t, |x| delete_tree_tx(x, OWNER, "/z")).await.unwrap();
        gone.sort();
        assert_eq!(gone, ["c2"]);
        assert!(matches!(tx(&t, |x| delete_tree_tx(x, OWNER, "/z")).await, Err(VfsError::NotFound)));
        assert!(matches!(tx(&t, |x| delete_tree_tx(x, OWNER, "/")).await, Err(VfsError::BadMove)));
        let (entries, keys): (i64, i64) = t
            .1
            .readers()
            .read(|c| Ok((c.query_row("SELECT count(*) FROM ucloud_entries", [], |r| r.get(0))?, c.query_row("SELECT count(*) FROM ucloud_keyring", [], |r| r.get(0))?)))
            .await
            .unwrap();
        assert_eq!((entries, keys), (1, 1), "reste /ab.txt (ex-f.txt) et sa clé c1");
    }
}
