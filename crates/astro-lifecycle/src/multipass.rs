//! Création d'un **MULTIPASS** : l'identité d'un membre sur une station (Nostr + Ğ1 + IPNS issus d'un même couple
//! `SALT`/`PEPPER`), sans passer par Astroport.ONE.
//!
//! Reprend les règles de `Astroport.ONE/tools/make_NOSTRCARD.sh` :
//!
//! - `SALT` et `PEPPER` aléatoires (24 caractères alphanumériques, ≤ 56 comme l'exige le DISCO) ;
//! - formule **isolée par UPlanet** : crédentiels `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)`, donc la même paire ne
//!   donne pas la même clé dans deux constellations ; clé Nostr = `sha256(graine)`, clé Ğ1 et IPNS = la même graine ;
//! - fichiers du dossier `<nostr>/<email>/` : `.secret.nostr`, `.secret.dunikey`, `.keyformula`, `G1PUBNOSTR`, `HEX`,
//!   `NPUB`, `LANG`, `NOSTRNS`, `.pass`, `.multipass.json` (0600 pour tout ce qui est secret) ;
//! - refus d'un email déjà inscrit ou d'une clé Nostr déjà prise par un autre compte.
//!
//! Non repris (annoncé, pas simulé) : le découpage SSSS 2‑sur‑3 de DISCO chiffré vers le Capitaine et UPlanet, les QR
//! codes, l'import de la clé IPNS dans kubo et l'envoi du zine par email. La récupération d'un compte repose sur le
//! `SALT`/`PEPPER` remis **une seule fois** au titulaire.

use std::path::{Path, PathBuf};

use astro_core::formula::{FormulaInput, KeyFormula};
use astro_core::kdf::Seed;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use serde_json::json;
use zeroize::Zeroizing;

use crate::dunikey;

/// Longueur des `SALT` et `PEPPER` tirés au hasard.
pub const SECRET_LEN: usize = 24;
/// Plus long `SALT`/`PEPPER` accepté quand ils sont imposés (limite du DISCO).
pub const SECRET_MAX: usize = 56;

/// Erreurs de création.
#[derive(Debug, thiserror::Error)]
pub enum MultipassError {
    /// Adresse email inutilisable.
    #[error("adresse email invalide : {0}")]
    BadEmail(&'static str),
    /// Le compte existe déjà sur cette station.
    #[error("{0} a déjà un MULTIPASS sur cette station")]
    Exists(String),
    /// Une autre adresse porte déjà cette clé Nostr.
    #[error("cette clé Nostr appartient déjà à {0}")]
    IdentityConflict(String),
    /// `SALT`/`PEPPER` imposés hors limites.
    #[error("SALT et PEPPER : 1 à {SECRET_MAX} caractères alphanumériques")]
    BadSecrets,
    /// Dérivation impossible.
    #[error("dérivation : {0}")]
    Derive(String),
    /// Fichier illisible ou non inscriptible.
    #[error("{path} : {why}")]
    Io {
        /// Fichier concerné.
        path: PathBuf,
        /// Cause.
        why: String,
    },
    /// Hasard système indisponible.
    #[error("hasard système indisponible")]
    Random,
}

fn io(path: &Path, e: impl ToString) -> MultipassError {
    MultipassError::Io { path: path.to_owned(), why: e.to_string() }
}

/// Un MULTIPASS tout juste créé. Les secrets sont effacés de la mémoire à la destruction.
pub struct Created {
    /// Email (en minuscules) : nom du dossier du compte.
    pub email: String,
    /// Clé Nostr publique (64 hex).
    pub hex: String,
    /// `npub1…`.
    pub npub: String,
    /// `nsec1…`.
    pub nsec: Zeroizing<String>,
    /// Clé publique Ğ1 v1 (base58).
    pub g1_v1: String,
    /// Adresse Ğ1 v2s (SS58, `g1…`) : le contenu de `G1PUBNOSTR`.
    pub g1_ss58: String,
    /// `/ipns/k51…` : l'espace IPNS du compte.
    pub nostrns: String,
    /// Code PASS (4 chiffres).
    pub pass: String,
    /// `SALT` à conserver : il permet de récupérer le compte.
    pub salt: Zeroizing<String>,
    /// `PEPPER` à conserver.
    pub pepper: Zeroizing<String>,
    /// Langue (`fr`, `en`…).
    pub lang: String,
    /// Dossier du compte.
    pub dir: PathBuf,
    seed: Seed,
}

impl Created {
    /// Clé secrète Nostr (pour signer les événements de naissance).
    pub fn nostr_secret(&self) -> Result<Zeroizing<[u8; 32]>, MultipassError> {
        let keys = self.seed.nostr_keys().map_err(|e| MultipassError::Derive(e.to_string()))?;
        Ok(Zeroizing::new(*keys.secret_bytes()))
    }
}

/// Normalise et valide un email : ASCII imprimable, une seule `@`, aucun séparateur de chemin. Le résultat sert de
/// nom de dossier, donc `..`, `/`, `\` et les caractères de contrôle sont refusés d'emblée.
pub fn normalize_email(raw: &str) -> Result<String, MultipassError> {
    let email = raw.trim().to_ascii_lowercase();
    if email.is_empty() || email.len() > 254 {
        return Err(MultipassError::BadEmail("longueur"));
    }
    if !email.bytes().all(|b| b.is_ascii_alphanumeric() || b"@._+-".contains(&b)) {
        return Err(MultipassError::BadEmail("caractères autorisés : lettres, chiffres et @ . _ + -"));
    }
    let (local, domain) = email.split_once('@').ok_or(MultipassError::BadEmail("@ manquant"))?;
    if local.is_empty() || domain.contains('@') || !domain.contains('.') || domain.starts_with(['.', '-']) || domain.ends_with(['.', '-']) || email.contains("..") {
        return Err(MultipassError::BadEmail("forme"));
    }
    Ok(email)
}

/// Chaîne alphanumérique aléatoire (tirage sans biais).
fn random_alnum(len: usize) -> Result<String, MultipassError> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        getrandom::fill(&mut buf).map_err(|_| MultipassError::Random)?;
        // 62 caractères : on rejette au-delà de 248 (= 62 × 4) pour que chaque lettre soit équiprobable.
        out.extend(buf.iter().filter(|b| **b < 248).map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char).take(len - out.len()));
    }
    Ok(out)
}

fn random_pass() -> Result<String, MultipassError> {
    let mut buf = [0u8; 16];
    loop {
        getrandom::fill(&mut buf).map_err(|_| MultipassError::Random)?;
        if let Some(b) = buf.iter().find(|b| **b < 250) {
            // 4 chiffres : on tire quatre octets non biaisés.
            let mut digits = String::new();
            for b in std::iter::once(b).chain(buf.iter().filter(|x| **x < 250).skip(1)).take(4) {
                digits.push(char::from(b'0' + b % 10));
            }
            if digits.len() == 4 {
                return Ok(digits);
            }
        }
    }
}

/// Espace IPNS (`k51…`) d'une clé ed25519 : CIDv1 `libp2p-key` de la clé publique, en base36 minuscule (`ipfs key import`).
pub fn ipns_name(public: &[u8; 32]) -> String {
    let mut cid = vec![0x01, 0x72, 0x00, 0x24, 0x08, 0x01, 0x12, 0x20];
    cid.extend_from_slice(public);
    let mut digits: Vec<u8> = Vec::new(); // base36, chiffre de poids faible en tête
    for byte in &cid {
        let mut carry = u32::from(*byte);
        for d in &mut digits {
            let v = u32::from(*d) * 256 + carry;
            *d = (v % 36) as u8;
            carry = v / 36;
        }
        while carry > 0 {
            digits.push((carry % 36) as u8);
            carry /= 36;
        }
    }
    let mut out = String::from("k");
    out.extend(digits.iter().rev().map(|d| char::from_digit(u32::from(*d), 36).unwrap_or('0')));
    out
}

fn write_private(path: &Path, content: &str) -> Result<(), MultipassError> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).map_err(|e| io(path, e))?.write_all(content.as_bytes()).map_err(|e| io(path, e))
}

fn write_public(path: &Path, content: &str) -> Result<(), MultipassError> {
    std::fs::write(path, content).map_err(|e| io(path, e))
}

/// Un autre compte porte-t-il déjà ce `HEX` ?
fn hex_owner(root: &Path, hex: &str) -> Option<String> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if let Ok(text) = std::fs::read_to_string(entry.path().join("HEX")) {
            if text.trim() == hex {
                return Some(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Crée un MULTIPASS avec un `SALT`/`PEPPER` aléatoires.
pub fn create(nostr_root: &Path, email: &str, uplanet_name: &str, lang: &str) -> Result<Created, MultipassError> {
    create_with(nostr_root, email, uplanet_name, lang, &random_alnum(SECRET_LEN)?, &random_alnum(SECRET_LEN)?)
}

/// Crée un MULTIPASS avec un `SALT`/`PEPPER` imposés (récupération, tests).
pub fn create_with(nostr_root: &Path, email: &str, uplanet_name: &str, lang: &str, salt: &str, pepper: &str) -> Result<Created, MultipassError> {
    let email = normalize_email(email)?;
    let valid = |s: &str| !s.is_empty() && s.len() <= SECRET_MAX && s.bytes().all(|b| b.is_ascii_alphanumeric());
    if !valid(salt) || !valid(pepper) {
        return Err(MultipassError::BadSecrets);
    }
    let lang = if lang.len() == 2 && lang.bytes().all(|b| b.is_ascii_lowercase()) { lang } else { "fr" };
    let dir = nostr_root.join(&email);
    if dir.exists() {
        return Err(MultipassError::Exists(email));
    }

    let formula = KeyFormula::Isolated;
    let cred = formula.credentials(&FormulaInput { salt, pepper, uplanet_name, coop_suffix: None }).map_err(|e| MultipassError::Derive(e.to_string()))?;
    let seed = cred.seed().map_err(|e| MultipassError::Derive(e.to_string()))?;
    let keys = seed.nostr_keys().map_err(|e| MultipassError::Derive(e.to_string()))?;
    let (hex, npub) = (keys.pubkey_hex(), keys.npub().map_err(|e| MultipassError::Derive(e.to_string()))?);
    let nsec = keys.nsec().map_err(|e| MultipassError::Derive(e.to_string()))?;
    if let Some(other) = hex_owner(nostr_root, &hex) {
        return Err(MultipassError::IdentityConflict(other));
    }
    let g1_v1 = seed.g1_pubkey();
    let g1_ss58 = seed.g1_ss58().map_err(|e| MultipassError::Derive(e.to_string()))?;
    let nostrns = format!("/ipns/{}", ipns_name(&seed.ed25519_public()));
    let pass = random_pass()?;

    // Tout s'écrit dans un dossier de travail renommé d'un coup : jamais de compte à moitié créé.
    std::fs::create_dir_all(nostr_root).map_err(|e| io(nostr_root, e))?;
    let work = nostr_root.join(format!(".creating-{}-{}", std::process::id(), &hex[..8]));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir(&work).map_err(|e| io(&work, e))?;
    let written = (|| {
        write_private(&work.join(".secret.nostr"), &format!("NSEC={}; NPUB={npub}; HEX={hex}\n", &*nsec))?;
        write_private(&work.join(".secret.dunikey"), &dunikey::render(&seed.ed25519_secret_seed()))?;
        write_private(&work.join(".keyformula"), &format!("nostr={f} g1={f} ipns={f}\n", f = formula.as_str()))?;
        write_private(&work.join(".pass"), &format!("{pass}\n"))?;
        let card = json!({
            "g1pub": g1_ss58, "nsec": &*nsec, "npub": npub, "hex": hex, "pass": pass, "nostrns": nostrns.trim_start_matches("/ipns/"),
            "salt": salt, "pepper": pepper, "email": email, "lang": lang, "uplanet": astro_core::formula::uplanet_salt(uplanet_name).unwrap_or_default(),
        });
        write_private(&work.join(".multipass.json"), &format!("{card:#}\n"))?;
        for (name, value) in [("G1PUBNOSTR", g1_ss58.as_str()), ("HEX", &hex), ("NPUB", &npub), ("LANG", lang), ("NOSTRNS", &nostrns)] {
            write_public(&work.join(name), &format!("{value}\n"))?;
        }
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(&work);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&work, &dir) {
        let _ = std::fs::remove_dir_all(&work);
        return Err(io(&dir, e));
    }
    Ok(Created { email, hex, npub, nsec, g1_v1, g1_ss58, nostrns, pass, salt: Zeroizing::new(salt.to_owned()), pepper: Zeroizing::new(pepper.to_owned()), lang: lang.to_owned(), dir, seed })
}

/// Libellé d'une constellation pour le profil : `ORIGIN` quand `UPLANETNAME` n'est que des zéros.
pub fn constellation_label(uplanet_name: &str) -> &'static str {
    if uplanet_name.bytes().all(|b| b == b'0') { "ORIGIN" } else { "ẐEN" }
}

/// Événements de naissance, signés par le compte : profil (kind 0), suivi du Capitaine (kind 3, règle « les
/// passagers suivent le Capitaine ») et DID W3C (kind 30800, `d = did`).
pub fn birth_events(c: &Created, uplanet_name: &str, captain_hex: Option<&str>, now: u64) -> Result<Vec<Event>, MultipassError> {
    let secret = c.nostr_secret()?;
    let sign = |kind: u32, tags: Vec<Vec<String>>, content: String| UnsignedEvent { created_at: now, kind, tags, content }.sign(&secret).map_err(|e| MultipassError::Derive(e.to_string()));
    let name = c.email.split('@').next().unwrap_or("membre");
    let label = constellation_label(uplanet_name);
    let profile = json!({ "name": name, "about": format!("MULTIPASS UPlanet {label} · DID did:nostr:{}", c.hex), "g1": c.g1_ss58 });
    let mut events = vec![sign(kind::METADATA, vec![vec!["i".into(), format!("g1:{}", c.g1_ss58)]], profile.to_string())?];
    if let Some(captain) = captain_hex.filter(|h| astro_nostr_proto::is_hex64(h) && *h != c.hex) {
        events.push(sign(kind::CONTACTS, vec![vec!["p".into(), captain.to_owned()]], String::new())?);
    }
    let did = json!({
        "@context": ["https://w3id.org/did/v1"],
        "id": format!("did:nostr:{}", c.hex),
        "type": "DIDNostr",
        "alsoKnownAs": [format!("mailto:{}", c.email), format!("did:g1:{}", c.g1_v1), c.nostrns],
        "verificationMethod": [
            { "id": format!("did:nostr:{}#key1", c.hex), "type": "Multikey", "publicKeyMultibase": format!("fe70102{}", c.hex) },
            { "id": format!("did:nostr:{}#g1-key", c.hex), "type": "Ed25519VerificationKey2020", "publicKeyBase58": c.g1_v1 },
        ],
        "metadata": { "email": c.email, "contractStatus": "active_rental", "language": c.lang, "uplanet": label },
    });
    events.push(sign(kind::DID, vec![vec!["d".into(), "did".into()], vec!["t".into(), "uplanet".into()], vec!["t".into(), "did-document".into()]], did.to_string())?);
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{Layout, discover};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("astroportd-multipass-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn emails_become_safe_directory_names() {
        assert_eq!(normalize_email("  Alice+Tag@Exemple.ORG ").unwrap(), "alice+tag@exemple.org");
        for bad in ["", "a", "a@b", "../x@y.z", "a/b@y.z", "a@@y.z", "a b@y.z", "a@y..z", "@y.z", "é@y.z", "a@-y.z", &"a".repeat(300)] {
            assert!(normalize_email(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn random_secrets_are_alphanumeric_and_distinct() {
        let (a, b) = (random_alnum(24).unwrap(), random_alnum(24).unwrap());
        assert_eq!((a.len(), b.len()), (24, 24));
        assert!(a.bytes().all(|c| c.is_ascii_alphanumeric()) && a != b);
        let p = random_pass().unwrap();
        assert!(p.len() == 4 && p.bytes().all(|c| c.is_ascii_digit()), "{p}");
    }

    #[test]
    fn ipns_name_matches_the_k51_form_of_a_peer_id() {
        // Même clé que le vecteur legacy de ylevel.rs : PeerID 12D3KooWRxhg… ⇔ k51qzi5uqu5d….
        let seed = astro_core::kdf::duniter_seed("5cc487d505584c0840d6745b61a43d5ed03cc072922a45541a4d10269f1acc25", "0a3d038212abc881d46e1fffadb6832246b54b3519a644c71a8cda4567c0a3e4").unwrap();
        let name = ipns_name(&seed.ed25519_public());
        assert!(name.starts_with("k51qzi5uqu5d") && name.len() == 62, "{name}");
    }

    #[test]
    fn a_multipass_is_isolated_per_constellation_and_discoverable_by_the_station() {
        let root = tmp("create");
        let nostr = root.join("nostr");
        let zeros = "0".repeat(64);
        let a = create_with(&nostr, "Alice@Exemple.org", &zeros, "fr", "SaltSaltSalt", "PepperPepper").unwrap();
        // Même paire SALT/PEPPER dans une autre constellation : autre clé (isolation par UPlanet).
        let zen = tmp("zen");
        let b = create_with(&zen.join("nostr"), "alice@exemple.org", &"ab".repeat(32), "fr", "SaltSaltSalt", "PepperPepper").unwrap();
        assert_ne!(a.hex, b.hex);
        assert_ne!(a.g1_ss58, b.g1_ss58);
        // La formule isolée = `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)`, vérifiée à la main.
        let salt16 = astro_core::formula::uplanet_salt(&zeros).unwrap();
        let direct = astro_core::kdf::duniter_seed("SaltSaltSalt", &format!("PepperPepper_{salt16}")).unwrap();
        assert_eq!(direct.nostr_keys().unwrap().pubkey_hex(), a.hex);
        assert_eq!(direct.g1_ss58().unwrap(), a.g1_ss58);

        // Fichiers : secrets en 0600, publics lisibles, et la station retrouve le compte.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [".secret.nostr", ".secret.dunikey", ".keyformula", ".pass", ".multipass.json"] {
                assert_eq!(std::fs::metadata(a.dir.join(f)).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
            }
        }
        assert_eq!(std::fs::read_to_string(a.dir.join("HEX")).unwrap().trim(), a.hex);
        assert_eq!(std::fs::read_to_string(a.dir.join("G1PUBNOSTR")).unwrap().trim(), a.g1_ss58);
        assert!(std::fs::read_to_string(a.dir.join("NOSTRNS")).unwrap().starts_with("/ipns/k51"));
        let game = root.join("game");
        std::fs::create_dir_all(&game).unwrap();
        std::os::unix::fs::symlink(&nostr, game.join("nostr")).unwrap();
        // NODE minimal pour la découverte (Y-Level sans toucher à ~/.ssh).
        crate::ylevel::init(&crate::ylevel::Paths { game: game.clone(), ssh: root.join("ssh") }, &crate::ylevel::InitOptions { no_ssh: true, ..Default::default() }, 1).unwrap();
        let found = discover(&Layout::new(&game)).unwrap();
        assert_eq!(found.multipasses.len(), 1);
        assert_eq!((found.multipasses[0].email.as_str(), found.multipasses[0].hex.as_str()), ("alice@exemple.org", a.hex.as_str()));
        assert_eq!(found.multipasses[0].key_formula, Some(KeyFormula::Isolated));
        assert!(found.problems.is_empty(), "{:?}", found.problems);

        // Doublon d'email (casse ignorée) et secrets invalides.
        assert!(matches!(create_with(&nostr, "ALICE@exemple.org", &zeros, "fr", "x", "y"), Err(MultipassError::Exists(_))));
        assert!(matches!(create_with(&nostr, "bob@exemple.org", &zeros, "fr", "pas d'espace", "y"), Err(MultipassError::BadSecrets)));
        // Même SALT/PEPPER sous un autre email, même constellation : conflit d'identité, rien n'est créé.
        assert!(matches!(create_with(&nostr, "bob@exemple.org", &zeros, "fr", "SaltSaltSalt", "PepperPepper"), Err(MultipassError::IdentityConflict(e)) if e == "alice@exemple.org"));
        assert!(!nostr.join("bob@exemple.org").exists());
        assert_eq!(std::fs::read_dir(&nostr).unwrap().count(), 1, "aucun dossier de travail abandonné");
        for d in [root, zen] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn keys_equal_the_legacy_keygen_for_the_isolated_formula() {
        // `keygen -t nostr|duniter|ipfs SaltSaltSalt PepperPepper_<sha256(64 zéros)[:16]>` (Astroport.ONE/tools/keygen).
        let root = tmp("legacy");
        let c = create_with(&root, "legacy@exemple.org", &"0".repeat(64), "fr", "SaltSaltSalt", "PepperPepper").unwrap();
        assert_eq!(c.npub, "npub1hpn2zsnhm9eaqpyzl2f8kaqfprf5xd906mcw86vgx88nmpz63paq0evzae");
        assert_eq!(c.g1_v1, "2sDVU1qTqmuJAAwgetNmHYNfJ3vix3gKGmjUqY1s7VJ9");
        assert!(c.g1_ss58.starts_with('g'));
        let peer = Seed::from_ed25519_seed(*astro_core::kdf::duniter_seed("SaltSaltSalt", &format!("PepperPepper_{}", astro_core::formula::uplanet_salt(&"0".repeat(64)).unwrap())).unwrap().ed25519_secret_seed()).ipfs_peer_id();
        assert_eq!(peer, "12D3KooWBgafD9RB2gAna4T7HV874LTVnNyMyjkoEN6WDXAm1S25");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn birth_events_are_signed_by_the_account_and_describe_it() {
        let root = tmp("events");
        let zeros = "0".repeat(64);
        let c = create(&root, "carol@exemple.org", &zeros, "en").unwrap();
        let captain = "cd".repeat(32);
        let ev = birth_events(&c, &zeros, Some(&captain), 1_800_000_000).unwrap();
        assert_eq!(ev.iter().map(|e| e.kind).collect::<Vec<_>>(), [kind::METADATA, kind::CONTACTS, kind::DID]);
        for e in &ev {
            assert_eq!(e.pubkey, c.hex);
            assert!(e.verify().is_ok());
        }
        assert_eq!(ev[1].tag_value("p"), Some(captain.as_str()));
        let did: serde_json::Value = serde_json::from_str(&ev[2].content).unwrap();
        assert_eq!(did["id"], format!("did:nostr:{}", c.hex));
        assert_eq!((did["metadata"]["email"].as_str(), did["metadata"]["language"].as_str(), did["metadata"]["uplanet"].as_str()), (Some("carol@exemple.org"), Some("en"), Some("ORIGIN")));
        // Sans Capitaine (station sans MULTIPASS Capitaine) : pas de kind 3 ; jamais de suivi de soi-même.
        assert_eq!(birth_events(&c, &zeros, None, 1).unwrap().len(), 2);
        assert_eq!(birth_events(&c, &zeros, Some(&c.hex), 1).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }
}
