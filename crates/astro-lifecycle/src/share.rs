//! Clé de secours en 3 parts (Shamir, 2 suffisent) : le secret `SALT`/`PEPPER` d'un MULTIPASS n'est plus donné en un seul QR.
//!
//! | Part | Qui la garde | Rôle |
//! |------|--------------|------|
//! | **P1** | le membre (QR, à imprimer) | seule, elle ne révèle rien |
//! | **P2** | la station d'accueil (dossier du compte, `0600`) | permet à la station de réimprimer P1 ou de réenvoyer P3 |
//! | **P3** | les amis qui hébergent sa sauvegarde | « j'appelle un ami » : il lit P3 à voix haute, ou la montre en QR |
//!
//! P1 + P3 rétablissent le compte sur une autre station, **sans** que l'ancienne existe encore. Une part isolée (P1 volée, P3 d'un
//! ami curieux) ne donne **aucune** information sur le secret : c'est une propriété du partage de Shamir sur GF(2⁸), pas une
//! affaire de confiance.
//!
//! Le texte d'une part porte en clair l'email, la clé publique et la formule (rien de secret : la clé publique est sur les relais) ;
//! seule la valeur `data` est un partage du secret. Une somme de contrôle de 4 hexadécimaux attrape les fautes de saisie.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::multipass::{MultipassError, SECRET_MAX, SecretCard, normalize_email, write_private};
use astro_core::formula::KeyFormula;

/// Nombre de parts créées.
pub const SHARES: u8 = 3;
/// Nombre de parts nécessaires.
pub const THRESHOLD: usize = 2;
/// Dossier du compte (`<nostr>/<email>/`) où la station garde ses parts.
pub const DIR: &str = ".ssss";

// ───────────────────────────── Shamir sur GF(2⁸) ─────────────────────────────

/// Produit dans GF(2⁸) modulo x⁸ + x⁴ + x³ + x + 1 (le corps d'AES).
fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 == 1 {
            p ^= a;
        }
        let carry = a & 0x80;
        a <<= 1;
        if carry != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

/// Inverse de `a` (≠ 0) : a²⁵⁴.
fn inv(a: u8) -> u8 {
    let mut r = 1;
    let mut base = a;
    let mut e = 254u16;
    while e > 0 {
        if e & 1 == 1 {
            r = mul(r, base);
        }
        base = mul(base, base);
        e >>= 1;
    }
    r
}

/// Découpe `secret` en `n` parts d'abscisses 1..=n dont `k` suffisent. Chaque octet reçoit son propre polynôme aléatoire.
fn split(secret: &[u8], k: usize, n: u8) -> Result<Vec<(u8, Vec<u8>)>, MultipassError> {
    let mut coefficients = Zeroizing::new(vec![0u8; secret.len() * (k - 1)]);
    getrandom::fill(&mut coefficients).map_err(|_| MultipassError::Random)?;
    Ok((1..=n)
        .map(|x| {
            let y = secret
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    // Horner : s + c1·x + c2·x² …
                    let mut acc = 0u8;
                    for j in (0..k - 1).rev() {
                        acc = mul(acc ^ coefficients[i * (k - 1) + j], x);
                    }
                    acc ^ s
                })
                .collect();
            (x, y)
        })
        .collect())
}

/// Valeur en `x` du polynôme qui passe par `points` (interpolation de Lagrange). `x = 0` donne le secret.
fn evaluate(points: &[(u8, &[u8])], x: u8) -> Vec<u8> {
    let len = points[0].1.len();
    (0..len)
        .map(|pos| {
            points.iter().enumerate().fold(0u8, |acc, (i, (xi, yi))| {
                let basis = points.iter().enumerate().filter(|(j, _)| *j != i).fold(1u8, |b, (_, (xj, _))| mul(b, mul(x ^ xj, inv(xi ^ xj))));
                acc ^ mul(yi[pos], basis)
            })
        })
        .collect()
}

// ───────────────────────────── Part de secours ─────────────────────────────

/// Une part de la clé de secours, avec ce qu'il faut pour savoir à quel compte elle appartient.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretShare {
    /// Email du compte (minuscules).
    pub email: String,
    /// Clé Nostr publique du compte (64 hexadécimaux).
    pub hex: String,
    /// Formule de dérivation.
    pub formula: KeyFormula,
    /// Empreinte de l'UPlanet (16 hexadécimaux), vide hors formule isolée.
    pub uplanet: String,
    /// Numéro de la part : 1 (membre), 2 (station), 3 (amis).
    pub index: u8,
    data: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for SecretShare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretShare(P{} de {})", self.index, self.email)
    }
}

fn checksum(body: &str) -> String {
    hex::encode(&Sha256::digest(body.as_bytes())[..2])
}

impl SecretShare {
    /// Début du texte d'une part.
    pub const PREFIX: &'static str = "astro-share:v1";

    /// Texte à encoder en QR (ou à dicter).
    pub fn to_text(&self) -> String {
        let body = format!("{}|{}|{}|{}|{}|{}|{}", Self::PREFIX, self.email, self.hex, self.formula.as_str(), self.uplanet, self.index, URL_SAFE_NO_PAD.encode(&*self.data));
        format!("{body}|{}", checksum(&body))
    }

    /// Lit une part (espaces et fins de ligne autour tolérés), en revalidant chaque champ.
    pub fn parse(text: &str) -> Result<Self, MultipassError> {
        let text = text.trim();
        let (body, crc) = text.rsplit_once('|').ok_or(MultipassError::BadCard("part de secours incomplète"))?;
        if checksum(body) != crc.to_ascii_lowercase() {
            return Err(MultipassError::BadCard("part de secours : somme de contrôle fausse (faute de frappe ?)"));
        }
        let fields: Vec<&str> = body.split('|').collect();
        let [prefix, email, hex, formula, uplanet, index, data] = fields[..] else {
            return Err(MultipassError::BadCard("nombre de champs"));
        };
        if prefix != Self::PREFIX {
            return Err(MultipassError::BadCard("version de part inconnue"));
        }
        let formula: KeyFormula = formula.parse().map_err(|_| MultipassError::BadCard("formule inconnue"))?;
        if !matches!(formula, KeyFormula::Isolated | KeyFormula::Legacy | KeyFormula::Raw) {
            return Err(MultipassError::BadCard("formule non prévue pour un MULTIPASS"));
        }
        let uplanet_ok = if formula == KeyFormula::Isolated { uplanet.len() == 16 && uplanet.bytes().all(|b| b.is_ascii_hexdigit()) } else { uplanet.is_empty() };
        if !uplanet_ok {
            return Err(MultipassError::BadCard("empreinte de l'UPlanet"));
        }
        if !astro_nostr_proto::is_hex64(hex) {
            return Err(MultipassError::BadCard("clé publique"));
        }
        let index: u8 = index.parse().ok().filter(|i| (1..=SHARES).contains(i)).ok_or(MultipassError::BadCard("numéro de part"))?;
        let data = URL_SAFE_NO_PAD.decode(data).map_err(|_| MultipassError::BadCard("contenu de la part"))?;
        if data.len() < 3 || data.len() > 2 * SECRET_MAX + 1 {
            return Err(MultipassError::BadCard("taille de la part"));
        }
        Ok(Self { email: normalize_email(email)?, hex: hex.to_ascii_lowercase(), formula, uplanet: uplanet.to_ascii_lowercase(), index, data: Zeroizing::new(data) })
    }

    fn same_account(&self, other: &Self) -> bool {
        self.email == other.email && self.hex == other.hex && self.formula == other.formula && self.uplanet == other.uplanet && self.data.len() == other.data.len()
    }

    fn like(&self, index: u8, data: Vec<u8>) -> Self {
        Self { email: self.email.clone(), hex: self.hex.clone(), formula: self.formula, uplanet: self.uplanet.clone(), index, data: Zeroizing::new(data) }
    }
}

/// Découpe la carte secrète en P1, P2, P3 (dans cet ordre).
pub fn split_card(card: &SecretCard) -> Result<Vec<SecretShare>, MultipassError> {
    let mut secret = Zeroizing::new(Vec::with_capacity(card.salt.len() + card.pepper.len() + 1));
    secret.extend_from_slice(card.salt.as_bytes());
    secret.push(0);
    secret.extend_from_slice(card.pepper.as_bytes());
    let template = SecretShare { email: card.email.clone(), hex: card.hex.clone(), formula: card.formula, uplanet: card.uplanet.clone(), index: 0, data: Zeroizing::new(Vec::new()) };
    Ok(split(&secret, THRESHOLD, SHARES)?.into_iter().map(|(x, y)| template.like(x, y)).collect())
}

/// Vérifie que ces parts sont du même compte, d'indices distincts, et en nombre suffisant.
fn check(shares: &[SecretShare]) -> Result<(), MultipassError> {
    if shares.len() < THRESHOLD {
        return Err(MultipassError::BadCard("deux parts sont nécessaires (P1 + P3)"));
    }
    for (i, a) in shares.iter().enumerate() {
        if shares[..i].iter().any(|b| b.index == a.index) {
            return Err(MultipassError::BadCard("la même part est donnée deux fois"));
        }
        if !a.same_account(&shares[0]) {
            return Err(MultipassError::BadCard("ces parts ne sont pas du même compte"));
        }
    }
    Ok(())
}

/// Reconstitue la carte secrète à partir de deux parts au moins. La clé publique n'est pas revérifiée ici : [`crate::multipass::recover`]
/// redérive la clé et refuse tout ce qui ne redonne pas `hex`.
pub fn combine(shares: &[SecretShare]) -> Result<SecretCard, MultipassError> {
    check(shares)?;
    let points: Vec<(u8, &[u8])> = shares.iter().take(THRESHOLD).map(|s| (s.index, s.data.as_slice())).collect();
    let secret = Zeroizing::new(evaluate(&points, 0));
    let Some(cut) = secret.iter().position(|b| *b == 0) else { return Err(MultipassError::BadSecrets) };
    let (salt, pepper) = (&secret[..cut], &secret[cut + 1..]);
    let valid = |s: &[u8]| !s.is_empty() && s.len() <= SECRET_MAX && s.iter().all(u8::is_ascii_alphanumeric);
    if !valid(salt) || !valid(pepper) {
        return Err(MultipassError::BadSecrets); // parts de comptes différents, ou erreur : le secret n'a pas la forme attendue
    }
    let s = &shares[0];
    Ok(SecretCard {
        email: s.email.clone(),
        salt: Zeroizing::new(String::from_utf8_lossy(salt).into_owned()),
        pepper: Zeroizing::new(String::from_utf8_lossy(pepper).into_owned()),
        formula: s.formula,
        uplanet: s.uplanet.clone(),
        hex: s.hex.clone(),
    })
}

/// Recalcule les trois parts d'origine à partir de deux d'entre elles : une station rétablie retrouve ainsi P2, et peut
/// réenvoyer P3, **sans** invalider le P1 imprimé ni le P3 que les amis gardent.
pub fn complete(shares: &[SecretShare]) -> Result<Vec<SecretShare>, MultipassError> {
    check(shares)?;
    let points: Vec<(u8, &[u8])> = shares.iter().take(THRESHOLD).map(|s| (s.index, s.data.as_slice())).collect();
    Ok((1..=SHARES).map(|x| shares[0].like(x, evaluate(&points, x))).collect())
}

/// Ce que l'utilisateur peut donner pour rétablir son compte.
pub struct Recovery {
    /// La carte reconstituée.
    pub card: SecretCard,
    /// Les trois parts, si on est parti de parts (sinon elles seront recréées).
    pub shares: Option<Vec<SecretShare>>,
}

impl Recovery {
    /// Lit un texte qui contient soit **deux parts** (une par ligne, ou séparées par des espaces), soit une carte secrète
    /// complète (ancien format, toujours accepté).
    pub fn parse(text: &str) -> Result<Self, MultipassError> {
        let words: Vec<&str> = text.split_whitespace().filter(|w| w.starts_with(SecretShare::PREFIX)).collect();
        if words.is_empty() {
            return Ok(Self { card: SecretCard::parse(text)?, shares: None });
        }
        let given = words.iter().map(|w| SecretShare::parse(w)).collect::<Result<Vec<_>, _>>()?;
        let card = combine(&given)?;
        Ok(Self { card, shares: Some(complete(&given)?) })
    }
}

/// Écrit les trois parts dans `<dossier du compte>/.ssss/{1,2,3}` (`0600`).
pub fn write_shares(dir: &Path, shares: &[SecretShare]) -> Result<(), MultipassError> {
    let folder = dir.join(DIR);
    std::fs::create_dir_all(&folder).map_err(|e| MultipassError::Io { path: folder.clone(), why: e.to_string() })?;
    for s in shares {
        write_private(&folder.join(s.index.to_string()), &s.to_text())?;
    }
    Ok(())
}

/// Lit la part `index` du dossier d'un compte, si la station la garde.
pub fn read_share(dir: &Path, index: u8) -> Option<SecretShare> {
    SecretShare::parse(&std::fs::read_to_string(dir.join(DIR).join(index.to_string())).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> SecretCard {
        SecretCard { email: "lea@example.org".into(), salt: Zeroizing::new("Sel0123456789abcdefghijkl".into()), pepper: Zeroizing::new("Poivre9876543210ZYXWVUTS".into()), formula: KeyFormula::Legacy, uplanet: String::new(), hex: "ab".repeat(32) }
    }

    #[test]
    fn field_arithmetic_is_a_field() {
        for a in 1..=255u8 {
            assert_eq!(mul(a, inv(a)), 1, "{a}");
        }
        assert_eq!(mul(0x57, 0x83), 0xc1, "vecteur de la spécification AES");
    }

    #[test]
    fn any_two_of_the_three_shares_give_the_secret_and_the_third() {
        let shares = split_card(&card()).unwrap();
        assert_eq!(shares.iter().map(|s| s.index).collect::<Vec<_>>(), [1, 2, 3]);
        for (a, b) in [(0, 1), (0, 2), (1, 2), (2, 0)] {
            let got = combine(&[shares[a].clone(), shares[b].clone()]).unwrap();
            assert_eq!((&*got.salt, &*got.pepper), (&*card().salt, &*card().pepper), "P{} + P{}", a + 1, b + 1);
            let all = complete(&[shares[a].clone(), shares[b].clone()]).unwrap();
            assert_eq!(all, shares, "les parts d'origine sont retrouvées à l'identique");
        }
    }

    #[test]
    fn one_share_alone_or_a_bad_pair_reveals_nothing() {
        let (one, other) = (split_card(&card()).unwrap(), split_card(&card()).unwrap());
        assert!(combine(&one[..1]).is_err(), "une part ne suffit pas");
        assert!(combine(&[one[0].clone(), one[0].clone()]).is_err(), "deux fois la même part");
        // Parts de deux découpages différents : le secret reconstitué n'a pas la forme attendue.
        assert!(combine(&[one[0].clone(), other[2].clone()]).is_err());
        // Chaque part, prise seule, n'a pas la même valeur d'un découpage à l'autre (polynômes aléatoires).
        assert_ne!(one[0].data, other[0].data);
        // Compte différent.
        let mut stranger = split_card(&card()).unwrap();
        stranger[1].email = "bob@example.org".into();
        assert!(combine(&[one[0].clone(), stranger[1].clone()]).is_err());
    }

    #[test]
    fn the_text_round_trips_and_catches_typos() {
        let shares = split_card(&card()).unwrap();
        let text = shares[0].to_text();
        assert!(text.starts_with("astro-share:v1|lea@example.org|"));
        assert_eq!(SecretShare::parse(&format!("  {text}\n")).unwrap(), shares[0]);
        let typo = text.replacen("lea@", "leb@", 1);
        assert!(SecretShare::parse(&typo).is_err(), "somme de contrôle");
        assert!(SecretShare::parse("astro-share:v1|x").is_err());
        assert!(SecretShare::parse(&text.replace("|1|", "|4|")).is_err());
        assert!(!text.contains(&*card().salt) && !text.contains(&*card().pepper), "le texte d'une part ne contient pas le secret en clair");
    }

    #[test]
    fn recovery_accepts_two_shares_in_any_order_or_the_old_full_card() {
        let shares = split_card(&card()).unwrap();
        let both = format!("{}\n{}\n", shares[2].to_text(), shares[0].to_text());
        let r = Recovery::parse(&both).unwrap();
        assert_eq!(&*r.card.salt, &*card().salt);
        assert_eq!(r.shares.unwrap(), shares);
        assert!(Recovery::parse(&shares[0].to_text()).is_err(), "une seule part ne suffit pas");
        let old = Recovery::parse(&card().to_text()).unwrap();
        assert!(old.shares.is_none() && *old.card.pepper == *card().pepper);
    }

    #[test]
    fn shares_are_written_privately_and_read_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("astro-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shares = split_card(&card()).unwrap();
        write_shares(&dir, &shares).unwrap();
        for s in &shares {
            let path = dir.join(DIR).join(s.index.to_string());
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(read_share(&dir, s.index).unwrap(), *s);
        }
        assert!(read_share(&dir, 9).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
