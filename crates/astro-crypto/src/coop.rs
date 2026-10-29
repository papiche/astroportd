//! Chiffrement des secrets de la **configuration coopérative** (kind 30800 `cooperative-config`), compatible avec
//! `Astroport.ONE/tools/cooperative_config.sh` : AES-256-CBC (bourrage PKCS#7), clé = SHA-256 de `UPLANETNAME` (32 octets
//! bruts), IV aléatoire de 16 octets ; valeur = `<IV en hexadécimal>:<chiffré en base64>`.
//!
//! La clé est le secret partagé de la constellation : quiconque connaît `UPLANETNAME` lit ces valeurs, et c'est voulu
//! (toutes les stations de la constellation doivent pouvoir les utiliser). Ce n'est donc pas une protection contre un
//! membre de la constellation, seulement contre les relais et les lecteurs extérieurs.

use aes::Aes256;
use base64::Engine;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use sha2::{Digest, Sha256};

/// Erreurs de déchiffrement.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CoopCryptError {
    /// Pas au format `iv:chiffré`.
    #[error("valeur chiffrée mal formée (iv:base64 attendu)")]
    Format,
    /// Mauvais `UPLANETNAME` ou données corrompues.
    #[error("déchiffrement impossible : UPLANETNAME différent ou valeur corrompue")]
    Decrypt,
}

fn key(uplanet_name: &str) -> [u8; 32] {
    Sha256::digest(uplanet_name.as_bytes()).into()
}

/// La valeur a-t-elle la forme d'un secret chiffré (`32 hexadécimaux`, `:`, base64) ?
pub fn is_encrypted(value: &str) -> bool {
    let v: String = value.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
    let Some((iv, data)) = v.split_once(':') else { return false };
    iv.len() == 32 && iv.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) && !data.is_empty() && data.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Chiffre `plaintext` avec l'IV fourni (déterministe : tests, vecteurs). En production, voir [`encrypt`].
pub fn encrypt_with_iv(uplanet_name: &str, plaintext: &str, iv: [u8; 16]) -> String {
    let ct = cbc::Encryptor::<Aes256>::new(&key(uplanet_name).into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes());
    format!("{}:{}", hex::encode(iv), base64::engine::general_purpose::STANDARD.encode(ct))
}

/// Chiffre `plaintext` avec un IV aléatoire neuf.
pub fn encrypt(uplanet_name: &str, plaintext: &str) -> Result<String, getrandom::Error> {
    let mut iv = [0u8; 16];
    getrandom::fill(&mut iv)?;
    Ok(encrypt_with_iv(uplanet_name, plaintext, iv))
}

/// Déchiffre `iv:base64`.
pub fn decrypt(uplanet_name: &str, value: &str) -> Result<String, CoopCryptError> {
    let v: String = value.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
    let (iv_hex, data) = v.split_once(':').ok_or(CoopCryptError::Format)?;
    let iv: [u8; 16] = hex::decode(iv_hex).map_err(|_| CoopCryptError::Format)?.try_into().map_err(|_| CoopCryptError::Format)?;
    let ct = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| CoopCryptError::Format)?;
    let plain = cbc::Decryptor::<Aes256>::new(&key(uplanet_name).into(), &iv.into()).decrypt_padded_vec_mut::<Pkcs7>(&ct).map_err(|_| CoopCryptError::Decrypt)?;
    String::from_utf8(plain).map_err(|_| CoopCryptError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IV: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    /// Vecteurs produits par `openssl enc -aes-256-cbc -pbkdf2 -a -A -K <sha256(UPLANETNAME)> -iv 000102…0f`
    /// (la commande de `cooperative_config.sh::coop_encrypt`).
    #[test]
    fn values_are_byte_identical_to_what_the_legacy_shell_script_produces() {
        assert_eq!(encrypt_with_iv("UPlanetTestName", "mon-secret-ocapikey", IV), "000102030405060708090a0b0c0d0e0f:a6Z5mnHrgxk3mKSxMbaOS1AYdF9UZuUQGjeKKZUKMAI=");
        assert_eq!(encrypt_with_iv("UPlanetTestName", "", IV), "000102030405060708090a0b0c0d0e0f:/mYqRiNMq7L2+6Jv28yPzA==");
        assert_eq!(encrypt_with_iv("UPlanetTestName", "été € ça", IV), "000102030405060708090a0b0c0d0e0f:19zUWXWziOfptoJ016JSVw==");
        assert_eq!(decrypt("UPlanetTestName", "000102030405060708090a0b0c0d0e0f:a6Z5mnHrgxk3mKSxMbaOS1AYdF9UZuUQGjeKKZUKMAI=").unwrap(), "mon-secret-ocapikey");
    }

    #[test]
    fn a_round_trip_needs_the_same_uplanet_and_malformed_values_are_refused() {
        let secret = encrypt("UPlanetTestName", "clé d'API 🔑").unwrap();
        assert!(is_encrypted(&secret));
        assert_eq!(decrypt("UPlanetTestName", &secret).unwrap(), "clé d'API 🔑");
        assert_ne!(encrypt("UPlanetTestName", "x").unwrap(), encrypt("UPlanetTestName", "x").unwrap(), "IV neuf à chaque fois");
        assert_eq!(decrypt("Autre", &secret), Err(CoopCryptError::Decrypt));
        for bad in ["", "pas-chiffre", "zz:abc", "000102030405060708090a0b0c0d0e0f:", "000102030405060708090a0b0c0d0e0f:@@@"] {
            assert!(decrypt("UPlanetTestName", bad).is_err(), "{bad:?}");
        }
        assert!(!is_encrypted("20.0") && !is_encrypted("https://opencollective.com/x") && !is_encrypted(""));
        // Le script legacy relit une valeur repliée sur plusieurs lignes (ancien format) : on l'accepte aussi.
        let split = secret.replacen(':', ":\n", 1);
        assert_eq!(decrypt("UPlanetTestName", &split).unwrap(), "clé d'API 🔑");
    }
}
