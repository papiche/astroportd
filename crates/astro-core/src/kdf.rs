//! Dérivations de clés : scrypt Duniter, Ğ1, IPFS, Nostr, et étirement PBKDF2 de la clé LOVE.
//!
//! Chaîne identique à `tools/keygen` :
//! `seed = scrypt(password=pepper, salt=salt, N=4096, r=16, p=1, len=32)` ;
//! Ğ1 = base58(ed25519 publique) ; PeerID IPFS = base58(multihash identité du protobuf de la clé) ;
//! Nostr = `secp256k1(sha256(seed))`, clé publique x-only.

use std::fmt;

use base64::Engine;
use blake2::Blake2b512;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::SigningKey;
use secp256k1::{Keypair, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::formula::Credentials;

/// Paramètres scrypt Duniter (v1 et v2s) : N = 2^12.
const SCRYPT_LOG_N: u8 = 12;
const SCRYPT_R: u32 = 16;
const SCRYPT_P: u32 = 1;
const SEED_LEN: usize = 32;

/// Domaine de l'étirement LOVE (`DOMAIN_SALT` de `atom4love_publish.py`, `atomic_keys.dart`).
pub const LOVE_DOMAIN: &[u8] = b"uplanet-a4l-v1";
/// Itérations PBKDF2 de l'étirement LOVE.
pub const LOVE_ITERATIONS: u32 = 600_000;

/// Erreurs de dérivation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KdfError {
    /// Paramètres scrypt refusés.
    #[error("paramètres scrypt invalides")]
    ScryptParams,
    /// Échec scrypt.
    #[error("scrypt a échoué")]
    Scrypt,
    /// La graine ne donne pas une clé secp256k1 valide (probabilité ≈ 2^-128).
    #[error("clé secp256k1 invalide")]
    BadSecp,
    /// Encodage bech32 impossible.
    #[error("encodage bech32 impossible")]
    Bech32,
    /// Préfixe de réseau SS58 hors de 0..=16383.
    #[error("préfixe SS58 hors bornes")]
    Ss58Prefix,
    /// Adresse SS58 illisible (base58, longueur ou somme de contrôle).
    #[error("adresse SS58 invalide")]
    BadSs58,
}

/// Graine ed25519 de 32 octets dérivée des crédentiels, effacée à la destruction.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Seed([u8; SEED_LEN]);

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed(**)")
    }
}

/// `scrypt(pepper, salt)` : graine de `duniterpy.SigningKey.from_credentials(salt, pepper)`.
pub fn duniter_seed(salt: &str, pepper: &str) -> Result<Seed, KdfError> {
    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SEED_LEN).map_err(|_| KdfError::ScryptParams)?;
    let mut out = [0u8; SEED_LEN];
    scrypt::scrypt(pepper.as_bytes(), salt.as_bytes(), &params, &mut out).map_err(|_| KdfError::Scrypt)?;
    Ok(Seed(out))
}

impl Seed {
    /// Graine déjà connue (fichier `.dunikey` : clé ed25519 de la station), sans dérivation scrypt.
    pub fn from_ed25519_seed(seed: [u8; SEED_LEN]) -> Self {
        Self(seed)
    }

    /// Graine ed25519 de 32 octets (clé privée IPFS / libp2p). Effacée à la destruction de la copie.
    pub fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0)
    }

    /// Clé publique ed25519 (32 octets).
    pub fn ed25519_public(&self) -> [u8; 32] {
        SigningKey::from_bytes(&self.0).verifying_key().to_bytes()
    }

    /// Clé publique Ğ1 v1 : base58 de la clé ed25519.
    pub fn g1_pubkey(&self) -> String {
        bs58::encode(self.ed25519_public()).into_string()
    }

    /// Adresse SS58 Ğ1 v2s (préfixe de réseau 4450) de la clé ed25519.
    pub fn g1_ss58(&self) -> Result<String, KdfError> {
        ss58_encode(G1_SS58_PREFIX, &self.ed25519_public())
    }

    /// PeerID IPFS (`12D3Koo…`) : multihash identité de `protobuf{Type=Ed25519, Data=pub}`.
    pub fn ipfs_peer_id(&self) -> String {
        let mut mh = Vec::with_capacity(38);
        mh.extend_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
        mh.extend_from_slice(&self.ed25519_public());
        bs58::encode(mh).into_string()
    }

    /// Clés Nostr : clé privée `sha256(seed)`.
    pub fn nostr_keys(&self) -> Result<NostrKeys, KdfError> {
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&Sha256::digest(self.0));
        let secret = SecretKey::from_slice(&sk).map_err(|_| KdfError::BadSecp);
        let result = secret.map(|secret| {
            let secp = Secp256k1::signing_only();
            let (xonly, _) = Keypair::from_secret_key(&secp, &secret).x_only_public_key();
            NostrKeys { secret: sk, public: xonly.serialize() }
        });
        sk.zeroize();
        result
    }
}

/// Paire de clés Nostr dérivée.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct NostrKeys {
    secret: [u8; 32],
    public: [u8; 32],
}

impl NostrKeys {
    /// Clé publique x-only en hexadécimal (64 caractères) : le `HEX` des MULTIPASS.
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.public)
    }

    /// `npub1…`.
    pub fn npub(&self) -> Result<String, KdfError> {
        bech32_encode("npub", &self.public)
    }

    /// `nsec1…`.
    pub fn nsec(&self) -> Result<Zeroizing<String>, KdfError> {
        bech32_encode("nsec", &self.secret).map(Zeroizing::new)
    }

    /// Clé privée brute, pour signer.
    pub fn secret_bytes(&self) -> &[u8; 32] {
        &self.secret
    }
}

impl fmt::Debug for NostrKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NostrKeys(pub={})", hex::encode(self.public))
    }
}

fn bech32_encode(hrp: &str, data: &[u8]) -> Result<String, KdfError> {
    let hrp = bech32::Hrp::parse(hrp).map_err(|_| KdfError::Bech32)?;
    bech32::encode::<bech32::Bech32>(hrp, data).map_err(|_| KdfError::Bech32)
}

/// Préfixe de réseau SS58 de la Ğ1 (Duniter v2s), identique à `tools/g1pub_to_ss58.py`.
pub const G1_SS58_PREFIX: u16 = 4450;

/// Encode une clé publique de 32 octets en adresse SS58 pour `prefix` (0..=16383).
///
/// Charge utile : préfixe (1 octet si < 64, sinon 2 octets) ‖ clé ‖ 2 octets de
/// `blake2b-512("SS58PRE" ‖ préfixe ‖ clé)`, le tout en base58.
pub fn ss58_encode(prefix: u16, pubkey: &[u8; 32]) -> Result<String, KdfError> {
    let mut payload = ss58_prefix_bytes(prefix)?;
    payload.extend_from_slice(pubkey);
    let sum = ss58_checksum(&payload);
    payload.extend_from_slice(&sum);
    Ok(bs58::encode(payload).into_string())
}

/// Décode une adresse SS58 en `(préfixe, clé publique)` après vérification de la somme de contrôle.
pub fn ss58_decode(address: &str) -> Result<(u16, [u8; 32]), KdfError> {
    let data = bs58::decode(address).into_vec().map_err(|_| KdfError::BadSs58)?;
    let prefix_len = match data.first() {
        Some(b) if *b < 64 => 1,
        Some(b) if *b < 128 => 2,
        _ => return Err(KdfError::BadSs58),
    };
    if data.len() != prefix_len + 32 + 2 {
        return Err(KdfError::BadSs58);
    }
    let (payload, sum) = data.split_at(data.len() - 2);
    if ss58_checksum(payload) != sum {
        return Err(KdfError::BadSs58);
    }
    let prefix = if prefix_len == 1 {
        u16::from(data[0])
    } else {
        let (first, second) = (u16::from(data[0] & 0x3f), u16::from(data[1]));
        (first << 2) | (second >> 6) | ((second & 0x3f) << 8)
    };
    let mut key = [0u8; 32];
    key.copy_from_slice(&payload[prefix_len..]);
    Ok((prefix, key))
}

fn ss58_prefix_bytes(prefix: u16) -> Result<Vec<u8>, KdfError> {
    match prefix {
        0..=63 => Ok(vec![prefix as u8]),
        64..=16_383 => {
            let first = (((prefix & 0b0000_0000_1111_1100) >> 2) as u8) | 0b0100_0000;
            let second = ((prefix >> 8) as u8) | (((prefix & 0b11) as u8) << 6);
            Ok(vec![first, second])
        }
        _ => Err(KdfError::Ss58Prefix),
    }
}

fn ss58_checksum(payload: &[u8]) -> [u8; 2] {
    let mut h = Blake2b512::new();
    h.update(b"SS58PRE");
    h.update(payload);
    let out = h.finalize();
    [out[0], out[1]]
}

/// Étirement LOVE : `base64url_sans_padding(PBKDF2-HMAC-SHA256(raw, "uplanet-a4l-v1", 600 000, 32))`.
///
/// Identique à `stretch` de `atom4love_publish.py` et à `stretchKey` de `atomic_keys.dart`.
pub fn love_stretch(raw: &str) -> Zeroizing<String> {
    let mut derived = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<Sha256>(raw.as_bytes(), LOVE_DOMAIN, LOVE_ITERATIONS, &mut *derived);
    Zeroizing::new(URL_SAFE_NO_PAD.encode(*derived))
}

/// Crédentiels de la clé LOVE : salt et pepper bruts (naissance, conception) étirés séparément,
/// puis passés à la même chaîne scrypt que les autres clés. Jamais isolés par UPlanet.
pub fn love_credentials(raw_salt: &str, raw_pepper: &str) -> Credentials {
    Credentials::new(love_stretch(raw_salt).to_string(), love_stretch(raw_pepper).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sorties de `tools/g1pub_to_ss58.py` (préfixe 4450) pour des clés v1 issues de `keygen`.
    const SS58_VECTORS: [(&str, &str); 4] = [
        ("6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN", "g1LsRGbH5st3WZfdF2SpknxM12W1THsmU5iFzwgKWLHDQqZDq"),
        ("9EUzPLqpCBw4FnUfKi9ChFvDQVmo5HmamnB7LBe7GrR7", "g1Mk9j82asAuAPJ3de5cPiS66MDGuL1vYELTzCt9qzTM1b3th"),
        ("6tUUePg7FMjgbQXpkxMPsczzUULurXJR361qFFMTBxcY", "g1LxU4Wdq1sqjB2oQjDDaq13ENq1FDKjLk6etiuQnfgWJfAaP"),
        ("DP35YSK14nALHCMLgRNuo2xvsexPgFY18ZAAJJwj5Eoe", "g1P8x5iHQnUr3mbL2fnuBgfsob11AMqMJHuy5BJ38RBcGaK22"),
    ];

    fn key_of(v1: &str) -> [u8; 32] {
        let mut k = [0u8; 32];
        k.copy_from_slice(&bs58::decode(v1).into_vec().unwrap());
        k
    }

    #[test]
    fn ss58_matches_the_legacy_converter() {
        for (v1, ss58) in SS58_VECTORS {
            assert_eq!(ss58_encode(G1_SS58_PREFIX, &key_of(v1)).unwrap(), ss58);
            assert_eq!(ss58_decode(ss58).unwrap(), (G1_SS58_PREFIX, key_of(v1)));
        }
    }

    #[test]
    fn seed_ss58_is_the_ss58_of_its_g1_key() {
        let seed = duniter_seed("salt-test", "pepper-test").unwrap();
        assert_eq!(seed.g1_pubkey(), "6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN");
        assert_eq!(seed.g1_ss58().unwrap(), SS58_VECTORS[0].1);
    }

    #[test]
    fn ss58_format_is_g1_prefixed_and_base58() {
        for (_, ss58) in SS58_VECTORS {
            assert!(ss58.starts_with("g1"), "{ss58}");
            assert!((47..=49).contains(&ss58.len()), "{ss58}");
            assert!(ss58.chars().all(|c| c.is_ascii_alphanumeric() && !"0OIl".contains(c)));
        }
    }

    #[test]
    fn ss58_rejects_corruption() {
        let good = SS58_VECTORS[0].1;
        let mut bad = good.to_owned();
        bad.pop();
        bad.push('Z');
        assert_eq!(ss58_decode(&bad), Err(KdfError::BadSs58));
        assert_eq!(ss58_decode("g1"), Err(KdfError::BadSs58));
        assert_eq!(ss58_decode("0OIl"), Err(KdfError::BadSs58));
        assert_eq!(ss58_decode(SS58_VECTORS[0].0), Err(KdfError::BadSs58), "une clé v1 n'est pas une adresse SS58");
    }

    #[test]
    fn ss58_prefix_round_trips_across_encodings() {
        let key = [7u8; 32];
        for prefix in [0u16, 42, 63, 64, 1000, 4450, 16_383] {
            let a = ss58_encode(prefix, &key).unwrap();
            assert_eq!(ss58_decode(&a).unwrap(), (prefix, key), "préfixe {prefix}");
        }
        assert_eq!(ss58_encode(16_384, &key), Err(KdfError::Ss58Prefix));
    }
}
