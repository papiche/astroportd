//! Interopérabilité NIP-44 v2 avec `nostr_sdk` (rust-nostr, utilisé par `atom4love_publish.py`).

#![allow(clippy::unwrap_used)]

use astro_crypto::nip44::{decrypt_from_self, encrypt_to_self};

const SECRET: [u8; 32] = [0x01; 32];
const PLAINTEXT: &str = "Bonjour uCloud ✓";
/// `nip44_encrypt(SecretKey(01*32), PublicKey(01*32), "Bonjour uCloud ✓", V2)` produit par nostr_sdk le 2026-10-26.
const SDK_PAYLOAD: &str = "AtDFZSnrAowcM426GlLw4YxostO3AAKuAHXIhnekPATHb9TjF4nTkn3AOBJjSaDb/I/1EnG9dj4z5TvYKbokbimqlkPz/wE+66SKbEOnSp9sKR9cVvLJ1BDFQJePNEBkFfW4";

#[test]
fn decrypts_a_payload_produced_by_nostr_sdk() {
    let me = astro_nostr_proto::pubkey_of(&SECRET).unwrap();
    assert_eq!(me, "1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f");
    assert_eq!(decrypt_from_self(&SECRET, &me, SDK_PAYLOAD).unwrap(), PLAINTEXT);
}

/// Rust → nostr_sdk (nécessite `~/.astro/bin/python`).
#[test]
#[ignore = "exécute nostr_sdk"]
fn nostr_sdk_decrypts_our_payload() {
    let me = astro_nostr_proto::pubkey_of(&SECRET).unwrap();
    let ct = encrypt_to_self(&SECRET, &me, PLAINTEXT).unwrap();
    let script = format!(
        "from nostr_sdk import SecretKey, Keys, nip44_decrypt\nsk=SecretKey.parse('01'*32)\nprint(nip44_decrypt(sk, Keys(sk).public_key(), '{ct}'), end='')"
    );
    let python = format!("{}/.astro/bin/python", std::env::var("HOME").unwrap());
    let out = std::process::Command::new(python).args(["-c", &script]).output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), PLAINTEXT, "{}", String::from_utf8_lossy(&out.stderr));
}
