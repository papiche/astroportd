//! Détection automatique des identités locales.
//!
//! Disposition lue (celle de la station legacy, `~/.zen/game`) :
//!
//! ```text
//! game/secret.nostr               NODE : NSEC=…; NPUB=…; HEX=…   (clé de signature Nostr de la station)
//! game/secret.NODE.dunikey        NODE : portefeuille machine ed25519 (clé Ğ1 + identité P2P/IPFS)
//! game/players/.current/.player   email du Capitaine
//! game/players/.current/.g1pub    portefeuille d'exploitation du Capitaine
//! game/nostr/<email>/.secret.nostr, .keyformula   un dossier par MULTIPASS hébergé
//! ```
//!
//! Le NODE est obligatoire : sans lui la station n'a pas d'identité. Un MULTIPASS illisible n'arrête pas le démarrage,
//! mais il est **signalé** dans [`Discovered::problems`] (jamais ignoré en silence).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use astro_core::KeyFormula;
use astro_nostr_proto::{is_hex64, pubkey_of};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::dunikey::{Dunikey, DunikeyError};

/// Emplacements des fichiers d'identité.
#[derive(Debug, Clone)]
pub struct Layout {
    game: PathBuf,
}

impl Layout {
    /// `game` : dossier `~/.zen/game`.
    pub fn new(game: impl Into<PathBuf>) -> Self {
        Self { game: game.into() }
    }

    /// Disposition par défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`.
    pub fn from_env() -> Result<Self, DiscoveryError> {
        if let Some(dir) = std::env::var_os("ASTRO_GAME_DIR") {
            return Ok(Self::new(dir));
        }
        let home = std::env::var_os("HOME").ok_or(DiscoveryError::NoHome)?;
        Ok(Self::new(PathBuf::from(home).join(".zen/game")))
    }

    fn node_nostr(&self) -> PathBuf {
        self.game.join("secret.nostr")
    }

    fn node_dunikey(&self) -> PathBuf {
        self.game.join("secret.NODE.dunikey")
    }

    fn current(&self, file: &str) -> PathBuf {
        self.game.join("players/.current").join(file)
    }

    fn nostr_root(&self) -> PathBuf {
        self.game.join("nostr")
    }
}

/// Échec bloquant de la découverte.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiscoveryError {
    /// `HOME` absent et `ASTRO_GAME_DIR` non défini.
    #[error("HOME introuvable : définir ASTRO_GAME_DIR")]
    NoHome,
    /// Fichier obligatoire du NODE absent ou illisible.
    #[error("{path} : {why}")]
    Node {
        /// Fichier concerné.
        path: PathBuf,
        /// Cause.
        why: String,
    },
}

/// Un fichier a été trouvé mais n'est pas utilisable (ou pas sûr) : à montrer à l'opérateur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Fichier ou dossier concerné.
    pub path: PathBuf,
    /// Description.
    pub what: String,
}

/// Identité de la station (Armateur).
pub struct NodeIdentity {
    nostr_secret: Zeroizing<[u8; 32]>,
    /// Clé Nostr publique (64 hex).
    pub nostr_hex: String,
    dunikey: Dunikey,
    /// PeerID IPFS/libp2p dérivé de la clé ed25519 du NODE.
    pub peer_id: String,
    /// Clé publique Ğ1 v1 du portefeuille machine.
    pub g1_pub: String,
}

impl NodeIdentity {
    /// Clé secrète Nostr de la station (signature des événements, attestations).
    pub fn nostr_secret(&self) -> Zeroizing<[u8; 32]> {
        self.nostr_secret.clone()
    }

    /// Graine ed25519 du NODE : identité P2P (remplace `ASTRO_P2P_SEED`) et signature Duniter.
    pub fn p2p_seed(&self) -> Zeroizing<[u8; 32]> {
        self.dunikey.ed25519_seed()
    }

    /// Secret de station (remplace `ASTRO_STATION_SECRET`) : jetons WebDAV et authentification entre stations.
    /// Dérivé de la clé Nostr du NODE avec séparation de domaine, donc stable d'un démarrage à l'autre et propre à la station.
    pub fn station_secret(&self) -> Zeroizing<Vec<u8>> {
        let mut h = Sha256::new();
        h.update(b"astroportd/station-secret/v1");
        h.update(*self.nostr_secret);
        Zeroizing::new(h.finalize().to_vec())
    }
}

/// MULTIPASS hébergé par la station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedMultipass {
    /// Email (nom du dossier).
    pub email: String,
    /// Clé Nostr publique (64 hex), vérifiée contre la clé secrète.
    pub hex: String,
    /// Formule de clés déclarée par `.keyformula`, `None` si absente.
    pub key_formula: Option<KeyFormula>,
    /// Fichier de la clé secrète (lue à la demande, jamais conservée ici).
    pub secret_path: PathBuf,
}

/// Résultat de la découverte.
pub struct Discovered {
    /// Station.
    pub node: NodeIdentity,
    /// Email du Capitaine (`.current/.player`).
    pub captain_email: Option<String>,
    /// Portefeuille d'exploitation du Capitaine (`.current/.g1pub`).
    pub captain_g1pub: Option<String>,
    /// MULTIPASS hébergés, triés par email.
    pub multipasses: Vec<HostedMultipass>,
    /// Anomalies rencontrées.
    pub problems: Vec<Problem>,
}

impl Discovered {
    /// MULTIPASS du Capitaine, s'il est hébergé ici.
    pub fn captain(&self) -> Option<&HostedMultipass> {
        let email = self.captain_email.as_deref()?;
        self.multipasses.iter().find(|m| m.email == email)
    }
}

/// Champs `NOM=valeur` d'un fichier `.secret.nostr` (séparés par `;` ou fin de ligne, `export ` toléré).
fn fields(text: &str) -> HashMap<String, Zeroizing<String>> {
    text.split([';', '\n']).filter_map(|p| p.trim().trim_start_matches("export ").split_once('=')).map(|(k, v)| (k.trim().to_owned(), Zeroizing::new(v.trim().trim_matches(['"', '\'']).to_owned()))).collect()
}

/// Clé secrète et clé publique d'un fichier `.secret.nostr`, avec contrôle de cohérence.
fn read_nostr_secret(path: &Path) -> Result<(Zeroizing<[u8; 32]>, String), String> {
    let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| e.to_string())?);
    let f = fields(&text);
    let nsec = f.get("NSEC").or_else(|| f.get("NODE_NSEC")).ok_or("NSEC absent")?;
    let (hrp, data) = bech32::decode(nsec).map_err(|_| "NSEC illisible")?;
    if hrp.as_str() != "nsec" || data.len() != 32 {
        return Err("NSEC invalide".into());
    }
    let mut secret = Zeroizing::new([0u8; 32]);
    secret.copy_from_slice(&data);
    let hex = pubkey_of(&secret).map_err(|_| "clé secrète invalide")?;
    if let Some(declared) = f.get("HEX") {
        if declared.as_str() != hex || !is_hex64(declared) {
            return Err("HEX ne correspond pas à la clé secrète".into());
        }
    }
    Ok((secret, hex))
}

/// Clé Nostr d'un MULTIPASS, relue à la demande (signature, déchiffrement).
pub fn load_multipass_secret(m: &HostedMultipass) -> Result<Zeroizing<[u8; 32]>, String> {
    let (secret, hex) = read_nostr_secret(&m.secret_path)?;
    (hex == m.hex).then_some(secret).ok_or_else(|| "la clé a changé depuis la découverte".to_owned())
}

#[cfg(unix)]
fn loose_permissions(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.permissions().mode() & 0o077 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn loose_permissions(_: &Path) -> bool {
    false
}

fn node_err(path: &Path, why: impl ToString) -> DiscoveryError {
    DiscoveryError::Node { path: path.to_owned(), why: why.to_string() }
}

/// Découvre toutes les identités locales.
pub fn discover(layout: &Layout) -> Result<Discovered, DiscoveryError> {
    let mut problems = Vec::new();
    let mut check_perm = |path: &Path| {
        if loose_permissions(path) {
            problems.push(Problem { path: path.to_owned(), what: "fichier secret lisible par d'autres utilisateurs (attendu : 0600)".into() });
        }
    };

    let nostr_path = layout.node_nostr();
    let (nostr_secret, nostr_hex) = read_nostr_secret(&nostr_path).map_err(|e| node_err(&nostr_path, e))?;
    check_perm(&nostr_path);
    let dunikey_path = layout.node_dunikey();
    let text = Zeroizing::new(std::fs::read_to_string(&dunikey_path).map_err(|e| node_err(&dunikey_path, e))?);
    let dunikey = Dunikey::parse(&text).map_err(|e: DunikeyError| node_err(&dunikey_path, e))?;
    check_perm(&dunikey_path);
    let peer_id = dunikey.seed().ipfs_peer_id();
    let g1_pub = dunikey.public.clone();
    let node = NodeIdentity { nostr_secret, nostr_hex, dunikey, peer_id, g1_pub };

    let read_line = |file: &str| std::fs::read_to_string(layout.current(file)).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let (captain_email, captain_g1pub) = (read_line(".player"), read_line(".g1pub"));

    let mut multipasses = Vec::new();
    let root = layout.nostr_root();
    match std::fs::read_dir(&root) {
        Ok(entries) => {
            let mut dirs: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
            dirs.sort_by_key(|e| e.file_name());
            for entry in dirs {
                let email = entry.file_name().to_string_lossy().into_owned();
                let dir = entry.path();
                let secret_path = dir.join(".secret.nostr");
                if !secret_path.exists() {
                    continue; // dossier sans identité (cache, résidu) : pas un MULTIPASS
                }
                match read_nostr_secret(&secret_path) {
                    Ok((_, hex)) => {
                        let (formula, warn) = read_formula(&dir.join(".keyformula"));
                        problems.extend(warn.map(|what| Problem { path: dir.join(".keyformula"), what }));
                        if loose_permissions(&secret_path) {
                            problems.push(Problem { path: secret_path.clone(), what: "fichier secret lisible par d'autres utilisateurs (attendu : 0600)".into() });
                        }
                        multipasses.push(HostedMultipass { email, hex, key_formula: formula, secret_path });
                    }
                    Err(why) => problems.push(Problem { path: secret_path, what: why }),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => problems.push(Problem { path: root, what: e.to_string() }),
    }
    Ok(Discovered { node, captain_email, captain_g1pub, multipasses, problems })
}

/// `.keyformula` : `nostr=isolated g1=isolated ipns=isolated`. La formule Nostr fait foi ; sans fichier, formule inconnue.
fn read_formula(path: &Path) -> (Option<KeyFormula>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else { return (None, None) };
    let name = text.split_whitespace().find_map(|w| w.strip_prefix("nostr=")).or_else(|| text.split_whitespace().next());
    match name.map(str::parse::<KeyFormula>) {
        Some(Ok(f)) => (Some(f), None),
        Some(Err(e)) => (None, Some(e.to_string())),
        None => (None, Some("fichier vide".into())),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Arborescence `game/` générée pour les tests : aucune clé réelle n'est lue.
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static N: AtomicU32 = AtomicU32::new(0);

    pub fn nsec(secret: &[u8; 32]) -> String {
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), secret).unwrap()
    }

    pub fn secret_nostr(secret: &[u8; 32]) -> String {
        format!("NSEC={}; NPUB=npub1test; HEX={}\n", nsec(secret), pubkey_of(secret).unwrap())
    }

    pub fn write_secret(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// Station complète : NODE (clés 1 et 2), Capitaine alice, MULTIPASS alice (3) et bob (4).
    pub fn station() -> PathBuf {
        let game = std::env::temp_dir().join(format!("astro-life-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        write_secret(&game.join("secret.nostr"), &secret_nostr(&[1; 32]));
        write_secret(&game.join("secret.NODE.dunikey"), &crate::dunikey::render(&[2; 32]));
        std::fs::create_dir_all(game.join("players/.current")).unwrap();
        std::fs::write(game.join("players/.current/.player"), "alice@example.org\n").unwrap();
        std::fs::write(game.join("players/.current/.g1pub"), "CaptainG1Pub\n").unwrap();
        for (email, n, formula) in [("alice@example.org", 3u8, Some("nostr=isolated g1=isolated ipns=isolated")), ("bob@example.org", 4, None)] {
            write_secret(&game.join("nostr").join(email).join(".secret.nostr"), &secret_nostr(&[n; 32]));
            if let Some(f) = formula {
                std::fs::write(game.join("nostr").join(email).join(".keyformula"), f).unwrap();
            }
        }
        game
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn a_whole_station_is_discovered_without_any_environment_variable() {
        let game = station();
        let d = discover(&Layout::new(&game)).unwrap();
        assert!(d.problems.is_empty(), "{:?}", d.problems);
        assert_eq!(d.node.nostr_hex, pubkey_of(&[1; 32]).unwrap());
        assert_eq!(*d.node.p2p_seed(), [2; 32]);
        assert_eq!(d.node.peer_id, astro_core::Seed::from_ed25519_seed([2; 32]).ipfs_peer_id());
        assert_eq!(d.captain_email.as_deref(), Some("alice@example.org"));
        assert_eq!(d.captain_g1pub.as_deref(), Some("CaptainG1Pub"));
        let emails: Vec<_> = d.multipasses.iter().map(|m| (m.email.as_str(), m.key_formula)).collect();
        assert_eq!(emails, [("alice@example.org", Some(KeyFormula::Isolated)), ("bob@example.org", None)]);
        assert_eq!(d.captain().unwrap().hex, pubkey_of(&[3; 32]).unwrap());
        assert_eq!(*load_multipass_secret(&d.multipasses[1]).unwrap(), [4; 32]);
        std::fs::remove_dir_all(game).unwrap();
    }

    #[test]
    fn the_station_secret_is_stable_distinct_from_the_node_key_and_changes_with_it() {
        let game = station();
        let a = discover(&Layout::new(&game)).unwrap().node.station_secret();
        let b = discover(&Layout::new(&game)).unwrap().node.station_secret();
        assert_eq!(*a, *b);
        assert_eq!(a.len(), 32);
        assert_ne!(a.as_slice(), &[1u8; 32]);
        write_secret(&game.join("secret.nostr"), &secret_nostr(&[9; 32]));
        assert_ne!(*discover(&Layout::new(&game)).unwrap().node.station_secret(), *a);
        std::fs::remove_dir_all(game).unwrap();
    }

    #[test]
    fn a_missing_or_inconsistent_node_identity_is_a_hard_error() {
        let game = station();
        std::fs::remove_file(game.join("secret.NODE.dunikey")).unwrap();
        assert!(matches!(discover(&Layout::new(&game)), Err(DiscoveryError::Node { .. })));
        write_secret(&game.join("secret.NODE.dunikey"), &crate::dunikey::render(&[2; 32]));
        write_secret(&game.join("secret.nostr"), &format!("NSEC={}; HEX={}\n", nsec(&[1; 32]), "ab".repeat(32)));
        let err = discover(&Layout::new(&game)).err().unwrap();
        assert!(err.to_string().contains("HEX ne correspond pas"), "{err}");
        assert!(matches!(discover(&Layout::new(game.join("nulle-part"))), Err(DiscoveryError::Node { .. })));
        std::fs::remove_dir_all(game).unwrap();
    }

    #[test]
    fn a_broken_multipass_is_reported_not_swallowed_and_does_not_stop_the_station() {
        let game = station();
        write_secret(&game.join("nostr/carol@example.org/.secret.nostr"), "NSEC=nsec1cassee; HEX=00\n");
        write_secret(&game.join("nostr/dave@example.org/.secret.nostr"), &secret_nostr(&[6; 32]));
        std::fs::write(game.join("nostr/dave@example.org/.keyformula"), "nostr=inconnue").unwrap();
        std::fs::create_dir_all(game.join("nostr/cache")).unwrap();
        let d = discover(&Layout::new(&game)).unwrap();
        let emails: Vec<_> = d.multipasses.iter().map(|m| m.email.as_str()).collect();
        assert_eq!(emails, ["alice@example.org", "bob@example.org", "dave@example.org"]);
        assert_eq!(d.problems.len(), 2, "{:?}", d.problems);
        assert!(d.problems.iter().any(|p| p.path.ends_with("carol@example.org/.secret.nostr")));
        assert!(d.problems.iter().any(|p| p.path.ends_with("dave@example.org/.keyformula")));
        std::fs::remove_dir_all(game).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_secrets_are_flagged() {
        use std::os::unix::fs::PermissionsExt;
        let game = station();
        std::fs::set_permissions(game.join("nostr/bob@example.org/.secret.nostr"), std::fs::Permissions::from_mode(0o644)).unwrap();
        let d = discover(&Layout::new(&game)).unwrap();
        assert_eq!(d.problems.len(), 1);
        assert!(d.problems[0].what.contains("0600"));
        std::fs::remove_dir_all(game).unwrap();
    }

    #[test]
    fn export_lines_and_node_nsec_spelling_are_accepted() {
        let game = station();
        write_secret(&game.join("secret.nostr"), &format!("export NODE_NSEC=\"{}\"\nexport HEX=\"{}\"\n", nsec(&[1; 32]), pubkey_of(&[1; 32]).unwrap()));
        assert_eq!(discover(&Layout::new(&game)).unwrap().node.nostr_hex, pubkey_of(&[1; 32]).unwrap());
        std::fs::remove_dir_all(game).unwrap();
    }
}
