//! Dépôt d'un flux arbitraire dans l'uCloud d'un compte, hors WebDAV (documents rapatriés par BRO).
//!
//! Mêmes garanties qu'un `PUT` : chiffrement UENC v2 en flux (le clair ne touche jamais le disque), plafond par
//! fichier et quota du compte (relevé par un bail de disque), clé dans le trousseau, miniature, publication du
//! manifeste et rappel FaceCloud.

use std::sync::Arc;

use astro_crypto::UencKey;
use astro_crypto::uenc::{self, Cipher};
use sha2::{Digest, Sha256};
use tokio::io::AsyncRead;

use crate::blob::{BlobError, BlobStore, BlobWriter};
use crate::server::{DavState, Measure};
use crate::vfs::{self, EntryKind, NewFile, VfsError, vfs_tx};

/// Erreurs de dépôt.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// Compte non hébergé ici.
    #[error("compte non hébergé ici")]
    NoAccount,
    /// Fichier au-delà du plafond ou du quota.
    #[error("{0}")]
    TooLarge(String),
    /// Nom ou dossier refusé.
    #[error("nom de fichier invalide")]
    BadName,
    /// VFS.
    #[error("uCloud : {0}")]
    Vfs(#[from] VfsError),
    /// Stockage.
    #[error("stockage : {0}")]
    Blob(#[from] BlobError),
    /// Lecture de la source interrompue.
    #[error("source interrompue : {0}")]
    Source(String),
    /// Chiffrement.
    #[error("chiffrement impossible")]
    Crypto,
}

/// Fichier déposé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// Chemin final dans l'uCloud (le nom a pu changer pour éviter un écrasement).
    pub path: String,
    /// Octets de clair.
    pub size: u64,
    /// Type MIME retenu.
    pub mime: String,
}

/// Nom de fichier sûr : pas de séparateur, de caractère de contrôle ni de nom réservé, 120 caractères au plus.
pub fn sanitize_name(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().chars().take(120).collect::<String>();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// « nom.ext » → « nom (2).ext ».
fn numbered(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 8 => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// Dépose `reader` sous `dir/name` dans l'uCloud de `owner`. `dir` est créé s'il n'existe pas ; si le nom est pris,
/// un numéro est ajouté (un document rapatrié n'écrase jamais un fichier existant).
pub async fn ingest<B: BlobStore, R: AsyncRead + Unpin>(st: &Arc<DavState<B>>, owner: &str, dir: &str, name: &str, mime_hint: Option<&str>, reader: R) -> Result<Ingested, IngestError> {
    ingest_with(st, owner, dir, name, mime_hint, reader, false).await
}

/// Comme [`ingest`], mais écrit EXACTEMENT sous `dir/name` : un fichier existant est remplacé (fiche de contact mise à jour).
pub async fn replace<B: BlobStore, R: AsyncRead + Unpin>(st: &Arc<DavState<B>>, owner: &str, dir: &str, name: &str, mime_hint: Option<&str>, reader: R) -> Result<Ingested, IngestError> {
    ingest_with(st, owner, dir, name, mime_hint, reader, true).await
}

async fn ingest_with<B: BlobStore, R: AsyncRead + Unpin>(st: &Arc<DavState<B>>, owner: &str, dir: &str, name: &str, mime_hint: Option<&str>, reader: R, replace: bool) -> Result<Ingested, IngestError> {
    let account = crate::auth::lookup_by_hex(&st.db, owner).await.ok_or(IngestError::NoAccount)?;
    let limits = st.limits_for(&account).await;
    let dir = vfs::normalize_path(dir)?;
    let name = sanitize_name(name).ok_or(IngestError::BadName)?;

    // Dossier et nom libre.
    let (o, d, n) = (owner.to_owned(), dir.clone(), name.clone());
    let path = vfs_tx(&st.db, move |tx| {
        match vfs::get_entry(tx, &o, &d)? {
            None => vfs::mkdir_tx(tx, &o, &d, now())?,
            Some(e) if e.kind == EntryKind::File => return Err(VfsError::NotCollection),
            Some(_) => {}
        }
        if replace {
            let full = if d == "/" { format!("/{n}") } else { format!("{d}/{n}") };
            return match vfs::get_entry(tx, &o, &full)? {
                Some(e) if e.kind == EntryKind::Dir => Err(VfsError::IsCollection),
                _ => Ok(full),
            };
        }
        for i in 1..=999u32 {
            let candidate = if i == 1 { n.clone() } else { numbered(&n, i) };
            let full = if d == "/" { format!("/{candidate}") } else { format!("{d}/{candidate}") };
            if vfs::get_entry(tx, &o, &full)?.is_none() {
                return Ok(full);
            }
        }
        Err(VfsError::Exists)
    })
    .await?;

    let mime = mime_hint.map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase()).filter(|m| !m.is_empty() && !matches!(m.as_str(), "application/octet-stream" | "binary/octet-stream" | "application/x-www-form-urlencoded")).unwrap_or_else(|| vfs::guess_mime(&path).to_owned());
    let key = UencKey::generate().map_err(|_| IngestError::Crypto)?;
    let mut blob = st.blobs.begin().await?;
    let mut measured = Measure { inner: reader, hasher: Sha256::new(), count: 0, max: limits.max_file };
    let written = uenc::encrypt_stream(&key, Cipher::detect(), &mut measured, &mut blob).await;
    if measured.count > limits.max_file {
        return Err(IngestError::TooLarge(format!("fichier au-delà du plafond de {} Mo", limits.max_file >> 20)));
    }
    written.map_err(|e| IngestError::Source(e.to_string()))?;
    let (size, sha_plain) = (measured.count, hex::encode(measured.hasher.finalize()));
    let meta = blob.commit().await?;
    let file = NewFile { cid: meta.cid.clone(), size_cid: meta.size, size_plain: size, sha_plain, mime: mime.clone(), key_hex: key.to_hex(), now: now() };
    let (o, p) = (owner.to_owned(), path.clone());
    match vfs_tx(&st.db, move |tx| vfs::commit_file_tx(tx, &o, &p, &file, limits)).await {
        Ok(old) => {
            // Remplacement : l'ancien blob n'est plus référencé par personne, il est libéré.
            if let Some(prev) = old.and_then(|e| e.cid) {
                crate::server::release_blobs(st, vec![prev]).await;
            }
        }
        Err(e) => {
            let _ = st.blobs.remove(&meta.cid).await;
            return Err(match e {
                VfsError::QuotaExceeded => IngestError::TooLarge("quota de l'espace uCloud dépassé".into()),
                VfsError::TooLarge => IngestError::TooLarge("fichier au-delà du plafond".into()),
                other => other.into(),
            });
        }
    }
    if let Some(hook) = &st.on_upload {
        hook(owner, &path, &mime);
    }
    if crate::thumb::is_thumbable(&mime) {
        let (st, owner, path) = (st.clone(), owner.to_owned(), path.clone());
        tokio::spawn(async move {
            if let Err(e) = crate::thumb::generate(&st, &owner, &path).await {
                eprintln!("miniature de {path} impossible : {e}");
            }
        });
    }
    if let Some(p) = &st.publisher {
        p.notify(owner);
    }
    Ok(Ingested { path, size, mime })
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe_and_numbered() {
        assert_eq!(sanitize_name("../../etc/passwd").as_deref(), Some("_.._etc_passwd"));
        assert_eq!(sanitize_name("  contrat\n2026.pdf ").as_deref(), Some("contrat_2026.pdf"));
        assert_eq!(sanitize_name("...").as_deref(), None);
        assert_eq!(sanitize_name("").as_deref(), None);
        assert_eq!(sanitize_name(&"é".repeat(300)).unwrap().chars().count(), 120);
        assert_eq!(numbered("photo.jpg", 2), "photo (2).jpg");
        assert_eq!(numbered("Makefile", 3), "Makefile (3)");
        assert_eq!(numbered(".bashrc", 2), ".bashrc (2)");
    }
}
