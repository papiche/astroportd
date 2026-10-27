//! Signature ed25519 en mémoire pour le runtime Ğ1 (`MultiSignature::Ed25519`).

use std::fmt;

use astro_core::kdf::Seed;
use ed25519_dalek::{Signer as _, SigningKey};
use subxt::config::PolkadotConfig;
use subxt::tx::Signer;
use subxt::utils::{AccountId32, MultiSignature};
use zeroize::Zeroizing;

use crate::Account;

/// Clé de signature d'un compte Ğ1. La graine est effacée de la mémoire à la destruction ;
/// la clé de travail n'existe que le temps d'une signature.
pub struct Ed25519Signer {
    seed: Zeroizing<[u8; 32]>,
    account: Account,
}

impl Ed25519Signer {
    /// À partir de la graine ed25519 de 32 octets.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let seed = Zeroizing::new(seed);
        let account = Account(SigningKey::from_bytes(&seed).verifying_key().to_bytes());
        Self { seed, account }
    }

    /// À partir de `scrypt(salt, pepper)` (la chaîne de `keygen`).
    pub fn from_kdf_seed(seed: &Seed) -> Self {
        Self::from_seed(*seed.ed25519_secret_seed())
    }

    /// Compte correspondant.
    pub fn account(&self) -> Account {
        self.account
    }

    /// Signature ed25519 brute.
    pub fn sign_bytes(&self, message: &[u8]) -> [u8; 64] {
        SigningKey::from_bytes(&self.seed).sign(message).to_bytes()
    }
}

impl fmt::Debug for Ed25519Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ed25519Signer({})", self.account)
    }
}

impl Signer<PolkadotConfig> for Ed25519Signer {
    fn account_id(&self) -> AccountId32 {
        self.account.account_id()
    }

    fn sign(&self, payload: &[u8]) -> MultiSignature {
        MultiSignature::Ed25519(self.sign_bytes(payload))
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    use super::*;

    #[test]
    fn account_matches_keygen_and_signatures_verify() {
        let seed = astro_core::kdf::duniter_seed("salt-test", "pepper-test").unwrap();
        let signer = Ed25519Signer::from_kdf_seed(&seed);
        assert_eq!(signer.account().v1(), "6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN", "même clé que tools/keygen -t duniter");
        assert_eq!(signer.account().ss58(), "g1LsRGbH5st3WZfdF2SpknxM12W1THsmU5iFzwgKWLHDQqZDq");
        let sig = signer.sign_bytes(b"message");
        VerifyingKey::from_bytes(&signer.account().0).unwrap().verify(b"message", &Signature::from_bytes(&sig)).unwrap();
        assert!(matches!(<Ed25519Signer as Signer<PolkadotConfig>>::sign(&signer, b"x"), MultiSignature::Ed25519(_)));
        assert!(!format!("{signer:?}").contains(&hex::encode(*seed.ed25519_secret_seed())), "la graine n'apparaît jamais dans Debug");
    }
}
