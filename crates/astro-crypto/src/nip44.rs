//! NIP-44 v2 : chiffrement authentifié entre deux clés Nostr (ou d'une clé vers elle-même).
//!
//! `conversation_key = HKDF-extract(sel "nip44-v2", x(ECDH))`, clés de message =
//! `HKDF-expand(conversation_key, nonce[32], 76)` (ChaCha20 32 o, nonce 12 o, HMAC 32 o),
//! clair préfixé de sa longueur (u16 BE) et complété par le schéma de remplissage de la spécification.
//! Charge utile : `base64(0x02 ‖ nonce ‖ chiffré ‖ HMAC-SHA256(nonce ‖ chiffré))`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use secp256k1::ecdh::shared_secret_point;
use secp256k1::{Parity, PublicKey, SecretKey, XOnlyPublicKey};
use sha2::Sha256;
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;

const VERSION: u8 = 2;
const MIN_PLAINTEXT: usize = 1;
const MAX_PLAINTEXT: usize = 65_535;

/// Erreurs NIP-44.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Nip44Error {
    /// Clé secrète ou publique invalide.
    #[error("clé invalide")]
    BadKey,
    /// Message vide ou de plus de 65 535 octets.
    #[error("longueur de message invalide")]
    BadLength,
    /// Charge utile mal formée (base64, taille, version).
    #[error("charge utile NIP-44 invalide")]
    BadPayload,
    /// Version autre que 2.
    #[error("version NIP-44 non supportée")]
    UnsupportedVersion,
    /// MAC invalide : mauvaise clé ou message altéré.
    #[error("authentification échouée")]
    BadMac,
    /// Remplissage incohérent.
    #[error("remplissage invalide")]
    BadPadding,
    /// Générateur aléatoire indisponible.
    #[error("générateur aléatoire indisponible")]
    Rng,
}

/// Clé de conversation (32 octets), effacée à la destruction.
pub type ConversationKey = Zeroizing<[u8; 32]>;

/// Clé de conversation entre `secret` et la clé publique x-only `peer_hex` (64 hex).
pub fn conversation_key(secret: &[u8; 32], peer_hex: &str) -> Result<ConversationKey, Nip44Error> {
    let sk = SecretKey::from_slice(secret).map_err(|_| Nip44Error::BadKey)?;
    let mut raw = [0u8; 32];
    hex::decode_to_slice(peer_hex, &mut raw).map_err(|_| Nip44Error::BadKey)?;
    let xonly = XOnlyPublicKey::from_slice(&raw).map_err(|_| Nip44Error::BadKey)?;
    let pk = PublicKey::from_x_only_public_key(xonly, Parity::Even);
    let point = Zeroizing::new(shared_secret_point(&pk, &sk));
    let (prk, _) = Hkdf::<Sha256>::extract(Some(b"nip44-v2"), &point[..32]);
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&prk);
    Ok(key)
}

struct MessageKeys {
    chacha_key: Zeroizing<[u8; 32]>,
    chacha_nonce: [u8; 12],
    hmac_key: Zeroizing<[u8; 32]>,
}

fn message_keys(ck: &ConversationKey, nonce: &[u8; 32]) -> Result<MessageKeys, Nip44Error> {
    let hk = Hkdf::<Sha256>::from_prk(&ck[..]).map_err(|_| Nip44Error::BadKey)?;
    let mut okm = Zeroizing::new([0u8; 76]);
    hk.expand(nonce, &mut *okm).map_err(|_| Nip44Error::BadKey)?;
    let mut keys = MessageKeys { chacha_key: Zeroizing::new([0; 32]), chacha_nonce: [0; 12], hmac_key: Zeroizing::new([0; 32]) };
    keys.chacha_key.copy_from_slice(&okm[..32]);
    keys.chacha_nonce.copy_from_slice(&okm[32..44]);
    keys.hmac_key.copy_from_slice(&okm[44..]);
    Ok(keys)
}

/// Longueur après remplissage (hors préfixe de 2 octets).
pub fn padded_len(len: usize) -> usize {
    if len <= 32 {
        return 32;
    }
    let next_power = 1usize << (usize::BITS - (len - 1).leading_zeros());
    let chunk = if next_power <= 256 { 32 } else { next_power / 8 };
    chunk * ((len - 1) / chunk + 1)
}

fn mac(key: &[u8; 32], nonce: &[u8; 32], ciphertext: &[u8]) -> Result<HmacSha256, Nip44Error> {
    let mut m = HmacSha256::new_from_slice(key).map_err(|_| Nip44Error::BadKey)?;
    m.update(nonce);
    m.update(ciphertext);
    Ok(m)
}

/// Chiffre `plaintext` avec un nonce aléatoire.
pub fn encrypt(ck: &ConversationKey, plaintext: &str) -> Result<String, Nip44Error> {
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).map_err(|_| Nip44Error::Rng)?;
    encrypt_with_nonce(ck, plaintext, &nonce)
}

/// Variante à nonce imposé (vecteurs de test uniquement : ne jamais réutiliser un nonce).
pub fn encrypt_with_nonce(ck: &ConversationKey, plaintext: &str, nonce: &[u8; 32]) -> Result<String, Nip44Error> {
    let data = plaintext.as_bytes();
    if !(MIN_PLAINTEXT..=MAX_PLAINTEXT).contains(&data.len()) {
        return Err(Nip44Error::BadLength);
    }
    let keys = message_keys(ck, nonce)?;
    let mut padded = Zeroizing::new(vec![0u8; 2 + padded_len(data.len())]);
    padded[..2].copy_from_slice(&(data.len() as u16).to_be_bytes());
    padded[2..2 + data.len()].copy_from_slice(data);
    let mut cipher = ChaCha20::new((&*keys.chacha_key).into(), (&keys.chacha_nonce).into());
    cipher.apply_keystream(&mut padded);
    let tag = mac(&keys.hmac_key, nonce, &padded)?.finalize().into_bytes();
    let mut payload = Vec::with_capacity(1 + 32 + padded.len() + 32);
    payload.push(VERSION);
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(&padded);
    payload.extend_from_slice(&tag);
    Ok(STANDARD.encode(payload))
}

/// Déchiffre une charge utile NIP-44 v2.
pub fn decrypt(ck: &ConversationKey, payload: &str) -> Result<String, Nip44Error> {
    if payload.starts_with('#') || payload.len() < 132 || payload.len() > 87_472 {
        return Err(Nip44Error::BadPayload);
    }
    let raw = STANDARD.decode(payload).map_err(|_| Nip44Error::BadPayload)?;
    if raw.len() < 99 || raw.len() > 65_603 {
        return Err(Nip44Error::BadPayload);
    }
    if raw[0] != VERSION {
        return Err(Nip44Error::UnsupportedVersion);
    }
    let mut nonce = [0u8; 32];
    nonce.copy_from_slice(&raw[1..33]);
    let (ciphertext, tag) = raw[33..].split_at(raw.len() - 33 - 32);
    let keys = message_keys(ck, &nonce)?;
    mac(&keys.hmac_key, &nonce, ciphertext)?.verify_slice(tag).map_err(|_| Nip44Error::BadMac)?;
    let mut padded = Zeroizing::new(ciphertext.to_vec());
    let mut cipher = ChaCha20::new((&*keys.chacha_key).into(), (&keys.chacha_nonce).into());
    cipher.apply_keystream(&mut padded);
    let len = usize::from(u16::from_be_bytes([padded[0], padded[1]]));
    if len < MIN_PLAINTEXT || padded.len() != 2 + padded_len(len) || 2 + len > padded.len() {
        return Err(Nip44Error::BadPadding);
    }
    String::from_utf8(padded[2..2 + len].to_vec()).map_err(|_| Nip44Error::BadPadding)
}

/// Chiffre vers soi-même (« self-DM ») : la clé publique de `secret` sert de destinataire.
pub fn encrypt_to_self(secret: &[u8; 32], own_pubkey_hex: &str, plaintext: &str) -> Result<String, Nip44Error> {
    encrypt(&conversation_key(secret, own_pubkey_hex)?, plaintext)
}

/// Déchiffre un message que `secret` s'est adressé.
pub fn decrypt_from_self(secret: &[u8; 32], own_pubkey_hex: &str, payload: &str) -> Result<String, Nip44Error> {
    decrypt(&conversation_key(secret, own_pubkey_hex)?, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pubkey(secret: &[u8; 32]) -> String {
        astro_nostr_proto::pubkey_of(secret).unwrap()
    }

    #[test]
    fn padding_schedule_matches_the_spec() {
        for (len, expected) in [(1, 32), (32, 32), (33, 64), (64, 64), (65, 96), (100, 128), (256, 256), (257, 320), (320, 320), (383, 384), (384, 384), (385, 448), (500, 512), (512, 512), (515, 640), (700, 768), (800, 896), (900, 1024), (1020, 1024), (65_535, 65_536)] {
            assert_eq!(padded_len(len), expected, "len {len}");
        }
    }

    #[test]
    fn conversation_key_is_symmetric() {
        let (a, b) = ([0x11u8; 32], [0x22u8; 32]);
        let ab = conversation_key(&a, &pubkey(&b)).unwrap();
        let ba = conversation_key(&b, &pubkey(&a)).unwrap();
        assert_eq!(*ab, *ba);
    }

    #[test]
    fn round_trips_including_unicode_and_boundaries() {
        let (a, b) = ([0x11u8; 32], [0x22u8; 32]);
        let ck = conversation_key(&a, &pubkey(&b)).unwrap();
        for msg in ["a", "Bonjour uCloud ✓", &"x".repeat(32), &"é".repeat(300), &"z".repeat(65_535)] {
            let ct = encrypt(&ck, msg).unwrap();
            assert_eq!(decrypt(&conversation_key(&b, &pubkey(&a)).unwrap(), &ct).unwrap(), msg);
        }
        assert_eq!(encrypt(&ck, ""), Err(Nip44Error::BadLength));
        assert_eq!(encrypt(&ck, &"x".repeat(65_536)), Err(Nip44Error::BadLength));
    }

    #[test]
    fn self_encryption_round_trips_and_nonces_differ() {
        let s = [0x33u8; 32];
        let me = pubkey(&s);
        let c1 = encrypt_to_self(&s, &me, "{\"v\":1}").unwrap();
        let c2 = encrypt_to_self(&s, &me, "{\"v\":1}").unwrap();
        assert_ne!(c1, c2);
        assert_eq!(decrypt_from_self(&s, &me, &c1).unwrap(), "{\"v\":1}");
        assert_eq!(decrypt_from_self(&[0x34; 32], &me, &c1), Err(Nip44Error::BadMac));
    }

    #[test]
    fn tampering_and_malformed_payloads_are_rejected() {
        let s = [0x33u8; 32];
        let me = pubkey(&s);
        let ct = encrypt_to_self(&s, &me, "secret").unwrap();
        let mut raw = STANDARD.decode(&ct).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 1;
        assert_eq!(decrypt_from_self(&s, &me, &STANDARD.encode(&raw)), Err(Nip44Error::BadMac));
        let mut v1 = STANDARD.decode(&ct).unwrap();
        v1[0] = 1;
        assert_eq!(decrypt_from_self(&s, &me, &STANDARD.encode(&v1)), Err(Nip44Error::UnsupportedVersion));
        assert_eq!(decrypt_from_self(&s, &me, "#abc"), Err(Nip44Error::BadPayload));
        assert_eq!(decrypt_from_self(&s, &me, "AAAA"), Err(Nip44Error::BadPayload));
        assert_eq!(conversation_key(&[0; 32], &me), Err(Nip44Error::BadKey));
        assert_eq!(conversation_key(&s, "zz"), Err(Nip44Error::BadKey));
    }
}
