//! Portefeuilles `*.dunikey` au format `Type: PubSec` (duniterpy / gcli).

use astro_core::Seed;
use zeroize::Zeroizing;

/// Erreurs de lecture d'un `.dunikey`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DunikeyError {
    /// Pas un fichier `Type: PubSec`.
    #[error("format dunikey non reconnu (Type: PubSec attendu)")]
    NotPubSec,
    /// Champ `pub:` ou `sec:` absent.
    #[error("champ {0} absent")]
    Missing(&'static str),
    /// Base58 invalide ou longueur inattendue.
    #[error("champ {0} illisible")]
    Bad(&'static str),
    /// La clé publique annoncée ne correspond pas à la clé secrète.
    #[error("clé publique incohérente avec la clé secrète")]
    Mismatch,
}

/// Portefeuille lu : graine ed25519 (effacée à la destruction) et clé publique Ğ1 v1 (base58).
pub struct Dunikey {
    seed: Zeroizing<[u8; 32]>,
    /// Clé publique base58 (adresse Ğ1 v1).
    pub public: String,
}

impl Dunikey {
    /// Lit le texte d'un `.dunikey`.
    pub fn parse(text: &str) -> Result<Self, DunikeyError> {
        let field = |name: &str| {
            text.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
            })
        };
        if field("type").as_deref() != Some("PubSec") {
            return Err(DunikeyError::NotPubSec);
        }
        let public = field("pub").ok_or(DunikeyError::Missing("pub"))?;
        let sec = Zeroizing::new(bs58::decode(field("sec").ok_or(DunikeyError::Missing("sec"))?).into_vec().map_err(|_| DunikeyError::Bad("sec"))?);
        // `sec` = graine ‖ clé publique (64 octets, format nacl) ; la graine seule (32 octets) est aussi acceptée.
        let seed: [u8; 32] = match sec.len() {
            32 | 64 => sec[..32].try_into().map_err(|_| DunikeyError::Bad("sec"))?,
            _ => return Err(DunikeyError::Bad("sec")),
        };
        let derived = pubkey_of_seed(&seed);
        if sec.len() == 64 && sec[32..] != derived {
            return Err(DunikeyError::Mismatch);
        }
        let announced = bs58::decode(&public).into_vec().map_err(|_| DunikeyError::Bad("pub"))?;
        if announced != derived {
            return Err(DunikeyError::Mismatch);
        }
        Ok(Self { seed: Zeroizing::new(seed), public })
    }

    /// Graine pour les dérivations (PeerID, signature Duniter).
    pub fn seed(&self) -> Seed {
        Seed::from_ed25519_seed(*self.seed)
    }

    /// Graine brute ed25519 (32 octets).
    pub fn ed25519_seed(&self) -> Zeroizing<[u8; 32]> {
        self.seed.clone()
    }
}

fn pubkey_of_seed(seed: &[u8; 32]) -> [u8; 32] {
    Seed::from_ed25519_seed(*seed).ed25519_public()
}

/// Contenu d'un `.dunikey` pour une graine (tests et outils d'installation).
pub fn render(seed: &[u8; 32]) -> String {
    let public = Seed::from_ed25519_seed(*seed).ed25519_public();
    let mut sec = [0u8; 64];
    sec[..32].copy_from_slice(seed);
    sec[32..].copy_from_slice(&public);
    let text = format!("Type: PubSec\nVersion: 1\npub: {}\nsec: {}\n", bs58::encode(public).into_string(), bs58::encode(sec).into_string());
    sec.fill(0);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_dunikey_round_trips_and_gives_the_g1_identity() {
        let text = render(&[5; 32]);
        let k = Dunikey::parse(&text).unwrap();
        assert_eq!(*k.ed25519_seed(), [5; 32]);
        assert_eq!(k.public, k.seed().g1_pubkey());
        assert!(k.seed().ipfs_peer_id().starts_with("12D3Koo"));
    }

    #[test]
    fn corrupted_or_foreign_files_are_refused() {
        let ok = render(&[5; 32]);
        assert_eq!(Dunikey::parse("Type: WIF\n").err(), Some(DunikeyError::NotPubSec));
        assert_eq!(Dunikey::parse("Type: PubSec\nsec: abc\n").err(), Some(DunikeyError::Missing("pub")));
        let other_pub = bs58::encode(Seed::from_ed25519_seed([6; 32]).ed25519_public()).into_string();
        let swapped = ok.lines().map(|l| if l.starts_with("pub:") { format!("pub: {other_pub}") } else { l.to_owned() }).collect::<Vec<_>>().join("\n");
        assert_eq!(Dunikey::parse(&swapped).err(), Some(DunikeyError::Mismatch));
        assert_eq!(Dunikey::parse("Type: PubSec\npub: 0OIl\nsec: 0OIl\n").err(), Some(DunikeyError::Bad("sec")));
    }
}
