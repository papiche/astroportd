//! Pont entre deux plans d'une même machine (ORIGIN / ZEN*) qui partagent le même bassin de blobs
//! (`--blobs` identique, voir `astro_dav::pool`) : **adoption sans copie**.
//!
//! Un fichier du compte source devient un fichier du compte de destination **sans qu'aucun octet ne soit écrit** : la
//! nouvelle entrée désigne le même blob chiffré, avec la même clé de fichier. Le disque ne grossit pas, et le blob reste
//! dans le bassin tant qu'un seul des deux plans le référence.
//!
//! Règles (plan double UPlanet, invariants I1 à I6) :
//! - **accord explicite** : partager un blob et sa clé entre deux comptes n'est permis que pour **une même personne**
//!   (l'option `--same-person` de la commande l'atteste) ;
//! - **jamais d'écrasement** : un chemin déjà pris par un autre contenu est un conflit, laissé tel quel ; le même contenu
//!   déjà là est ignoré (la commande est **rejouable**) ;
//! - **le quota du compte de destination s'applique** (la place logique compte, pas la place physique) ;
//! - seuls les blobs UENC v2 présents **dans le bassin** sont adoptés. Un fichier absent du bassin est signalé, jamais
//!   copié en silence : son transfert demande un ré-chiffrement (étape suivante du plan).

use std::collections::HashMap;
use std::path::Path;

use astro_dav::blob::{BlobError, BlobStore};
use astro_dav::server::DavConfig;
use astro_dav::vfs::{self, EntryKind, NewFile, VfsError, vfs_tx};
use astro_db::Db;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// Ce que la commande doit savoir.
#[derive(Debug, Clone)]
pub struct Options {
    /// Clé (64 hex) du compte source.
    pub from_owner: String,
    /// Clé (64 hex) du compte de destination.
    pub to_owner: String,
    /// Dossier (ou fichier) source ; `/` pour tout le uCloud.
    pub path: String,
    /// Dossier de destination (le chemin source y est reproduit).
    pub dest: String,
    /// Compte sans rien écrire.
    pub dry_run: bool,
}

/// Bilan de l'adoption.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Fichiers adoptés (ou qui le seraient, en `dry_run`).
    pub adopted: u64,
    /// Octets (clair) adoptés.
    pub bytes: u64,
    /// Déjà là, même contenu : ignorés.
    pub same: u64,
    /// Chemin pris par un autre contenu : laissés tels quels.
    pub conflicts: u64,
    /// Blob absent du bassin ou clé inconnue : non adoptés.
    pub missing: u64,
    /// Refusés par le quota du compte de destination.
    pub quota: u64,
}

struct Source {
    path: String,
    cid: String,
    mime: String,
    size: u64,
    size_cid: Option<u64>,
    mtime: i64,
    sha_plain: String,
    taken_at: Option<i64>,
    lat: Option<f64>,
    lon: Option<f64>,
    phash: Option<i64>,
}

/// Clé (64 hex) du compte hébergé `login` (email ou clé) dans la base `db`.
pub fn resolve_owner(db: &Path, login: &str) -> Result<String, String> {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| format!("{} : {e}", db.display()))?;
    c.query_row("SELECT hex FROM identities WHERE hosted = 1 AND (hex = lower(?1) OR lower(email) = lower(?1)) ORDER BY hex LIMIT 1", [login], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{login} : aucun compte hébergé dans {}", db.display()))
}

/// Entrées de fichier du compte source sous `path`, et leurs clés (UENC v2).
fn read_source(src_db: &Path, owner: &str, path: &str) -> Result<(Vec<Source>, HashMap<String, String>), String> {
    let c = Connection::open_with_flags(src_db, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| format!("{} : {e}", src_db.display()))?;
    c.busy_timeout(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?;
    let root = vfs::normalize_path(path).map_err(|e| e.to_string())?;
    // Une arborescence est une plage d'index : `/a` puis tout ce qui commence par `/a/` (`0` suit `/`).
    let (low, high) = if root == "/" { ("/".to_owned(), "\u{10FFFF}".to_owned()) } else { (format!("{root}/"), format!("{root}0")) };
    let mut stmt = c
        .prepare(
            "SELECT path, cid, COALESCE(mime, ''), COALESCE(size, 0), json_extract(data, '$.size_cid'), COALESCE(mtime, 0), COALESCE(sha_plain, ''), taken_at, lat, lon, phash \
             FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL AND (path = ?2 OR (path >= ?3 AND path < ?4)) ORDER BY path",
        )
        .map_err(|e| e.to_string())?;
    let files = stmt
        .query_map(rusqlite::params![owner, root, low, high], |r| {
            Ok(Source {
                path: r.get(0)?,
                cid: r.get(1)?,
                mime: r.get(2)?,
                size: u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
                size_cid: r.get::<_, Option<i64>>(4)?.and_then(|n| u64::try_from(n).ok()),
                mtime: r.get(5)?,
                sha_plain: r.get(6)?,
                taken_at: r.get(7)?,
                lat: r.get(8)?,
                lon: r.get(9)?,
                phash: r.get(10)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut keys = HashMap::new();
    let mut stmt = c.prepare("SELECT cid, key_hex FROM ucloud_keyring WHERE owner_hex = ?1 AND enc = 'uenc2'").map_err(|e| e.to_string())?;
    for row in stmt.query_map([owner], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())? {
        let (cid, key) = row.map_err(|e| e.to_string())?;
        keys.insert(cid, key);
    }
    Ok((files, keys))
}

/// Chemin de destination : le chemin source reproduit sous `dest`.
fn dest_path(root: &str, dest: &str, path: &str) -> String {
    let rel = if root == "/" { path } else { path.strip_prefix(root).unwrap_or(path) };
    let rel = if rel.is_empty() { path.rsplit_once('/').map_or(path, |(_, name)| name) } else { rel };
    let rel = rel.trim_start_matches('/');
    if dest == "/" { format!("/{rel}") } else { format!("{dest}/{rel}") }
}

/// Adopte sans copie les fichiers de `opts.from_owner` (base `src_db`) dans `opts.to_owner` (base `dst`).
/// `pool` est le dossier de blobs commun aux deux plans.
pub async fn adopt<B: BlobStore>(src_db: &Path, dst: &Db, pool: &B, config: &DavConfig, opts: &Options) -> Result<Report, String> {
    let root = vfs::normalize_path(&opts.path).map_err(|e| e.to_string())?;
    let dest = vfs::normalize_path(&opts.dest).map_err(|e| e.to_string())?;
    let account = astro_dav::auth::lookup_by_hex(dst, &opts.to_owner).await.ok_or("compte de destination non hébergé ici")?;
    let limits = config.limits_for(&account);
    let (files, keys) = read_source(src_db, &opts.from_owner, &root)?;
    let mut report = Report::default();
    for f in files {
        let Some(key) = keys.get(&f.cid).cloned() else {
            report.missing += 1;
            continue;
        };
        match pool.open(&f.cid).await {
            Ok(_) => {}
            Err(BlobError::NotFound | BlobError::BadCid) => {
                report.missing += 1;
                continue;
            }
            Err(e) => return Err(format!("bassin illisible ({}) : {e}", f.cid)),
        }
        let target = dest_path(&root, &dest, &f.path);
        let (owner, t, cid, sha) = (opts.to_owner.clone(), target.clone(), f.cid.clone(), f.sha_plain.clone());
        let existing = dst
            .readers()
            .read(move |c| Ok(vfs::get_entry(c, &owner, &t).ok().flatten().map(|e| (e.kind, e.cid, e.size))))
            .await
            .map_err(|e| e.to_string())?;
        if let Some((kind, ecid, esize)) = existing {
            let same = kind == EntryKind::File && (ecid.as_deref() == Some(cid.as_str()) || (!sha.is_empty() && esize == f.size && same_sha(dst, &opts.to_owner, &target, &sha).await));
            if same {
                report.same += 1;
            } else {
                report.conflicts += 1;
            }
            continue;
        }
        if opts.dry_run {
            report.adopted += 1;
            report.bytes += f.size;
            continue;
        }
        let file = NewFile { cid: f.cid.clone(), size_cid: f.size_cid.unwrap_or(0), size_plain: f.size, sha_plain: f.sha_plain.clone(), mime: f.mime.clone(), key_hex: key, now: f.mtime };
        let (owner, p, parent) = (opts.to_owner.clone(), target.clone(), target.rsplit_once('/').map_or("/".to_owned(), |(d, _)| if d.is_empty() { "/".to_owned() } else { d.to_owned() }));
        let (taken_at, lat, lon, phash, mtime) = (f.taken_at, f.lat, f.lon, f.phash, f.mtime);
        let done = vfs_tx(dst, move |tx| {
            vfs::mkdir_p_tx(tx, &owner, &parent, mtime)?;
            vfs::commit_file_tx(tx, &owner, &p, &file, limits)?;
            tx.execute("UPDATE ucloud_entries SET taken_at = ?3, lat = ?4, lon = ?5, phash = ?6 WHERE owner_hex = ?1 AND path = ?2", rusqlite::params![owner, p, taken_at, lat, lon, phash]).map_err(VfsError::from)?;
            Ok(())
        })
        .await;
        match done {
            Ok(()) => {
                report.adopted += 1;
                report.bytes += f.size;
            }
            Err(VfsError::QuotaExceeded) => report.quota += 1,
            Err(e) => return Err(format!("{target} : {e}")),
        }
    }
    Ok(report)
}

async fn same_sha(db: &Db, owner: &str, path: &str, sha: &str) -> bool {
    let (o, p, s) = (owner.to_owned(), path.to_owned(), sha.to_owned());
    db.readers().read(move |c| Ok(c.query_row("SELECT sha_plain = ?3 FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", rusqlite::params![o, p, s], |r| r.get::<_, bool>(0)).unwrap_or(false))).await.unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_path_is_reproduced_under_the_destination() {
        assert_eq!(dest_path("/Photos", "/Imports/ORIGIN", "/Photos/2024/a.jpg"), "/Imports/ORIGIN/2024/a.jpg");
        assert_eq!(dest_path("/", "/Imports/ORIGIN", "/Photos/a.jpg"), "/Imports/ORIGIN/Photos/a.jpg");
        assert_eq!(dest_path("/Photos/a.jpg", "/Imports", "/Photos/a.jpg"), "/Imports/a.jpg", "un fichier seul garde son nom");
        assert_eq!(dest_path("/Photos", "/", "/Photos/a.jpg"), "/a.jpg");
    }
}
