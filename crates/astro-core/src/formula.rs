//! Formules de crédentiels : comment `(SALT, PEPPER)` devient la paire passée à `keygen`.
//!
//! Sources legacy : `tools/my.sh` (`uplanet_salt`, `uplanet_pepper`, `resolve_player_dunikey`,
//! `init_and_cache_wallet`) et `_12345.sh:211-232` (clé swarm).

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::kdf::{self, KdfError};

/// Formule de dérivation des crédentiels d'une clé.
///
/// Persistée par compte (`identities.key_formula`). Aucune formule n'entraîne la destruction d'un compte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyFormula {
    /// `(SALT, PEPPER_<UPLANET_SALT>)`, avec `UPLANET_SALT = sha256(UPLANETNAME)[..16]` : référence des nouveaux comptes.
    Isolated,
    /// `(SALT, PEPPER)` : comptes créés avant l'isolation par UPlanet. Jamais choisie pour un nouveau compte.
    Legacy,
    /// `(SALT, PEPPER)` fournis tels quels : clés NODE (`secret.june`), ZenCard (diceware), volontairement identiques d'une UPlanet à l'autre.
    Raw,
    /// `(SECRET1‖UPLANETNAME, SECRET2‖UPLANETNAME)` : clés MySwarm.
    ConcatSwarm,
    /// `(UPLANETNAME[.suffixe], UPLANETNAME[.suffixe])` : portefeuilles coopératifs (`uplanet.G1`, `.SOCIETY`, `.CAPITAL`…).
    CoopPrefix,
}

impl KeyFormula {
    /// Toutes les formules.
    pub const ALL: [KeyFormula; 5] =
        [Self::Isolated, Self::Legacy, Self::Raw, Self::ConcatSwarm, Self::CoopPrefix];

    /// Ordre d'essai pour retrouver la formule d'un compte existant (comme `resolve_player_dunikey`).
    pub const RESOLVE_ORDER: [KeyFormula; 2] = [Self::Isolated, Self::Legacy];

    /// Nom stable stocké en base et dans `.keyformula`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::Legacy => "legacy",
            Self::Raw => "raw",
            Self::ConcatSwarm => "concat_swarm",
            Self::CoopPrefix => "coop_prefix",
        }
    }

    /// Calcule les crédentiels à fournir à scrypt.
    pub fn credentials(self, input: &FormulaInput<'_>) -> Result<Credentials, FormulaError> {
        match self {
            Self::Isolated => {
                let salt = uplanet_salt(input.uplanet_name)?;
                Ok(Credentials::new(input.salt.to_owned(), format!("{}_{salt}", input.pepper)))
            }
            Self::Legacy | Self::Raw => Ok(Credentials::new(input.salt.to_owned(), input.pepper.to_owned())),
            Self::ConcatSwarm => {
                let name = require_name(input.uplanet_name)?;
                Ok(Credentials::new(format!("{}{name}", input.salt), format!("{}{name}", input.pepper)))
            }
            Self::CoopPrefix => {
                let name = require_name(input.uplanet_name)?;
                let label = match input.coop_suffix {
                    Some(s) if !s.is_empty() => format!("{name}.{s}"),
                    _ => name.to_owned(),
                };
                Ok(Credentials::new(label.clone(), label))
            }
        }
    }
}

impl fmt::Display for KeyFormula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for KeyFormula {
    type Err = FormulaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| FormulaError::UnknownFormula(s.to_owned()))
    }
}

/// Entrées d'une formule.
#[derive(Debug, Clone, Copy)]
pub struct FormulaInput<'a> {
    /// `SALT` du compte (ou `SECRET1` pour `ConcatSwarm`). Ignoré par `CoopPrefix`.
    pub salt: &'a str,
    /// `PEPPER` du compte (ou `SECRET2`). Ignoré par `CoopPrefix`.
    pub pepper: &'a str,
    /// `UPLANETNAME` de la constellation.
    pub uplanet_name: &'a str,
    /// Suffixe du portefeuille coopératif (`G1`, `SOCIETY`, `TREASURY`…) ; `None` pour `UPLANETNAME` seul. Réservé à `CoopPrefix`.
    pub coop_suffix: Option<&'a str>,
}

/// Paire `(salt, pepper)` prête pour scrypt, effacée de la mémoire à la destruction.
#[derive(Clone)]
pub struct Credentials {
    salt: Zeroizing<String>,
    pepper: Zeroizing<String>,
}

impl Credentials {
    /// Crédentiels bruts.
    pub fn new(salt: String, pepper: String) -> Self {
        Self { salt: Zeroizing::new(salt), pepper: Zeroizing::new(pepper) }
    }

    /// Salt effectif.
    pub fn salt(&self) -> &str {
        &self.salt
    }

    /// Pepper effectif.
    pub fn pepper(&self) -> &str {
        &self.pepper
    }

    /// Graine Duniter (scrypt) de ces crédentiels.
    pub fn seed(&self) -> Result<kdf::Seed, KdfError> {
        kdf::duniter_seed(&self.salt, &self.pepper)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials(**)")
    }
}

/// Erreurs de formule.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FormulaError {
    /// `UPLANETNAME` vide : la formule demandée en a besoin.
    #[error("UPLANETNAME requis pour cette formule")]
    MissingUplanetName,
    /// Nom de formule inconnu.
    #[error("formule inconnue : {0}")]
    UnknownFormula(String),
    /// Format de référence illisible.
    #[error("référence de clé illisible")]
    BadReference,
    /// Échec de dérivation.
    #[error("dérivation impossible : {0}")]
    Kdf(#[from] KdfError),
}

fn require_name(name: &str) -> Result<&str, FormulaError> {
    if name.is_empty() { Err(FormulaError::MissingUplanetName) } else { Ok(name) }
}

/// `sha256(UPLANETNAME)` en hexadécimal, tronqué à 16 caractères (`uplanet_salt()` de `my.sh`).
pub fn uplanet_salt(uplanet_name: &str) -> Result<String, FormulaError> {
    let name = require_name(uplanet_name)?;
    Ok(hex::encode(Sha256::digest(name.as_bytes()))[..16].to_owned())
}

/// Type de clé publique de référence connue d'un compte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// Clé publique Ğ1 : v1 (base58 de la clé ed25519) ou adresse SS58 v2s `g1…` (préfixe 4450).
    G1,
    /// Clé Nostr, en hexadécimal (64) ou `npub1…`.
    Nostr,
    /// PeerID IPFS `12D3Koo…`, éventuellement préfixé par `/ipns/`.
    Ipns,
}

/// Retrouve la formule qui reproduit une clé publique de référence (`Isolated` d'abord, puis `Legacy`).
///
/// `None` si aucune ne correspond : l'appelant ne doit alors pas signer avec la clé dérivée.
pub fn resolve_formula(
    kind: KeyKind,
    salt: &str,
    pepper: &str,
    uplanet_name: &str,
    reference: &str,
) -> Result<Option<KeyFormula>, FormulaError> {
    let wanted = normalize_reference(kind, reference)?;
    for formula in KeyFormula::RESOLVE_ORDER {
        let creds = formula.credentials(&FormulaInput { salt, pepper, uplanet_name, coop_suffix: None })?;
        let seed = creds.seed()?;
        let got = match kind {
            KeyKind::G1 => seed.g1_pubkey(),
            KeyKind::Nostr => seed.nostr_keys()?.pubkey_hex(),
            KeyKind::Ipns => seed.ipfs_peer_id(),
        };
        if got == wanted {
            return Ok(Some(formula));
        }
    }
    Ok(None)
}

fn normalize_reference(kind: KeyKind, reference: &str) -> Result<String, FormulaError> {
    let r = reference.trim();
    match kind {
        // Une adresse SS58 Ğ1 v2s (préfixe 4450) est ramenée à la clé v1 base58 de la même clé ed25519.
        KeyKind::G1 if r.starts_with("g1") && r.len() > 44 => {
            let (prefix, key) = kdf::ss58_decode(r)?;
            if prefix != kdf::G1_SS58_PREFIX {
                return Err(FormulaError::BadReference);
            }
            Ok(bs58::encode(key).into_string())
        }
        KeyKind::G1 => Ok(r.to_owned()),
        KeyKind::Ipns => Ok(r.trim_start_matches("/ipns/").to_owned()),
        KeyKind::Nostr if r.starts_with("npub1") => {
            let (hrp, data) = bech32::decode(r).map_err(|_| FormulaError::BadReference)?;
            if hrp.as_str() != "npub" || data.len() != 32 {
                return Err(FormulaError::BadReference);
            }
            Ok(hex::encode(data))
        }
        KeyKind::Nostr => Ok(r.to_ascii_lowercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(salt: &'a str, pepper: &'a str) -> FormulaInput<'a> {
        FormulaInput { salt, pepper, uplanet_name: "UPlanetTestName", coop_suffix: None }
    }

    #[test]
    fn uplanet_salt_matches_my_sh() {
        // echo -n UPlanetTestName | sha256sum | cut -c1-16
        assert_eq!(uplanet_salt("UPlanetTestName").unwrap(), "aeeb4f9fe9753492");
        assert_eq!(uplanet_salt(""), Err(FormulaError::MissingUplanetName));
    }

    #[test]
    fn credentials_per_formula() {
        let i = input("alice@example.org", "PEP");
        let c = KeyFormula::Isolated.credentials(&i).unwrap();
        assert_eq!((c.salt(), c.pepper()), ("alice@example.org", "PEP_aeeb4f9fe9753492"));
        let c = KeyFormula::Legacy.credentials(&i).unwrap();
        assert_eq!((c.salt(), c.pepper()), ("alice@example.org", "PEP"));
        let c = KeyFormula::Raw.credentials(&i).unwrap();
        assert_eq!((c.salt(), c.pepper()), ("alice@example.org", "PEP"));
        let c = KeyFormula::ConcatSwarm.credentials(&i).unwrap();
        assert_eq!((c.salt(), c.pepper()), ("alice@example.orgUPlanetTestName", "PEPUPlanetTestName"));
        let i2 = FormulaInput { coop_suffix: Some("G1"), ..i };
        let c = KeyFormula::CoopPrefix.credentials(&i2).unwrap();
        assert_eq!((c.salt(), c.pepper()), ("UPlanetTestName.G1", "UPlanetTestName.G1"));
        let c = KeyFormula::CoopPrefix.credentials(&i).unwrap();
        assert_eq!((c.salt(), c.pepper()), ("UPlanetTestName", "UPlanetTestName"));
    }

    #[test]
    fn formulas_needing_a_name_refuse_an_empty_one() {
        let i = FormulaInput { salt: "s", pepper: "p", uplanet_name: "", coop_suffix: None };
        for f in [KeyFormula::Isolated, KeyFormula::ConcatSwarm, KeyFormula::CoopPrefix] {
            assert_eq!(f.credentials(&i).err(), Some(FormulaError::MissingUplanetName), "{f}");
        }
        assert!(KeyFormula::Legacy.credentials(&i).is_ok());
    }

    #[test]
    fn names_round_trip() {
        for f in KeyFormula::ALL {
            assert_eq!(f.as_str().parse::<KeyFormula>().unwrap(), f);
        }
        assert!("destroyed".parse::<KeyFormula>().is_err());
    }

    #[test]
    fn credentials_debug_never_leaks() {
        let c = Credentials::new("secret-salt".into(), "secret-pepper".into());
        assert!(!format!("{c:?}").contains("secret"));
    }
}
