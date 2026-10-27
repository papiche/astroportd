//! Compatibilité avec un blob produit par le codec legacy `tools/uenc_codec.py`.
//!
//! Vecteur généré le 2026-10-26 :
//! `printf 'UPlanet vecteur UENC v1' | python3 Astroport.ONE/tools/uenc_codec.py encrypt 000102…1f`

#![allow(clippy::unwrap_used)]

use astro_crypto::uenc::{self, v1};
use astro_crypto::{Cipher, UencError, UencKey};

const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const PLAIN: &[u8] = b"UPlanet vecteur UENC v1";
const BLOB_HEX: &str = "55454e43010186115a196ee46e3e7a8d1e5859739c794610660e51bad2fde1fc73b1dcaa309fe5bd1091b2b1b6d1797220c9f192376d233394";

fn blob() -> Vec<u8> {
    hex::decode(BLOB_HEX).unwrap()
}

#[test]
fn python_blob_has_expected_layout() {
    let b = blob();
    assert_eq!(&b[..6], &[0x55, 0x45, 0x4e, 0x43, 0x01, 0x01]);
    assert_eq!(b.len(), 6 + 12 + PLAIN.len() + 16);
}

#[test]
fn decrypts_python_blob_directly() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    assert_eq!(v1::decrypt(&key, &blob()).unwrap(), PLAIN);
}

#[tokio::test]
async fn decrypts_python_blob_through_runtime_detection() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    assert_eq!(uenc::decrypt_to_vec(&key, &blob()).await.unwrap(), PLAIN);
}

#[tokio::test]
async fn rust_v1_output_is_readable_by_the_same_layout() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    let b = v1::encrypt(&key, PLAIN).unwrap();
    assert_eq!(b.len(), blob().len());
    assert_eq!(&b[..6], &blob()[..6]);
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
async fn new_writes_are_v2_and_distinct_from_v1() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    let b = uenc::encrypt_to_vec(&key, Cipher::detect(), PLAIN).await.unwrap();
    assert_eq!(b[4], 2);
}

#[tokio::test]
async fn garbage_is_not_uenc() {
    let key = UencKey::from_hex(KEY_HEX).unwrap();
    assert!(matches!(uenc::decrypt_to_vec(&key, b"hello").await, Err(UencError::NotUenc)));
    assert!(matches!(uenc::decrypt_to_vec(&key, b"UENC\x09\x01").await, Err(UencError::UnsupportedVersion(9))));
}
