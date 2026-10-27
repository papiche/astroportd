//! Accès aux clés de signature des MULTIPASS hébergés.
//!
//! Les clés racines restent dans des fichiers `0600` (arbitrage C) : elles ne sont lues que pour signer
//! ou déchiffrer, tenues dans des `Zeroizing` et jamais copiées en base.

use std::collections::HashMap;
use std::path::PathBuf;

use zeroize::Zeroizing;

/// Clé secrète de 32 octets, effacée à la destruction.
pub type SecretBytes = Zeroizing<[u8; 32]>;

/// Source de clés secrètes, indexées par clé publique x-only (64 hex).
pub trait KeySource: Send + Sync {
    /// Clé secrète du compte `pubkey_hex`, si cette station la détient.
    fn secret(&self, pubkey_hex: &str) -> Option<SecretBytes>;
}

/// Clés fixes en mémoire (tests, intégration).
#[derive(Default)]
pub struct StaticKeySource {
    keys: HashMap<String, [u8; 32]>,
}

impl StaticKeySource {
    /// Source vide.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ajoute une clé ; retourne sa clé publique.
    pub fn with(mut self, secret: [u8; 32]) -> Self {
        if let Ok(pk) = astro_nostr_proto::pubkey_of(&secret) {
            self.keys.insert(pk, secret);
        }
        self
    }
}

impl KeySource for StaticKeySource {
    fn secret(&self, pubkey_hex: &str) -> Option<SecretBytes> {
        self.keys.get(pubkey_hex).map(|k| Zeroizing::new(*k))
    }
}

/// Lit `<racine>/<email>/.secret.nostr`, au format legacy d'une ligne :
/// `NSEC=nsec1…; NPUB=npub1…; HEX=<64 hex>`.
pub struct FileKeySource {
    root: PathBuf,
}

impl FileKeySource {
    /// Racine contenant un dossier par compte (`~/.zen/game/nostr`).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl KeySource for FileKeySource {
    fn secret(&self, pubkey_hex: &str) -> Option<SecretBytes> {
        for entry in std::fs::read_dir(&self.root).ok()?.flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path().join(".secret.nostr")) else { continue };
            let field = |name: &str| text.split(';').find_map(|p| p.trim().strip_prefix(name)).map(|v| v.trim().to_owned());
            if field("HEX=").as_deref() != Some(pubkey_hex) {
                continue;
            }
            let nsec = Zeroizing::new(field("NSEC=")?);
            let (hrp, data) = bech32::decode(&nsec).ok()?;
            if hrp.as_str() != "nsec" || data.len() != 32 {
                return None;
            }
            let mut secret = Zeroizing::new([0u8; 32]);
            secret.copy_from_slice(&data);
            // Une clé qui ne redonne pas la clé publique annoncée est refusée : jamais signer avec le mauvais compte.
            return (astro_nostr_proto::pubkey_of(&secret).ok()?.as_str() == pubkey_hex).then_some(secret);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nsec(secret: &[u8; 32]) -> String {
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), secret).unwrap()
    }

    #[test]
    fn static_source_finds_by_pubkey() {
        let src = StaticKeySource::new().with([7; 32]);
        let pk = astro_nostr_proto::pubkey_of(&[7; 32]).unwrap();
        assert_eq!(*src.secret(&pk).unwrap(), [7; 32]);
        assert!(src.secret(&"00".repeat(32)).is_none());
    }

    #[test]
    fn file_source_reads_the_legacy_format_and_rejects_mismatches() {
        let dir = std::env::temp_dir().join(format!("astro-keys-{}", std::process::id()));
        let (good, other) = ([9u8; 32], [8u8; 32]);
        let (good_pk, other_pk) = (astro_nostr_proto::pubkey_of(&good).unwrap(), astro_nostr_proto::pubkey_of(&other).unwrap());
        for (email, line) in [
            ("alice@example.org", format!("NSEC={}; NPUB=npub1x; HEX={good_pk}\n", nsec(&good))),
            // Fichier incohérent : HEX annoncé ≠ clé réelle.
            ("mallory@example.org", format!("NSEC={}; NPUB=npub1y; HEX={other_pk}\n", nsec(&good))),
        ] {
            std::fs::create_dir_all(dir.join(email)).unwrap();
            std::fs::write(dir.join(email).join(".secret.nostr"), line).unwrap();
        }
        std::fs::create_dir_all(dir.join("vide@example.org")).unwrap();
        let src = FileKeySource::new(&dir);
        assert_eq!(*src.secret(&good_pk).unwrap(), good);
        assert!(src.secret(&other_pk).is_none(), "clé qui ne correspond pas à son HEX");
        assert!(src.secret(&"ab".repeat(32)).is_none());
        assert!(FileKeySource::new(dir.join("inexistant")).secret(&good_pk).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
