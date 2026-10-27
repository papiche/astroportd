//! UENC v1 : format monolithique legacy (AES-256-GCM, IV de 12 octets, sans AAD).

use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Aead, Cipher, MAGIC, TAG_LEN, UencError};
use crate::UencKey;

/// Taille maximale lue en mémoire pour un blob v1.
///
/// Le legacy plafonnait les écritures à 20 Mo ; cette limite protège contre un blob v1 forgé.
pub const MAX_V1_BYTES: usize = 128 * 1024 * 1024;
const IV_LEN: usize = 12;
const HEADER_LEN: usize = 6;

/// Lit le reste d'un blob v1 dont les 6 premiers octets ont déjà été consommés.
pub(crate) async fn read_remaining<R: AsyncRead + Unpin>(
    reader: &mut R,
    head: &[u8; 6],
) -> Result<Vec<u8>, UencError> {
    let mut blob = head.to_vec();
    let limit = (MAX_V1_BYTES as u64) + 1;
    (&mut *reader).take(limit).read_to_end(&mut blob).await?;
    if blob.len() > MAX_V1_BYTES {
        return Err(UencError::V1TooLarge(MAX_V1_BYTES));
    }
    Ok(blob)
}

/// Déchiffre un blob v1 complet.
pub fn decrypt(key: &UencKey, blob: &[u8]) -> Result<Vec<u8>, UencError> {
    if blob.len() < HEADER_LEN + IV_LEN + TAG_LEN {
        return Err(UencError::Truncated);
    }
    if &blob[..4] != MAGIC {
        return Err(UencError::NotUenc);
    }
    if blob[4] != 1 {
        return Err(UencError::UnsupportedVersion(blob[4]));
    }
    if blob[5] != 1 {
        return Err(UencError::UnsupportedCipher(blob[5]));
    }
    let mut iv = [0u8; IV_LEN];
    iv.copy_from_slice(&blob[HEADER_LEN..HEADER_LEN + IV_LEN]);
    let mut buf = blob[HEADER_LEN + IV_LEN..].to_vec();
    Aead::new(Cipher::Aes256Gcm, key)?.open(&iv, &[], &mut buf)?;
    Ok(buf)
}

/// Écrit un blob v1. Réservé à l'interopérabilité (tests, export vers un client legacy) :
/// les nouveaux blobs sont toujours écrits en v2.
pub fn encrypt(key: &UencKey, plain: &[u8]) -> Result<Vec<u8>, UencError> {
    let mut iv = [0u8; IV_LEN];
    getrandom::fill(&mut iv).map_err(|_| UencError::Rng)?;
    let mut buf = plain.to_vec();
    Aead::new(Cipher::Aes256Gcm, key)?.seal(&iv, &[], &mut buf)?;
    let mut out = Vec::with_capacity(HEADER_LEN + IV_LEN + buf.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[1, 1]);
    out.extend_from_slice(&iv);
    out.extend_from_slice(&buf);
    Ok(out)
}
