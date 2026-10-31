//! Jetons WebDAV d'appareil : un jeton par téléphone ou ordinateur, **confiné à un dossier** du uCloud et à un mode.
//!
//! - **Dossier** : le client voit ce dossier comme la racine du WebDAV ; il ne peut ni lire ni écrire ailleurs. Son envoi
//!   arrive donc dans le répertoire choisi par le titulaire (par exemple `/Photos/Téléphone`).
//! - **Mode** : `write` (envoi seul : déposer, créer des dossiers, déplacer ; ni lire, ni supprimer), `readwrite` (tout, dans
//!   le dossier), `read` (lecture seule).
//! - **Jeton** : `dv_` + 32 hexadécimaux, montré **une seule fois** ; seule son empreinte SHA-256 est gardée. Il se révoque
//!   à tout moment. Il n'ouvre ni l'API de l'interface, ni les contacts, ni l'agenda : seulement ce dossier, en WebDAV.
//! - Le jeton principal du compte (et NIP-98) fonctionne comme avant, sans confinement.

use astro_db::{Db, DbError};
use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::vfs::{self, mkdir_p_tx, normalize_path, vfs_tx};

/// Appareils par compte au plus.
pub const MAX_DEVICES: usize = 16;
/// Dossiers qu'un appareil ne peut jamais recevoir comme racine.
const RESERVED: [&str; 5] = ["Coffre", "Contacts", "Calendrier", "Partages", "Sauvegardes"];

/// Ce qu'un appareil peut faire dans son dossier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Envoi seul : déposer, créer des dossiers, déplacer, lister ; ni lire un fichier, ni supprimer.
    Write,
    /// Lire, écrire, supprimer, déplacer.
    ReadWrite,
    /// Lire et lister seulement.
    Read,
}

impl Mode {
    /// Nom stocké et échangé.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::ReadWrite => "readwrite",
            Self::Read => "read",
        }
    }

    /// Depuis le nom stocké.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "write" => Some(Self::Write),
            "readwrite" => Some(Self::ReadWrite),
            "read" => Some(Self::Read),
            _ => None,
        }
    }

    /// Une requête WebDAV de cette méthode est-elle permise ?
    #[must_use]
    pub fn allows(self, method: &str) -> bool {
        match (self, method) {
            (_, "OPTIONS" | "PROPFIND") => true,
            (Self::Read, "GET" | "HEAD") => true,
            (Self::Read, _) => false,
            (Self::Write, "PUT" | "MKCOL" | "MOVE" | "COPY" | "PROPPATCH" | "LOCK" | "UNLOCK") => true,
            (Self::Write, _) => false,
            (Self::ReadWrite, _) => true,
        }
    }
}

/// Confinement établi par un jeton d'appareil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// Identifiant de l'appareil.
    pub id: String,
    /// Racine du uCloud vue comme `/` par le client.
    pub root: String,
    /// Mode.
    pub mode: Mode,
}

impl Scope {
    /// Chemin du uCloud pour un chemin vu par le client.
    #[must_use]
    pub fn real(&self, client: &str) -> String {
        if client == "/" { self.root.clone() } else { format!("{}{client}", self.root) }
    }
}

/// Un appareil, tel que l'interface le montre (jamais d'empreinte).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Identifiant (8 hex).
    pub id: String,
    /// Nom donné par le titulaire.
    pub label: String,
    /// Dossier.
    pub root: String,
    /// Mode.
    pub mode: Mode,
    /// Création (secondes Unix).
    pub created_at: i64,
    /// Dernier usage.
    pub last_used: Option<i64>,
}

/// Échec d'une opération sur les appareils.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    /// Saisie refusée.
    #[error("{0}")]
    Invalid(String),
    /// Appareil inconnu.
    #[error("appareil inconnu")]
    NotFound,
    /// Base.
    #[error("{0}")]
    Db(String),
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn sha(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Dossier racine acceptable : normalisé, pas `/`, ni un dossier réservé, ni caché.
fn check_root(raw: &str) -> Result<String, DeviceError> {
    let root = normalize_path(raw).map_err(|_| DeviceError::Invalid("dossier invalide".into()))?;
    let first = root.trim_start_matches('/').split('/').next().unwrap_or("");
    if root == "/" || RESERVED.iter().any(|r| r.eq_ignore_ascii_case(first)) || root.split('/').any(|s| s.starts_with('.')) {
        return Err(DeviceError::Invalid("choisir un sous-dossier du uCloud (ni la racine, ni le coffre, les contacts, l'agenda, les partages, les sauvegardes, ni un dossier caché)".into()));
    }
    Ok(root)
}

/// Crée un appareil et son dossier ; rend l'appareil et son **jeton, montré cette seule fois**.
///
/// # Errors
/// Saisie invalide, trop d'appareils ou base indisponible.
pub async fn create(db: &Db, owner: &str, label: &str, root: &str, mode: Mode) -> Result<(Device, String), DeviceError> {
    let label: String = label.trim().chars().filter(|c| !c.is_control()).take(60).collect();
    if label.is_empty() {
        return Err(DeviceError::Invalid("nom de l'appareil attendu".into()));
    }
    let root = check_root(root)?;
    if list(db, owner).await?.len() >= MAX_DEVICES {
        return Err(DeviceError::Invalid(format!("{MAX_DEVICES} appareils au plus : en retirer un d'abord")));
    }
    let mut raw = [0u8; 20];
    getrandom::fill(&mut raw).map_err(|e| DeviceError::Db(format!("aléa du système indisponible : {e}")))?;
    let (id, token) = (hex::encode(&raw[..4]), format!("dv_{}", hex::encode(&raw[4..])));
    let (o, r) = (owner.to_owned(), root.clone());
    vfs_tx(db, move |tx| match vfs::get_entry(tx, &o, &r)? {
        Some(e) if e.kind == vfs::EntryKind::File => Err(vfs::VfsError::NotCollection),
        Some(_) => Ok(()),
        None => mkdir_p_tx(tx, &o, &r, now()),
    })
    .await
    .map_err(|e| DeviceError::Invalid(format!("dossier inutilisable : {e}")))?;
    let (o, i, l, h, r, m) = (owner.to_owned(), id.clone(), label.clone(), sha(&token), root.clone(), mode.as_str());
    db.writer()
        .execute(
            "INSERT INTO dav_tokens(owner_hex, id, label, token_sha256, root, mode, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            vec![o.into(), i.into(), l.into(), h.into(), r.into(), m.to_owned().into(), now().into()],
        )
        .await
        .map_err(|e: DbError| DeviceError::Db(e.to_string()))?;
    Ok((Device { id, label, root, mode, created_at: now(), last_used: None }, token))
}

/// Appareils du compte, sans leur empreinte, révoqués exclus.
///
/// # Errors
/// Base indisponible.
pub async fn list(db: &Db, owner: &str) -> Result<Vec<Device>, DeviceError> {
    let o = owner.to_owned();
    db.readers()
        .read(move |c| {
            let mut s = c.prepare("SELECT id, label, root, mode, created_at, last_used FROM dav_tokens WHERE owner_hex = ?1 AND revoked_at IS NULL ORDER BY created_at, id")?;
            let rows = s.query_map([o], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, Option<i64>>(5)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?.into_iter().filter_map(|(id, label, root, mode, created_at, last_used)| Some(Device { id, label, root, mode: Mode::parse(&mode)?, created_at, last_used })).collect())
        })
        .await
        .map_err(|e| DeviceError::Db(e.to_string()))
}

/// Révoque un appareil : son jeton cesse de fonctionner tout de suite. Ce qu'il a déposé reste.
///
/// # Errors
/// Appareil inconnu.
pub async fn revoke(db: &Db, owner: &str, id: &str) -> Result<(), DeviceError> {
    let (o, i) = (owner.to_owned(), id.to_owned());
    let n = db
        .writer()
        .execute("UPDATE dav_tokens SET revoked_at = ?3 WHERE owner_hex = ?1 AND id = ?2 AND revoked_at IS NULL", vec![o.into(), i.into(), now().into()])
        .await
        .map_err(|e| DeviceError::Db(e.to_string()))?
        .changes;
    if n == 0 { Err(DeviceError::NotFound) } else { Ok(()) }
}

/// Le jeton présenté est-il celui d'un appareil de `owner` ? Rend son confinement. L'empreinte est calculée dans tous les cas.
pub async fn lookup(db: &Db, owner: &str, presented: &str) -> Option<Scope> {
    let (o, h) = (owner.to_owned(), sha(presented));
    let found: Option<(String, String, String)> = db
        .readers()
        .read(move |c| {
            Ok(rusqlite::OptionalExtension::optional(c.query_row(
                "SELECT id, root, mode FROM dav_tokens WHERE owner_hex = ?1 AND token_sha256 = ?2 AND revoked_at IS NULL",
                params![o, h],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ))?)
        })
        .await
        .ok()
        .flatten();
    let (id, root, mode) = found?;
    let mode = Mode::parse(&mode)?;
    // Dernier usage, au plus une écriture par minute et par appareil.
    let (o, i, t) = (owner.to_owned(), id.clone(), now());
    let _ = db.writer().execute("UPDATE dav_tokens SET last_used = ?3 WHERE owner_hex = ?1 AND id = ?2 AND (last_used IS NULL OR last_used < ?3 - 60)", vec![o.into(), i.into(), t.into()]).await;
    Some(Scope { id, root, mode })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_must_be_ordinary_subfolders() {
        assert_eq!(check_root("/Photos/Téléphone/").unwrap(), "/Photos/Téléphone");
        for bad in ["/", "", "/Coffre", "/coffre/x", "/Contacts", "/Calendrier/a", "/Partages", "/Sauvegardes", "/.cache", "/Photos/.cachés", "/a/../.."] {
            assert!(check_root(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn modes_gate_the_methods() {
        for m in ["OPTIONS", "PROPFIND"] {
            assert!([Mode::Write, Mode::ReadWrite, Mode::Read].iter().all(|x| x.allows(m)));
        }
        assert!(Mode::Write.allows("PUT") && Mode::Write.allows("MKCOL") && !Mode::Write.allows("GET") && !Mode::Write.allows("DELETE") && !Mode::Write.allows("HEAD"));
        assert!(Mode::Read.allows("GET") && !Mode::Read.allows("PUT") && !Mode::Read.allows("DELETE") && !Mode::Read.allows("MKCOL"));
        assert!(Mode::ReadWrite.allows("GET") && Mode::ReadWrite.allows("DELETE") && Mode::ReadWrite.allows("PUT"));
        let s = Scope { id: "x".into(), root: "/Photos/Tel".into(), mode: Mode::Write };
        assert_eq!((s.real("/").as_str(), s.real("/a/b.jpg").as_str()), ("/Photos/Tel", "/Photos/Tel/a/b.jpg"));
    }
}
