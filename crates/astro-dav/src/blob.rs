//! Stockage de blobs adressés par contenu.
//!
//! [`FsBlobStore`] range les blobs chiffrés dans un dossier, nommés par leur CID v1 `raw` + sha2-256
//! (`bafkrei…`). Un adaptateur Kubo (`/api/v0/add`) implémentera le même trait pour publier sur IPFS.

pub use crate::kubo::{AutoBlobStore, KuboBlobStore};

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncSeek, AsyncWrite, AsyncWriteExt};

/// Erreurs de stockage de blobs.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// Blob absent.
    #[error("blob introuvable")]
    NotFound,
    /// Identifiant qui n'est pas un CID.
    #[error("CID invalide")]
    BadCid,
    /// Erreur d'entrée/sortie.
    #[error("entrée/sortie : {0}")]
    Io(#[from] io::Error),
}

/// Blob stocké.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    /// CID du blob.
    pub cid: String,
    /// Taille en octets.
    pub size: u64,
}

/// Écriture en flux d'un nouveau blob : rien n'est visible avant `commit`.
pub trait BlobWriter: AsyncWrite + Unpin + Send {
    /// Termine l'écriture, calcule le CID et publie le blob.
    fn commit(self) -> impl Future<Output = Result<BlobMeta, BlobError>> + Send;
}

/// Magasin de blobs.
pub trait BlobStore: Send + Sync + 'static {
    /// Écrivain en flux.
    type Writer: BlobWriter;
    /// Lecteur à accès aléatoire (pour les requêtes `Range`).
    type Reader: AsyncRead + AsyncSeek + Unpin + Send + 'static;

    /// Ouvre un nouvel écrivain.
    fn begin(&self) -> impl Future<Output = Result<Self::Writer, BlobError>> + Send;
    /// Ouvre un blob en lecture ; retourne aussi sa taille.
    fn open(&self, cid: &str) -> impl Future<Output = Result<(Self::Reader, u64), BlobError>> + Send;
    /// Supprime un blob (sans erreur s'il est déjà absent).
    fn remove(&self, cid: &str) -> impl Future<Output = Result<(), BlobError>> + Send;
}

const B32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// CID v1, codec `raw` (0x55), multihash sha2-256 (0x12 0x20), en base32 minuscule préfixée de `b`.
pub fn cid_raw_sha256(digest: &[u8; 32]) -> String {
    let mut bytes = vec![0x01, 0x55, 0x12, 0x20];
    bytes.extend_from_slice(digest);
    let mut out = String::from("b");
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(B32[((acc >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(B32[((acc << (5 - bits)) & 31) as usize]));
    }
    out
}

/// `true` si `s` a la forme d'un CID v1 sha2-256 en base32 : `bafkrei…` (bloc `raw`, [`cid_raw_sha256`])
/// ou `bafybei…` (racine `dag-pb` d'un fichier UnixFS découpé par Kubo).
pub fn is_cid(s: &str) -> bool {
    s.len() == 59 && (s.starts_with("bafkrei") || s.starts_with("bafybei")) && s.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// Magasin sur disque.
#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    /// Crée (si besoin) le dossier de stockage.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join(".tmp"))?;
        Ok(Self { root })
    }

    /// Dossier de stockage.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_of(&self, cid: &str) -> Result<PathBuf, BlobError> {
        if is_cid(cid) { Ok(self.root.join(cid)) } else { Err(BlobError::BadCid) }
    }
}

/// Écrivain du magasin disque : fichier temporaire haché au fil de l'eau.
pub struct FsBlobWriter {
    file: tokio::fs::File,
    hasher: Sha256,
    size: u64,
    tmp: PathBuf,
    root: PathBuf,
    committed: bool,
}

impl AsyncWrite for FsBlobWriter {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.file).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &poll {
            this.hasher.update(&buf[..*n]);
            this.size += *n as u64;
        }
        poll
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_shutdown(cx)
    }
}

impl BlobWriter for FsBlobWriter {
    async fn commit(mut self) -> Result<BlobMeta, BlobError> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        let digest: [u8; 32] = self.hasher.clone().finalize().into();
        let cid = cid_raw_sha256(&digest);
        let dest = self.root.join(&cid);
        if tokio::fs::try_exists(&dest).await? {
            tokio::fs::remove_file(&self.tmp).await?; // même contenu déjà présent
        } else {
            tokio::fs::rename(&self.tmp, &dest).await?;
        }
        self.committed = true;
        Ok(BlobMeta { cid, size: self.size })
    }
}

impl Drop for FsBlobWriter {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

impl BlobStore for FsBlobStore {
    type Writer = FsBlobWriter;
    type Reader = tokio::fs::File;

    async fn begin(&self) -> Result<FsBlobWriter, BlobError> {
        let mut id = [0u8; 12];
        getrandom::fill(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
        let tmp = self.root.join(".tmp").join(hex::encode(id));
        let file = tokio::fs::File::create(&tmp).await?;
        Ok(FsBlobWriter { file, hasher: Sha256::new(), size: 0, tmp, root: self.root.clone(), committed: false })
    }

    async fn open(&self, cid: &str) -> Result<(tokio::fs::File, u64), BlobError> {
        let path = self.path_of(cid)?;
        match tokio::fs::File::open(&path).await {
            Ok(f) => {
                let len = f.metadata().await?.len();
                Ok((f, len))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(BlobError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn remove(&self, cid: &str) -> Result<(), BlobError> {
        match tokio::fs::remove_file(self.path_of(cid)?).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    use super::*;

    fn dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("astro-blob-{tag}-{}", std::process::id()))
    }

    #[test]
    fn cid_matches_known_ipfs_values() {
        // `ipfs add --cid-version 1 --raw-leaves` d'un fichier vide et de « hello world », vérifiés avec hashlib + base32.
        assert_eq!(cid_raw_sha256(&Sha256::digest(b"").into()), "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku");
        assert_eq!(cid_raw_sha256(&Sha256::digest(b"hello world").into()), "bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e");
        assert!(is_cid("bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku"));
        assert!(is_cid("bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi"), "racine dag-pb de Kubo");
        assert!(!is_cid("../etc/passwd") && !is_cid("Qmabc") && !is_cid("") && !is_cid(&"bafkrei".repeat(9)));
    }

    #[tokio::test]
    async fn write_commit_open_seek_remove() {
        let d = dir("rw");
        let store = FsBlobStore::open(&d).unwrap();
        let mut w = store.begin().await.unwrap();
        w.write_all(b"0123456789").await.unwrap();
        let meta = w.commit().await.unwrap();
        assert_eq!(meta.size, 10);
        assert!(is_cid(&meta.cid));
        let (mut r, len) = store.open(&meta.cid).await.unwrap();
        assert_eq!(len, 10);
        r.seek(io::SeekFrom::Start(4)).await.unwrap();
        let mut rest = String::new();
        r.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, "456789");
        store.remove(&meta.cid).await.unwrap();
        store.remove(&meta.cid).await.unwrap();
        assert!(matches!(store.open(&meta.cid).await, Err(BlobError::NotFound)));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn uncommitted_writes_leave_nothing_and_bad_cids_are_refused() {
        let d = dir("drop");
        let store = FsBlobStore::open(&d).unwrap();
        {
            let mut w = store.begin().await.unwrap();
            w.write_all(b"abandonne").await.unwrap();
        }
        assert_eq!(std::fs::read_dir(d.join(".tmp")).unwrap().count(), 0);
        assert!(matches!(store.open("../secret").await, Err(BlobError::BadCid)));
        assert!(matches!(store.remove("x").await, Err(BlobError::BadCid)));
        // Deux écritures du même contenu donnent le même CID, un seul fichier.
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut w = store.begin().await.unwrap();
            w.write_all(b"identique").await.unwrap();
            ids.push(w.commit().await.unwrap().cid);
        }
        assert_eq!(ids[0], ids[1]);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2, ".tmp + 1 blob");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
