//! UENC v2 : flux chunké (STREAM) avec accès aléatoire.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use super::{Aead, Cipher, MAGIC, TAG_LEN, UencError, read_full};
use crate::UencKey;

/// Taille de chunk en écriture : 2^16 = 64 Kio.
pub const CHUNK_LOG2: u8 = 16;
/// Taille de l'en-tête v2 : magic(4) + version(1) + enc_type(1) + chunk_log2(1) + préfixe de nonce(7).
pub const HEADER_LEN: usize = 14;
const PREFIX_LEN: usize = 7;
const MIN_CHUNK_LOG2: u8 = 12;
const MAX_CHUNK_LOG2: u8 = 24;

struct Header {
    raw: [u8; HEADER_LEN],
    cipher: Cipher,
    chunk_log2: u8,
    prefix: [u8; PREFIX_LEN],
}

impl Header {
    fn chunk_size(&self) -> usize {
        1usize << self.chunk_log2
    }

    /// Reconstitue l'en-tête à partir des 6 premiers octets déjà lus et des 8 suivants.
    fn parse(head: &[u8; 6], rest: &[u8; HEADER_LEN - 6]) -> Result<Self, UencError> {
        let cipher = Cipher::from_code(head[5])?;
        let chunk_log2 = rest[0];
        if !(MIN_CHUNK_LOG2..=MAX_CHUNK_LOG2).contains(&chunk_log2) {
            return Err(UencError::BadChunkSize(chunk_log2));
        }
        let mut raw = [0u8; HEADER_LEN];
        raw[..6].copy_from_slice(head);
        raw[6..].copy_from_slice(rest);
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&rest[1..]);
        Ok(Self { raw, cipher, chunk_log2, prefix })
    }

    fn nonce(&self, index: u32, last: bool) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[..PREFIX_LEN].copy_from_slice(&self.prefix);
        n[PREFIX_LEN..PREFIX_LEN + 4].copy_from_slice(&index.to_be_bytes());
        n[11] = u8::from(last);
        n
    }
}

pub(crate) async fn encrypt_stream<R, W>(
    key: &UencKey,
    cipher: Cipher,
    reader: &mut R,
    writer: &mut W,
) -> Result<u64, UencError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut prefix = [0u8; PREFIX_LEN];
    getrandom::fill(&mut prefix).map_err(|_| UencError::Rng)?;
    let mut raw = [0u8; HEADER_LEN];
    raw[..4].copy_from_slice(MAGIC);
    raw[4] = 2;
    raw[5] = cipher.code();
    raw[6] = CHUNK_LOG2;
    raw[7..].copy_from_slice(&prefix);
    let header = Header { raw, cipher, chunk_log2: CHUNK_LOG2, prefix };
    let aead = Aead::new(cipher, key)?;

    writer.write_all(&header.raw).await?;
    let mut written = HEADER_LEN as u64;

    let cs = header.chunk_size();
    let mut cur = Vec::with_capacity(cs + TAG_LEN);
    let mut next = Vec::with_capacity(cs + TAG_LEN);
    read_full(reader, &mut cur, cs).await?;
    let mut index: u32 = 0;
    loop {
        // Lecture en avance d'un chunk : seul le dernier porte le drapeau « last ».
        read_full(reader, &mut next, cs).await?;
        let last = next.is_empty();
        aead.seal(&header.nonce(index, last), &header.raw, &mut cur)?;
        writer.write_all(&cur).await?;
        written += cur.len() as u64;
        if last {
            break;
        }
        index = index.checked_add(1).ok_or(UencError::TooManyChunks)?;
        std::mem::swap(&mut cur, &mut next);
    }
    writer.flush().await?;
    Ok(written)
}

async fn read_header_rest<R: AsyncRead + Unpin>(reader: &mut R, head: &[u8; 6]) -> Result<Header, UencError> {
    let mut rest = [0u8; HEADER_LEN - 6];
    reader.read_exact(&mut rest).await.map_err(eof_is_truncated)?;
    Header::parse(head, &rest)
}

fn eof_is_truncated(e: std::io::Error) -> UencError {
    if e.kind() == std::io::ErrorKind::UnexpectedEof { UencError::Truncated } else { e.into() }
}

pub(crate) async fn decrypt_after_head<R, W>(
    key: &UencKey,
    head: &[u8; 6],
    reader: &mut R,
    writer: &mut W,
) -> Result<u64, UencError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let header = read_header_rest(reader, head).await?;
    let aead = Aead::new(header.cipher, key)?;
    let ct_chunk = header.chunk_size() + TAG_LEN;
    let mut cur = Vec::with_capacity(ct_chunk);
    let mut next = Vec::with_capacity(ct_chunk);
    read_full(reader, &mut cur, ct_chunk).await?;
    let mut index: u32 = 0;
    let mut total = 0u64;
    loop {
        if cur.len() < TAG_LEN {
            return Err(UencError::Truncated);
        }
        read_full(reader, &mut next, ct_chunk).await?;
        let last = next.is_empty();
        aead.open(&header.nonce(index, last), &header.raw, &mut cur)?;
        writer.write_all(&cur).await?;
        total += cur.len() as u64;
        if last {
            break;
        }
        index = index.checked_add(1).ok_or(UencError::TooManyChunks)?;
        std::mem::swap(&mut cur, &mut next);
    }
    writer.flush().await?;
    Ok(total)
}

/// Géométrie d'un blob v2 déduite de sa seule taille.
struct Layout {
    chunk_count: u64,
    ct_chunk: u64,
    last_ct: u64,
    plain_len: u64,
}

fn layout(total_len: u64, chunk_log2: u8) -> Result<Layout, UencError> {
    let body = total_len.checked_sub(HEADER_LEN as u64).ok_or(UencError::Truncated)?;
    let ct_chunk = (1u64 << chunk_log2) + TAG_LEN as u64;
    let chunk_count = body.div_ceil(ct_chunk);
    if chunk_count == 0 || chunk_count > u64::from(u32::MAX) + 1 {
        return Err(UencError::Truncated);
    }
    let last_ct = body - (chunk_count - 1) * ct_chunk;
    if last_ct < TAG_LEN as u64 {
        return Err(UencError::Truncated);
    }
    Ok(Layout { chunk_count, ct_chunk, last_ct, plain_len: body - chunk_count * TAG_LEN as u64 })
}

/// Taille du clair d'un blob v2 à partir de la taille du blob chiffré (pour `Content-Length`).
pub fn v2_plaintext_len(total_len: u64, chunk_log2: u8) -> Result<u64, UencError> {
    layout(total_len, chunk_log2).map(|l| l.plain_len)
}

pub(crate) async fn decrypt_range_after_head<R, W>(
    key: &UencKey,
    head: &[u8; 6],
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
    let header = read_header_rest(reader, head).await?;
    let lay = layout(total_len, header.chunk_log2)?;
    if start > end_inclusive || start >= lay.plain_len {
        return Err(UencError::BadRange);
    }
    let end = end_inclusive.min(lay.plain_len - 1);
    let cs = 1u64 << header.chunk_log2;
    let (first, last) = (start / cs, end / cs);
    let aead = Aead::new(header.cipher, key)?;

    reader.seek(std::io::SeekFrom::Start(HEADER_LEN as u64 + first * lay.ct_chunk)).await?;
    let mut buf = Vec::with_capacity(lay.ct_chunk as usize);
    let mut written = 0u64;
    for i in first..=last {
        let is_last = i == lay.chunk_count - 1;
        let want = if is_last { lay.last_ct } else { lay.ct_chunk } as usize;
        buf.clear();
        buf.resize(want, 0);
        reader.read_exact(&mut buf).await.map_err(eof_is_truncated)?;
        let index = u32::try_from(i).map_err(|_| UencError::TooManyChunks)?;
        aead.open(&header.nonce(index, is_last), &header.raw, &mut buf)?;
        let lo = if i == first { (start - first * cs) as usize } else { 0 };
        let hi = if i == last { (end - last * cs) as usize + 1 } else { buf.len() };
        let slice = buf.get(lo..hi).ok_or(UencError::BadRange)?;
        writer.write_all(slice).await?;
        written += slice.len() as u64;
    }
    writer.flush().await?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::uenc::{decrypt_range, decrypt_to_vec, encrypt_to_vec};

    const CIPHERS: [Cipher; 2] = [Cipher::Aes256Gcm, Cipher::ChaCha20Poly1305];

    fn key() -> UencKey {
        UencKey::from_bytes([0x42; 32])
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i.wrapping_mul(31) ^ (i >> 8)) as u8).collect()
    }

    #[tokio::test]
    async fn roundtrip_all_boundary_sizes() {
        let cs = 1usize << CHUNK_LOG2;
        for cipher in CIPHERS {
            for n in [0, 1, 15, cs - 1, cs, cs + 1, 2 * cs, 3 * cs + 17] {
                let plain = data(n);
                let blob = encrypt_to_vec(&key(), cipher, &plain).await.unwrap();
                assert_eq!(decrypt_to_vec(&key(), &blob).await.unwrap(), plain, "{cipher:?} n={n}");
                assert_eq!(v2_plaintext_len(blob.len() as u64, CHUNK_LOG2).unwrap(), n as u64);
            }
        }
    }

    #[tokio::test]
    async fn wrong_key_fails() {
        let blob = encrypt_to_vec(&key(), Cipher::Aes256Gcm, b"secret").await.unwrap();
        let other = UencKey::from_bytes([1; 32]);
        assert!(matches!(decrypt_to_vec(&other, &blob).await, Err(UencError::AuthFailed)));
    }

    #[tokio::test]
    async fn truncation_at_chunk_boundary_is_detected() {
        let cs = 1usize << CHUNK_LOG2;
        let blob = encrypt_to_vec(&key(), Cipher::Aes256Gcm, &data(3 * cs)).await.unwrap();
        let cut = &blob[..HEADER_LEN + 2 * (cs + TAG_LEN)];
        assert!(matches!(decrypt_to_vec(&key(), cut).await, Err(UencError::AuthFailed)));
    }

    #[tokio::test]
    async fn header_only_is_truncated() {
        let blob = encrypt_to_vec(&key(), Cipher::Aes256Gcm, b"x").await.unwrap();
        assert!(matches!(decrypt_to_vec(&key(), &blob[..HEADER_LEN]).await, Err(UencError::Truncated)));
    }

    #[tokio::test]
    async fn swapped_chunks_are_detected() {
        let cs = 1usize << CHUNK_LOG2;
        let mut blob = encrypt_to_vec(&key(), Cipher::ChaCha20Poly1305, &data(3 * cs)).await.unwrap();
        let ct = cs + TAG_LEN;
        let (a, b) = (HEADER_LEN, HEADER_LEN + ct);
        for i in 0..ct {
            blob.swap(a + i, b + i);
        }
        assert!(matches!(decrypt_to_vec(&key(), &blob).await, Err(UencError::AuthFailed)));
    }

    #[tokio::test]
    async fn header_tampering_is_detected() {
        let mut blob = encrypt_to_vec(&key(), Cipher::Aes256Gcm, b"hello").await.unwrap();
        blob[10] ^= 1; // préfixe de nonce
        assert!(matches!(decrypt_to_vec(&key(), &blob).await, Err(UencError::AuthFailed)));
    }

    #[tokio::test]
    async fn absurd_chunk_size_is_rejected() {
        let mut blob = encrypt_to_vec(&key(), Cipher::Aes256Gcm, b"hello").await.unwrap();
        blob[6] = 40;
        assert!(matches!(decrypt_to_vec(&key(), &blob).await, Err(UencError::BadChunkSize(40))));
    }

    #[tokio::test]
    async fn range_reads_match_plaintext() {
        let cs = 1u64 << CHUNK_LOG2;
        let plain = data(3 * cs as usize + 100);
        for cipher in CIPHERS {
            let blob = encrypt_to_vec(&key(), cipher, &plain).await.unwrap();
            let total = blob.len() as u64;
            let n = plain.len() as u64;
            for (s, e) in [(0, 0), (5, 10), (cs - 1, cs), (cs, 2 * cs - 1), (cs - 3, 2 * cs + 3), (n - 1, n - 1), (10, n + 999)] {
                let mut out = Vec::new();
                let mut rd = Cursor::new(blob.clone());
                let got = decrypt_range(&key(), &mut rd, total, s, e, &mut out).await.unwrap();
                let hi = e.min(n - 1);
                assert_eq!(out, plain[s as usize..=hi as usize], "{cipher:?} {s}-{e}");
                assert_eq!(got, out.len() as u64);
            }
        }
    }

    #[tokio::test]
    async fn range_out_of_bounds_is_rejected() {
        let blob = encrypt_to_vec(&key(), Cipher::Aes256Gcm, &data(100)).await.unwrap();
        let total = blob.len() as u64;
        let mut out = Vec::new();
        let r = decrypt_range(&key(), &mut Cursor::new(blob.clone()), total, 100, 200, &mut out).await;
        assert!(matches!(r, Err(UencError::BadRange)));
        let r = decrypt_range(&key(), &mut Cursor::new(blob), total, 9, 3, &mut out).await;
        assert!(matches!(r, Err(UencError::BadRange)));
    }

    #[tokio::test]
    async fn range_on_v1_blob_works() {
        let plain = data(500);
        let blob = crate::uenc::v1::encrypt(&key(), &plain).unwrap();
        let mut out = Vec::new();
        let total = blob.len() as u64;
        decrypt_range(&key(), &mut Cursor::new(blob), total, 100, 199, &mut out).await.unwrap();
        assert_eq!(out, plain[100..200]);
    }

    #[test]
    fn detect_returns_a_cipher() {
        let _ = Cipher::detect();
    }
}
