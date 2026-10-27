//! Adresses Ğ1 : clé ed25519 de 32 octets, affichée en SS58 (préfixe 4450) ou en base58 v1 (legacy Cesium).

use std::fmt;
use std::str::FromStr;

use astro_core::kdf::{G1_SS58_PREFIX, KdfError, ss58_decode, ss58_encode};

/// Erreurs d'adresse.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AccountError {
    /// Format non reconnu.
    #[error("adresse Ğ1 invalide : {0}")]
    Invalid(String),
    /// Préfixe de réseau SS58 différent de 4450.
    #[error("préfixe SS58 {0} ≠ 4450 (autre réseau)")]
    WrongNetwork(u16),
}

/// Compte Ğ1 (clé publique ed25519).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Account(pub [u8; 32]);

impl Account {
    /// Adresse SS58 (`g1…`).
    pub fn ss58(&self) -> String {
        // Le préfixe 4450 est constant et valide : l'encodage ne peut pas échouer.
        ss58_encode(G1_SS58_PREFIX, &self.0).unwrap_or_default()
    }

    /// Clé publique en base58 (ancien format v1, celui des fichiers `.dunikey`).
    pub fn v1(&self) -> String {
        bs58::encode(self.0).into_string()
    }

    /// Identifiant subxt.
    pub fn account_id(&self) -> subxt::utils::AccountId32 {
        subxt::utils::AccountId32(self.0)
    }
}

impl From<[u8; 32]> for Account {
    fn from(k: [u8; 32]) -> Self {
        Self(k)
    }
}

impl From<&subxt::utils::AccountId32> for Account {
    fn from(a: &subxt::utils::AccountId32) -> Self {
        Self(a.0)
    }
}

impl fmt::Display for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.ss58())
    }
}

impl FromStr for Account {
    type Err = AccountError;

    /// Accepte une adresse SS58 `g1…` (préfixe 4450) ou une clé v1 en base58.
    fn from_str(s: &str) -> Result<Self, AccountError> {
        let s = s.trim();
        if s.starts_with("g1") && s.len() > 44 {
            return match ss58_decode(s) {
                Ok((G1_SS58_PREFIX, key)) => Ok(Self(key)),
                Ok((other, _)) => Err(AccountError::WrongNetwork(other)),
                Err(KdfError::BadSs58) => Err(AccountError::Invalid(s.to_owned())),
                Err(e) => Err(AccountError::Invalid(e.to_string())),
            };
        }
        let raw = bs58::decode(s).into_vec().map_err(|_| AccountError::Invalid(s.to_owned()))?;
        let key: [u8; 32] = raw.try_into().map_err(|_| AccountError::Invalid(s.to_owned()))?;
        Ok(Self(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_and_ss58_are_the_same_account() {
        // Vecteur de tools/g1pub_to_ss58.py.
        let (v1, ss58) = ("6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN", "g1LsRGbH5st3WZfdF2SpknxM12W1THsmU5iFzwgKWLHDQqZDq");
        let a: Account = v1.parse().unwrap();
        assert_eq!(a.ss58(), ss58);
        assert_eq!(a.v1(), v1);
        assert_eq!(ss58.parse::<Account>().unwrap(), a);
        assert_eq!(a.to_string(), ss58);
    }

    #[test]
    fn bad_addresses_and_other_networks_are_refused() {
        assert!("pas une adresse".parse::<Account>().is_err());
        assert!("g1LsRGbH5st3WZfdF2SpknxM12W1THsmU5iFzwgKWLHDQqZDr".parse::<Account>().is_err(), "somme de contrôle");
        assert!("abc".parse::<Account>().is_err());
        let other = ss58_encode(42, &[7; 32]).unwrap();
        assert!(other.parse::<Account>().is_err(), "adresse d'un autre réseau");
    }
}
