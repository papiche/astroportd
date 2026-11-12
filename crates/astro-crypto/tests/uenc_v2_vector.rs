//! Compatibilité avec un blob produit par le codec partagé `Astroport.ONE/tools/uenc_codec.py` (UENC v2, Python).
//!
//! Vecteur à préfixe de nonce fixé (`os.urandom` remplacé par `0xa0, 0xa1…`), clé `000102…1f` :
//! `encrypt_aes256gcm(b'UPlanet vecteur UENC v2', key)`.

#![allow(clippy::unwrap_used)]

use astro_crypto::uenc;
use astro_crypto::{Cipher, UencError, UencKey};

const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const PLAIN: &[u8] = b"UPlanet vecteur UENC v2";
const BLOB_HEX: &str = "55454e43020110a0a1a2a3a4a5a62231257fd66394e07500bb5baafbc978524e65aae2944eb803a99f6b9c784273fc29adc0d04515";

fn blob() -> Vec<u8> {
    hex::decode(BLOB_HEX).unwrap()
}

#[test]
fn python_blob_has_expected_layout() {
    let b = blob();
    assert_eq!(&b[..7], &[0x55, 0x45, 0x4e, 0x43, 0x02, 0x01, 16]);
    assert_eq!(b.len(), 14 + PLAIN.len() + 16);
}

#[tokio::test]
async fn decrypts_python_blob() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    assert_eq!(uenc::decrypt_to_vec(&key, &blob()).await.unwrap(), PLAIN);
}

#[tokio::test]
async fn rust_output_has_the_same_layout_as_python() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    let b = uenc::encrypt_to_vec(&key, Cipher::Aes256Gcm, PLAIN).await.unwrap();
    assert_eq!(b.len(), blob().len());
    assert_eq!(&b[..7], &blob()[..7]);
    assert_eq!(uenc::decrypt_to_vec(&key, &b).await.unwrap(), PLAIN);
}

#[tokio::test]
async fn tampered_python_blob_is_rejected() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    let mut b = blob();
    let last = b.len() - 1;
    b[last] ^= 0x01;
    assert!(matches!(uenc::decrypt_to_vec(&key, &b).await, Err(UencError::AuthFailed)));
}

#[tokio::test]
async fn garbage_and_the_retired_v1_are_not_readable() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    assert!(matches!(uenc::decrypt_to_vec(&key, b"hello").await, Err(UencError::NotUenc)));
    assert!(matches!(uenc::decrypt_to_vec(&key, b"UENC\x09\x01").await, Err(UencError::UnsupportedVersion(9))));
    assert!(matches!(uenc::decrypt_to_vec(&key, b"UENC\x01\x01").await, Err(UencError::UnsupportedVersion(1))), "la v1 n'est plus lue");
}
