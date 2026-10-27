//! Échange direct de blobs chiffrés entre stations : `/astro/blob/1.0.0`.
//!
//! Requête : `GET <cid>\n`. Réponse : 1 octet d'état (0 ok, 1 absent, 2 refusé, 3 requête invalide) puis, si ok,
//! la longueur (u64 BE) et les octets. Le demandeur **vérifie l'empreinte SHA-256 avant de valider** le blob :
//! un pair malveillant ne peut rien faire entrer dans le magasin sous un CID qui n'est pas le sien.
//! Seuls les CID `raw` (`bafkrei…`) sont échangés, car ce sont les seuls vérifiables sans DAG.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use astro_dav::blob::{BlobError, BlobMeta, BlobStore, BlobWriter, cid_raw_sha256, is_cid};
use futures_util::StreamExt;
use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p::{PeerId, StreamProtocol};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

use crate::{P2pError, P2pHandle};

/// Protocole d'échange de blobs.
pub const PROTOCOL: &str = "/astro/blob/1.0.0";
const OK: u8 = 0;
const NOT_FOUND: u8 = 1;
const REFUSED: u8 = 2;
const BAD_REQUEST: u8 = 3;
const CHUNK: usize = 64 * 1024;

/// Erreurs de rapatriement.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Aucun pair n'a pu fournir le blob.
    #[error("blob introuvable chez {tried} pair(s) : {reasons}")]
    Unavailable {
        /// Pairs interrogés.
        tried: usize,
        /// Raisons par pair.
        reasons: String,
    },
    /// CID non échangeable.
    #[error("CID non supporté (seuls les blocs `raw` bafkrei… sont échangés)")]
    BadCid,
    /// Stockage local.
    #[error("stockage : {0}")]
    Store(#[from] BlobError),
}

/// Réglages du serveur de blobs.
#[derive(Debug, Clone)]
pub struct ServeOpts {
    /// Pairs autorisés ; `None` = tout membre du réseau privé.
    pub allow: Option<HashSet<PeerId>>,
    /// Taille maximale servie.
    pub max_blob_bytes: u64,
}

impl Default for ServeOpts {
    fn default() -> Self {
        Self { allow: None, max_blob_bytes: 1 << 30 }
    }
}

/// Serveur de blobs : s'arrête à la destruction.
pub struct BlobServer {
    task: JoinHandle<()>,
}

impl Drop for BlobServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Sert les blobs de `store` aux pairs du maillage.
pub fn serve_blobs<B: BlobStore>(handle: &P2pHandle, store: Arc<B>, opts: ServeOpts) -> Result<BlobServer, P2pError> {
    let mut incoming = handle.control().accept(StreamProtocol::new(PROTOCOL)).map_err(|e| P2pError::Stream(e.to_string()))?;
    let task = tokio::spawn(async move {
        while let Some((peer, stream)) = incoming.next().await {
            if opts.allow.as_ref().is_some_and(|a| !a.contains(&peer)) {
                continue;
            }
            let (store, max) = (store.clone(), opts.max_blob_bytes);
            tokio::spawn(async move {
                let _ = serve_one(stream, store.as_ref(), max).await;
            });
        }
    });
    Ok(BlobServer { task })
}

async fn read_request(stream: &mut libp2p::Stream) -> std::io::Result<Option<String>> {
    let mut line = Vec::with_capacity(80);
    let mut byte = [0u8; 1];
    while line.len() < 80 {
        stream.read_exact(&mut byte).await?;
        if byte[0] == b'\n' {
            return Ok(String::from_utf8(line).ok().and_then(|l| l.strip_prefix("GET ").map(str::to_owned)));
        }
        line.push(byte[0]);
    }
    Ok(None)
}

async fn serve_one<B: BlobStore>(mut stream: libp2p::Stream, store: &B, max: u64) -> std::io::Result<()> {
    let Some(cid) = read_request(&mut stream).await? else {
        stream.write_all(&[BAD_REQUEST]).await?;
        return stream.close().await;
    };
    if !cid.starts_with("bafkrei") || !is_cid(&cid) {
        stream.write_all(&[BAD_REQUEST]).await?;
        return stream.close().await;
    }
    let Ok((mut reader, len)) = store.open(&cid).await else {
        stream.write_all(&[NOT_FOUND]).await?;
        return stream.close().await;
    };
    if len > max {
        stream.write_all(&[REFUSED]).await?;
        return stream.close().await;
    }
    stream.write_all(&[OK]).await?;
    stream.write_all(&len.to_be_bytes()).await?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n]).await?;
    }
    stream.close().await
}

/// Demande `cid` à `peer` et l'enregistre dans `store` après vérification de l'empreinte.
pub async fn fetch_from<B: BlobStore>(handle: &P2pHandle, peer: PeerId, cid: &str, store: &B, max_bytes: u64, timeout: Duration) -> Result<BlobMeta, FetchError> {
    if !cid.starts_with("bafkrei") || !is_cid(cid) {
        return Err(FetchError::BadCid);
    }
    let work = async {
        let mut control = handle.control();
        let mut stream = control.open_stream(peer, StreamProtocol::new(PROTOCOL)).await.map_err(|e| format!("ouverture du flux : {e}"))?;
        stream.write_all(format!("GET {cid}\n").as_bytes()).await.map_err(|e| e.to_string())?;
        let mut status = [0u8; 1];
        stream.read_exact(&mut status).await.map_err(|e| e.to_string())?;
        match status[0] {
            OK => {}
            NOT_FOUND => return Err("le pair n'a pas ce blob".to_owned()),
            REFUSED => return Err("le pair refuse de le servir (trop gros ou non autorisé)".to_owned()),
            _ => return Err("requête refusée par le pair".to_owned()),
        }
        let mut len = [0u8; 8];
        stream.read_exact(&mut len).await.map_err(|e| e.to_string())?;
        let len = u64::from_be_bytes(len);
        if len > max_bytes {
            return Err(format!("annonce {len} octets, au-delà de la limite de {max_bytes}"));
        }
        let mut writer = store.begin().await.map_err(|e| e.to_string())?;
        let (mut hasher, mut remaining, mut buf) = (Sha256::new(), len, vec![0u8; CHUNK]);
        while remaining > 0 {
            let want = usize::try_from(remaining.min(CHUNK as u64)).unwrap_or(CHUNK);
            let n = stream.read(&mut buf[..want]).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("flux interrompu avant la fin du blob".to_owned());
            }
            hasher.update(&buf[..n]);
            writer.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
            remaining -= n as u64;
        }
        // Vérification AVANT validation : un blob falsifié est jeté avec son fichier temporaire.
        if cid_raw_sha256(&hasher.finalize().into()) != cid {
            return Err("empreinte SHA-256 différente du CID demandé : blob rejeté".to_owned());
        }
        let meta = writer.commit().await.map_err(|e| e.to_string())?;
        Ok::<_, String>(meta)
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(Ok(meta)) => Ok(meta),
        Ok(Err(reason)) => Err(FetchError::Unavailable { tried: 1, reasons: format!("{peer} : {reason}") }),
        Err(_) => Err(FetchError::Unavailable { tried: 1, reasons: format!("{peer} : délai dépassé") }),
    }
}

/// Demande `cid` aux `peers` (dans l'ordre) ; ou à tous les pairs connectés si `peers` est vide.
pub async fn fetch_blob<B: BlobStore>(handle: &P2pHandle, peers: &[PeerId], cid: &str, store: &B, max_bytes: u64, timeout: Duration) -> Result<BlobMeta, FetchError> {
    let candidates: Vec<PeerId> = if peers.is_empty() { handle.connected_peers() } else { peers.to_vec() };
    let mut reasons = Vec::new();
    for peer in &candidates {
        match fetch_from(handle, *peer, cid, store, max_bytes, timeout).await {
            Ok(meta) => return Ok(meta),
            Err(FetchError::BadCid) => return Err(FetchError::BadCid),
            Err(FetchError::Unavailable { reasons: r, .. }) => reasons.push(r),
            Err(e) => reasons.push(e.to_string()),
        }
    }
    Err(FetchError::Unavailable { tried: candidates.len(), reasons: reasons.join(" ; ") })
}

/// Magasin qui rapatrie à la demande les blobs absents : `open` cherche d'abord en local, puis auprès des pairs
/// et enregistre le résultat. Branché sous WebDAV et FaceCloud, il rend Kubo facultatif pour les partages entre amis.
pub struct P2pBlobStore<B: BlobStore> {
    inner: Arc<B>,
    handle: Option<P2pHandle>,
    peers: Vec<PeerId>,
    max_bytes: u64,
    timeout: Duration,
}

impl<B: BlobStore> P2pBlobStore<B> {
    /// `peers` : pairs à interroger en priorité (vide = tous les pairs connectés).
    pub fn new(inner: Arc<B>, handle: P2pHandle, peers: Vec<PeerId>) -> Self {
        Self { inner, handle: Some(handle), peers, max_bytes: 1 << 30, timeout: Duration::from_secs(120) }
    }

    /// Sans maillage : se comporte comme `inner` (permet de n'avoir qu'un seul type de magasin selon la configuration).
    pub fn local_only(inner: Arc<B>) -> Self {
        Self { inner, handle: None, peers: Vec::new(), max_bytes: 1 << 30, timeout: Duration::from_secs(120) }
    }
}

impl<B: BlobStore> BlobStore for P2pBlobStore<B> {
    type Writer = B::Writer;
    type Reader = B::Reader;

    async fn begin(&self) -> Result<B::Writer, BlobError> {
        self.inner.begin().await
    }

    async fn open(&self, cid: &str) -> Result<(B::Reader, u64), BlobError> {
        match self.inner.open(cid).await {
            Err(BlobError::NotFound) => {}
            other => return other,
        }
        let Some(handle) = &self.handle else { return Err(BlobError::NotFound) };
        match fetch_blob(handle, &self.peers, cid, self.inner.as_ref(), self.max_bytes, self.timeout).await {
            Ok(_) => self.inner.open(cid).await,
            Err(FetchError::BadCid) => Err(BlobError::BadCid),
            Err(_) => Err(BlobError::NotFound),
        }
    }

    async fn remove(&self, cid: &str) -> Result<(), BlobError> {
        self.inner.remove(cid).await
    }
}
