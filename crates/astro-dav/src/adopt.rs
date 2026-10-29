//! Adoption d'un fichier envoyé par un ami : le destinataire référence le **même blob chiffré** avec la **même clé**
//! (rien n'est ré-encodé ni recopié). Il le range sous `/Partages/De_<expéditeur>/`, visible dans son explorateur et
//! par tout client WebDAV. Utilisé par le partage entre MULTIPASS d'une même station et par la réception d'un fichier
//! envoyé depuis une autre station (message NIP-17 emballé en NIP-59).

use astro_db::Db;
use rusqlite::params;

use crate::vfs::{self, VfsError, vfs_tx};

/// Fichier chiffré à adopter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFile {
    /// CID du blob chiffré (déjà présent sur cette station, ou rapatrié avant l'appel).
    pub cid: String,
    /// Clé de fichier, 64 hexadécimaux.
    pub key_hex: String,
    /// Type MIME.
    pub mime: String,
    /// Taille du clair.
    pub size: u64,
    /// SHA-256 du clair.
    pub sha_plain: String,
    /// Nom du fichier, tel que l'expéditeur l'a donné (nettoyé ici).
    pub name: String,
}

/// Nom de fichier sûr : jamais de barre, ni `.`/`..`, 120 caractères au plus ; `fichier` si rien ne reste.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control() && !matches!(c, '/' | '\\')).take(120).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_owned();
    if cleaned.is_empty() { "fichier".to_owned() } else { cleaned }
}

/// Nom de dossier sûr pour un expéditeur : partie locale de l'email, sinon 8 premiers hexadécimaux de sa clé.
pub fn sender_label(email: Option<&str>, sender_hex: &str) -> String {
    let local: String = email.and_then(|e| e.split('@').next()).unwrap_or("").chars().filter(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-')).take(40).collect();
    if local.is_empty() || local == "." || local == ".." { sender_hex[..8.min(sender_hex.len())].to_owned() } else { local }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Range `file` dans `/Partages/De_<label>/` chez `recipient` (nom suffixé par la fin du CID en cas de collision) et
/// rend son chemin. `origin` : provenance notée dans l'entrée (`facecloud-share`, `friend-share`).
pub async fn adopt_shared_file(db: &Db, recipient: &str, sender_hex: &str, label: &str, file: &SharedFile, origin: &str) -> Result<String, VfsError> {
    let (recipient, sender_hex, file, origin) = (recipient.to_owned(), sender_hex.to_owned(), file.clone(), origin.to_owned());
    let dir = format!("/Partages/De_{}", sender_label(Some(label), &sender_hex));
    vfs_tx(db, move |tx| {
        for d in ["/Partages", dir.as_str()] {
            match vfs::mkdir_tx(tx, &recipient, d, now()) {
                Ok(()) | Err(VfsError::Exists) => {}
                Err(e) => return Err(e),
            }
        }
        let name = safe_file_name(&file.name);
        let mut target = format!("{dir}/{name}");
        if let Some(existing) = vfs::get_entry(tx, &recipient, &target)? {
            // Le même blob déjà rangé sous ce nom : rien à refaire.
            if existing.cid.as_deref() == Some(file.cid.as_str()) {
                return Ok(target);
            }
            let tail = &file.cid[file.cid.len().saturating_sub(8)..];
            target = match name.rsplit_once('.') {
                Some((stem, ext)) => format!("{dir}/{stem}_{tail}.{ext}"),
                None => format!("{dir}/{name}_{tail}"),
            };
        }
        let data = serde_json::json!({ "type": "file", "cid": file.cid, "enc": "uenc2", "mime": file.mime, "size_plain": file.size, "sha256_plain": file.sha_plain, "mtime": now(), "shared_by": sender_hex }).to_string();
        tx.execute(
            "INSERT OR REPLACE INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, sha_orig, sha_plain, origin, data) VALUES (?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10)",
            params![recipient, target, vfs::parent_of(&target), file.cid, file.mime, i64::try_from(file.size).unwrap_or(0), now(), file.sha_plain, origin, data],
        )?;
        tx.execute("INSERT OR REPLACE INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", params![recipient, file.cid, file.key_hex])?;
        Ok(target)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_labels_are_safe_path_segments() {
        assert_eq!(safe_file_name("../../etc/passwd"), "....etcpasswd".trim_start_matches('.'));
        assert_eq!(safe_file_name("photo 1.jpg"), "photo 1.jpg");
        assert_eq!(safe_file_name("..."), "fichier");
        assert_eq!(safe_file_name("a\\b/c\u{7}"), "abc");
        assert_eq!(safe_file_name(&"x".repeat(500)).chars().count(), 120);
        assert_eq!(sender_label(Some("alice.dupont@example.org"), "ab12cd34ef"), "alice.dupont");
        assert_eq!(sender_label(Some("../../etc@x.org"), "ab12cd34ef"), "....etc");
        assert_eq!(sender_label(None, "ab12cd34ef56"), "ab12cd34");
        assert_eq!(sender_label(Some("..@x.org"), "ab12cd34ef56"), "ab12cd34");
    }
}
