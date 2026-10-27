use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::UencError;

/// Clé symétrique de 32 octets (AES-256-GCM ou ChaCha20-Poly1305).
///
/// Effacée de la mémoire à la destruction ; `Debug` ne l'affiche jamais.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct UencKey([u8; 32]);

impl UencKey {
    /// Construit une clé à partir de 32 octets.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Construit une clé à partir de 64 caractères hexadécimaux (format du keyring legacy).
    pub fn from_hex(s: &str) -> Result<Self, UencError> {
        let mut buf = [0u8; 32];
        hex::decode_to_slice(s.trim(), &mut buf).map_err(|_| UencError::BadKey)?;
        Ok(Self(buf))
    }

    /// Tire une clé aléatoire du système.
    pub fn generate() -> Result<Self, UencError> {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).map_err(|_| UencError::Rng)?;
        Ok(Self(buf))
    }

    /// Représentation hexadécimale (64 caractères) pour le keyring.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for UencKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UencKey(**)")
    }
}
