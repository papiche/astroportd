//! Codec UENC.
//!
//! # Formats
//!
//! **v1** (legacy, `tools/uenc_codec.py`) : `"UENC" | 0x01 | enc_type=0x01 | iv[12] | ciphertext | tag[16]`,
//! AES-256-GCM, sans AAD. Lecture seule : aucun CID IPFS existant n'est cassé.
//!
//! **v2** (écriture) : `"UENC" | 0x02 | enc_type | chunk_log2 | nonce_prefix[7]` (14 octets) suivis de
//! chunks `ciphertext | tag[16]`. Chaque chunk contient `2^chunk_log2` octets de clair (64 Kio),
//! sauf le dernier qui en contient entre 0 et `2^chunk_log2`.
//! Construction STREAM : nonce de 12 octets = `prefix[7] | compteur u32 BE | drapeau « dernier chunk »`,
//! et l'en-tête complet sert d'AAD. Réordonner, tronquer ou prolonger le flux fait échouer l'authentification.
//!
//! `enc_type` : `0x01` AES-256-GCM, `0x02` ChaCha20-Poly1305.

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::Aes256Gcm;
use chacha20poly1305::ChaCha20Poly1305;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use crate::UencKey;

pub mod v1;
pub mod v2;

pub use v2::{CHUNK_LOG2, HEADER_LEN, v2_plaintext_len};

/// Octets magiques d'un blob UENC.
pub const MAGIC: &[u8; 4] = b"UENC";
/// Taille du tag d'authentification (GCM et Poly1305).
pub const TAG_LEN: usize = 16;

/// Erreurs du codec.
#[derive(Debug, thiserror::Error)]
pub enum UencError {
    /// Clé absente, mal formée ou de mauvaise taille.
    #[error("clé invalide : 64 caractères hexadécimaux attendus")]
    BadKey,
    /// Le blob ne commence pas par `UENC`.
    #[error("ce n'est pas un blob UENC")]
    NotUenc,
    /// Version de format inconnue.
    #[error("version UENC non supportée : {0}")]
    UnsupportedVersion(u8),
    /// Algorithme inconnu.
    #[error("type de chiffrement non supporté : {0:#04x}")]
    UnsupportedCipher(u8),
    /// Taille de chunk hors des bornes acceptées (2^12 à 2^24).
    #[error("taille de chunk invalide : 2^{0}")]
    BadChunkSize(u8),
    /// Blob incomplet.
    #[error("blob tronqué ou corrompu")]
    Truncated,
    /// Tag invalide : mauvaise clé ou données altérées.
    #[error("authentification échouée (clé erronée ou données altérées)")]
    AuthFailed,
    /// Un blob v1 dépasse la limite de lecture en mémoire.
    #[error("blob v1 trop gros (> {0} octets)")]
    V1TooLarge(usize),
    /// Plage demandée incohérente avec la taille du clair.
    #[error("plage invalide")]
    BadRange,
    /// Plus de 2^32 chunks.
    #[error("trop de chunks")]
    TooManyChunks,
    /// Générateur aléatoire système indisponible.
    #[error("générateur aléatoire indisponible")]
    Rng,
    /// Erreur d'entrée/sortie.
    #[error("entrée/sortie : {0}")]
    Io(#[from] std::io::Error),
}

/// Algorithme d'un blob v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cipher {
    /// AES-256-GCM (code `0x01`).
    Aes256Gcm,
    /// ChaCha20-Poly1305 (code `0x02`).
    ChaCha20Poly1305,
}

impl Cipher {
    /// Choisit AES-GCM si le CPU l'accélère (x86-64 AES+PCLMUL, ARMv8 AES), sinon ChaCha20-Poly1305
    /// (Raspberry Pi 4 par exemple).
    pub fn detect() -> Self {
        if has_hw_aes() { Self::Aes256Gcm } else { Self::ChaCha20Poly1305 }
    }

    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Aes256Gcm => 0x01,
            Self::ChaCha20Poly1305 => 0x02,
        }
    }

    pub(crate) fn from_code(c: u8) -> Result<Self, UencError> {
        match c {
            0x01 => Ok(Self::Aes256Gcm),
            0x02 => Ok(Self::ChaCha20Poly1305),
            other => Err(UencError::UnsupportedCipher(other)),
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn has_hw_aes() -> bool {
    std::arch::is_x86_feature_detected!("aes") && std::arch::is_x86_feature_detected!("pclmulqdq")
}
#[cfg(target_arch = "aarch64")]
fn has_hw_aes() -> bool {
    std::arch::is_aarch64_feature_detected!("aes")
}
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn has_hw_aes() -> bool {
    false
}

/// AEAD concret, construit une fois par flux.
pub(crate) enum Aead {
    Aes(Box<Aes256Gcm>),
    Chacha(Box<ChaCha20Poly1305>),
}

impl Aead {
    pub(crate) fn new(cipher: Cipher, key: &UencKey) -> Result<Self, UencError> {
        match cipher {
            Cipher::Aes256Gcm => Aes256Gcm::new_from_slice(key.as_bytes())
                .map(|c| Self::Aes(Box::new(c)))
                .map_err(|_| UencError::BadKey),
            Cipher::ChaCha20Poly1305 => ChaCha20Poly1305::new_from_slice(key.as_bytes())
                .map(|c| Self::Chacha(Box::new(c)))
                .map_err(|_| UencError::BadKey),
        }
    }

    /// Chiffre `buf` en place et ajoute le tag.
    pub(crate) fn seal(&self, nonce: &[u8; 12], aad: &[u8], buf: &mut Vec<u8>) -> Result<(), UencError> {
        let n = GenericArray::from_slice(nonce);
        match self {
            Self::Aes(c) => c.encrypt_in_place(n, aad, buf),
            Self::Chacha(c) => c.encrypt_in_place(n, aad, buf),
        }
        .map_err(|_| UencError::AuthFailed)
    }

    /// Vérifie le tag puis déchiffre `buf` en place (le tag est retiré).
    pub(crate) fn open(&self, nonce: &[u8; 12], aad: &[u8], buf: &mut Vec<u8>) -> Result<(), UencError> {
        let n = GenericArray::from_slice(nonce);
        match self {
            Self::Aes(c) => c.decrypt_in_place(n, aad, buf),
            Self::Chacha(c) => c.decrypt_in_place(n, aad, buf),
        }
        .map_err(|_| UencError::AuthFailed)
    }
}

/// Remplit `buf` avec jusqu'à `n` octets ; s'arrête avant si EOF. `buf` est vide si le flux est fini.
pub(crate) async fn read_full<R: AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    n: usize,
) -> Result<(), UencError> {
    buf.clear();
    buf.resize(n, 0);
    let mut filled = 0;
    while filled < n {
        let got = reader.read(&mut buf[filled..]).await?;
        if got == 0 {
            break;
        }
        filled += got;
    }
    buf.truncate(filled);
    Ok(())
}

/// Chiffre un flux en UENC v2 (buffer fixe de 2 chunks, mémoire O(1)). Retourne le nombre d'octets écrits.
pub async fn encrypt_stream<R, W>(
    key: &UencKey,
    cipher: Cipher,
    reader: &mut R,
    writer: &mut W,
) -> Result<u64, UencError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    v2::encrypt_stream(key, cipher, reader, writer).await
}

/// Déchiffre un flux UENC, version détectée à l'exécution (v1 en mémoire, v2 en streaming).
pub async fn decrypt_stream<R, W>(key: &UencKey, reader: &mut R, writer: &mut W) -> Result<u64, UencError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut head = [0u8; 6];
    read_head(reader, &mut head).await?;
    match head[4] {
        1 => {
            let blob = v1::read_remaining(reader, &head).await?;
            let plain = v1::decrypt(key, &blob)?;
            writer.write_all(&plain).await?;
            writer.flush().await?;
            Ok(plain.len() as u64)
        }
        2 => v2::decrypt_after_head(key, &head, reader, writer).await,
        other => Err(UencError::UnsupportedVersion(other)),
    }
}

/// Déchiffre les octets `start..=end_inclusive` du clair sans lire les autres chunks (v2).
///
/// Pour un blob v1, le blob est déchiffré en entier puis découpé.
/// `total_len` est la taille du blob chiffré (taille IPFS ou en-tête `Content-Length` amont).
pub async fn decrypt_range<R, W>(
    key: &UencKey,
    reader: &mut R,
    total_len: u64,
    start: u64,
    end_inclusive: u64,
    writer: &mut W,
) -> Result<u64, UencError>
where
    R: AsyncRead + AsyncSeek + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut head = [0u8; 6];
    reader.seek(std::io::SeekFrom::Start(0)).await?;
    read_head(reader, &mut head).await?;
    match head[4] {
        1 => {
            let blob = v1::read_remaining(reader, &head).await?;
            let plain = v1::decrypt(key, &blob)?;
            let len = plain.len() as u64;
            if start > end_inclusive || start >= len {
                return Err(UencError::BadRange);
            }
            let end = end_inclusive.min(len - 1);
            let slice = plain.get(start as usize..=end as usize).ok_or(UencError::BadRange)?;
            writer.write_all(slice).await?;
            writer.flush().await?;
            Ok(slice.len() as u64)
        }
        2 => v2::decrypt_range_after_head(key, &head, reader, total_len, start, end_inclusive, writer).await,
        other => Err(UencError::UnsupportedVersion(other)),
    }
}

async fn read_head<R: AsyncRead + Unpin>(reader: &mut R, head: &mut [u8; 6]) -> Result<(), UencError> {
    match reader.read_exact(head).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(UencError::NotUenc),
        Err(e) => return Err(e.into()),
    }
    if &head[..4] != MAGIC {
        return Err(UencError::NotUenc);
    }
    Ok(())
}

/// Chiffre un tampon en mémoire (UENC v2).
pub async fn encrypt_to_vec(key: &UencKey, cipher: Cipher, plain: &[u8]) -> Result<Vec<u8>, UencError> {
    let mut input = plain;
    let mut out = Vec::with_capacity(plain.len() + HEADER_LEN + TAG_LEN);
    encrypt_stream(key, cipher, &mut input, &mut out).await?;
    Ok(out)
}

/// Déchiffre un tampon en mémoire (v1 ou v2).
pub async fn decrypt_to_vec(key: &UencKey, blob: &[u8]) -> Result<Vec<u8>, UencError> {
    let mut input = blob;
    let mut out = Vec::with_capacity(blob.len());
    decrypt_stream(key, &mut input, &mut out).await?;
    Ok(out)
}
