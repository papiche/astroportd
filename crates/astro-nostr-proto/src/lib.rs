//! Types et validateurs Nostr partagés par le relai, l'API et le client.
//!
//! Contient l'événement NIP-01, le calcul de l'identifiant, la vérification
//! de la signature BIP-340 et les constantes de kinds réellement utilisés
//! par l'écosystème UPlanet (voir `NIP-101/relay.writePolicy.plugin/filter/`).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod filter;
pub mod message;

pub use filter::{Filter, FilterError};
pub use message::{ClientMessage, MessageError, RelayMessage};

use secp256k1::{Keypair, Secp256k1, XOnlyPublicKey, schnorr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Kinds utilisés par l'écosystème.
pub mod kind {
    /// Profil.
    pub const METADATA: u32 = 0;
    /// Note courte.
    pub const TEXT_NOTE: u32 = 1;
    /// Liste de contacts (graphe N1).
    pub const CONTACTS: u32 = 3;
    /// Message direct NIP-04/NIP-44 (legacy).
    pub const DM_LEGACY: u32 = 4;
    /// Suppression.
    pub const DELETION: u32 = 5;
    /// Réaction (porte aussi le paiement `+N` ẐEN).
    pub const REACTION: u32 = 7;
    /// Seal NIP-59.
    pub const SEAL: u32 = 13;
    /// Chat NIP-17.
    pub const CHAT: u32 = 14;
    /// Fichier chiffré NIP-17.
    pub const FILE: u32 = 15;
    /// Gift wrap NIP-59.
    pub const GIFT_WRAP: u32 = 1059;
    /// Authentification NIP-42.
    pub const AUTH: u32 = 22242;
    /// Authentification HTTP NIP-98.
    pub const HTTP_AUTH: u32 = 27235;
    /// Vidéo NIP-71 (> 60 s).
    pub const VIDEO: u32 = 21;
    /// Vidéo courte NIP-71 (≤ 60 s).
    pub const VIDEO_SHORT: u32 = 22;
    /// Présence visio (éphémère).
    pub const VISIO_PRESENCE: u32 = 24315;
    /// Sonnerie visio (éphémère).
    pub const VISIO_RING: u32 = 24316;
    /// Article long (annonces UMAP/SECTOR/REGION).
    pub const LONG_FORM: u32 = 30023;
    /// Données applicatives (`d` tag par usage).
    pub const APP_DATA: u32 = 30078;
    /// DID coopératif / DID Node.
    pub const DID: u32 = 30800;
    /// Santé économique de la station.
    pub const ECONOMIC_HEALTH: u32 = 30850;
    /// Preuve d'émission ẐEN.
    pub const ZEN_ISSUANCE: u32 = 30851;
    /// Ledger Ğ1-N².
    pub const LEDGER_N2: u32 = 30852;
    /// Paramètres applicatifs utilisateur (un `d` par usage).
    pub const APP_PARAMS: u32 = 31903;
}

/// Erreurs de validation d'un événement.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EventError {
    /// Champ hexadécimal invalide.
    #[error("hex invalide pour {0}")]
    BadHex(&'static str),
    /// L'identifiant ne correspond pas au contenu.
    #[error("identifiant d'événement incorrect")]
    IdMismatch,
    /// Signature BIP-340 invalide.
    #[error("signature invalide")]
    BadSignature,
    /// Sérialisation canonique impossible.
    #[error("sérialisation impossible")]
    Serialize,
    /// Clé secrète secp256k1 invalide.
    #[error("clé secrète invalide")]
    BadSecretKey,
}

/// Événement NIP-01.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// sha256 de la sérialisation canonique (64 hex).
    pub id: String,
    /// Clé publique x-only de l'auteur (64 hex).
    pub pubkey: String,
    /// Horodatage UNIX en secondes.
    pub created_at: u64,
    /// Kind.
    pub kind: u32,
    /// Tags.
    pub tags: Vec<Vec<String>>,
    /// Contenu.
    pub content: String,
    /// Signature Schnorr (128 hex).
    pub sig: String,
}

impl Event {
    /// Identifiant canonique : `sha256([0, pubkey, created_at, kind, tags, content])`.
    pub fn compute_id(&self) -> Result<[u8; 32], EventError> {
        let canon = serde_json::to_vec(&(
            0u8,
            &self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &self.content,
        ))
        .map_err(|_| EventError::Serialize)?;
        Ok(Sha256::digest(canon).into())
    }

    /// Valeur du premier tag de nom `name`.
    pub fn tag_value(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(String::as_str) == Some(name))
            .and_then(|t| t.get(1))
            .map(String::as_str)
    }

    /// Vérifie l'identifiant puis la signature BIP-340.
    pub fn verify(&self) -> Result<(), EventError> {
        let id = self.compute_id()?;
        let claimed = hex::decode(&self.id).map_err(|_| EventError::BadHex("id"))?;
        if claimed.as_slice() != id {
            return Err(EventError::IdMismatch);
        }
        let pk_bytes = hex::decode(&self.pubkey).map_err(|_| EventError::BadHex("pubkey"))?;
        let pk = XOnlyPublicKey::from_slice(&pk_bytes).map_err(|_| EventError::BadHex("pubkey"))?;
        let sig_bytes = hex::decode(&self.sig).map_err(|_| EventError::BadHex("sig"))?;
        let sig = schnorr::Signature::from_slice(&sig_bytes).map_err(|_| EventError::BadSignature)?;
        Secp256k1::verification_only()
            .verify_schnorr(&sig, &id, &pk)
            .map_err(|_| EventError::BadSignature)
    }
}

/// Événement à signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedEvent {
    /// Horodatage UNIX en secondes.
    pub created_at: u64,
    /// Kind.
    pub kind: u32,
    /// Tags.
    pub tags: Vec<Vec<String>>,
    /// Contenu.
    pub content: String,
}

impl UnsignedEvent {
    /// Signe avec la clé secrète `secret` (BIP-340) : calcule la clé publique, l'identifiant et la signature.
    pub fn sign(self, secret: &[u8; 32]) -> Result<Event, EventError> {
        let secp = Secp256k1::new();
        let kp = Keypair::from_seckey_slice(&secp, secret).map_err(|_| EventError::BadSecretKey)?;
        let (xonly, _) = kp.x_only_public_key();
        let mut ev = Event {
            id: String::new(),
            pubkey: hex::encode(xonly.serialize()),
            created_at: self.created_at,
            kind: self.kind,
            tags: self.tags,
            content: self.content,
            sig: String::new(),
        };
        let id = ev.compute_id()?;
        ev.id = hex::encode(id);
        ev.sig = hex::encode(secp.sign_schnorr_no_aux_rand(&id, &kp).to_byte_array());
        Ok(ev)
    }
}

/// Clé publique x-only (64 hex) d'une clé secrète.
pub fn pubkey_of(secret: &[u8; 32]) -> Result<String, EventError> {
    let secp = Secp256k1::new();
    let kp = Keypair::from_seckey_slice(&secp, secret).map_err(|_| EventError::BadSecretKey)?;
    Ok(hex::encode(kp.x_only_public_key().0.serialize()))
}

/// `true` si `s` est une clé publique ou un identifiant hexadécimal de 64 caractères minuscules.
pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construit un événement signé avec une clé de test fixe.
    pub fn signed(kind: u32, tags: Vec<Vec<String>>, content: &str, created_at: u64) -> Event {
        use secp256k1::Keypair;
        let secp = Secp256k1::new();
        let kp = Keypair::from_seckey_slice(&secp, &[7u8; 32]).unwrap();
        let (xonly, _) = kp.x_only_public_key();
        let mut ev = Event {
            id: String::new(),
            pubkey: hex::encode(xonly.serialize()),
            created_at,
            kind,
            tags,
            content: content.into(),
            sig: String::new(),
        };
        let id = ev.compute_id().unwrap();
        ev.id = hex::encode(id);
        ev.sig = hex::encode(secp.sign_schnorr_no_aux_rand(&id, &kp).to_byte_array());
        ev
    }

    #[test]
    fn signed_event_verifies() {
        assert_eq!(signed(1, vec![], "bonjour", 1_800_000_000).verify(), Ok(()));
    }

    #[test]
    fn tampered_content_is_rejected() {
        let mut ev = signed(1, vec![], "bonjour", 1_800_000_000);
        ev.content = "autre".into();
        assert_eq!(ev.verify(), Err(EventError::IdMismatch));
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let mut ev = signed(1, vec![], "bonjour", 1_800_000_000);
        ev.sig = "00".repeat(64);
        assert_eq!(ev.verify(), Err(EventError::BadSignature));
    }

    #[test]
    fn unsigned_event_signs_and_verifies() {
        let ev = UnsignedEvent { created_at: 1_800_000_000, kind: 1, tags: vec![], content: "salut".into() }
            .sign(&[3u8; 32])
            .unwrap();
        assert_eq!(ev.verify(), Ok(()));
        assert_eq!(ev.pubkey, pubkey_of(&[3u8; 32]).unwrap());
        assert!(is_hex64(&ev.pubkey) && is_hex64(&ev.id));
    }

    #[test]
    fn zero_secret_is_refused() {
        let r = UnsignedEvent { created_at: 0, kind: 1, tags: vec![], content: String::new() }.sign(&[0u8; 32]);
        assert_eq!(r, Err(EventError::BadSecretKey));
    }

    #[test]
    fn hex64_is_strict() {
        assert!(!is_hex64(&"A".repeat(64)));
        assert!(!is_hex64("abc"));
    }
}
