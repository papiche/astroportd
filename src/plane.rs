//! **Plans** d'une station : ORIGIN et les UPlanet privées (ZEN1, ZEN2…) servis par **un seul processus**.
//!
//! Un plan = une UPlanet vue de cette machine : sa base (`uplanet.db`), son dossier de comptes, sa clé de réseau privé
//! (`swarm.key`, absente pour ORIGIN), son port. Tous partagent **le même NODE** (même `IPFSNODEID`, mêmes fichiers
//! `secret.nostr` et `secret.NODE.dunikey`) et **le même bassin de blobs** (`astro_dav::pool`), chaque plan déclarant les bases
//! des autres comme pairs du bassin : le disque n'est payé qu'une fois et un blob n'est supprimé que si plus aucun plan ne
//! le référence.
//!
//! Invariants vérifiés ici, avant de démarrer quoi que ce soit (I1 à I6 du plan, `docs/PLAN_DOUBLE_UPLANET.md`) :
//! - **Kubo n'appartient qu'à ORIGIN** : un plan à `swarm.key` qui demande Kubo est refusé ;
//! - **au plus un ORIGIN** (le plan sans `swarm.key`), et deux plans n'ont jamais la même `swarm.key`, ni la même base,
//!   ni le même dossier de comptes, ni le même port : l'étanchéité est **structurelle**, aucune requête ne peut mélanger
//!   deux plans ;
//! - les noms sont des mots simples (`a-z`, `0-9`, `-`) : ils servent de sous-domaine et de nom de dossier.
//!
//! Le fichier de configuration est du JSON (`planes.json`) :
//!
//! ```json
//! { "pool": "~/.zen/astroportd/pool", "game_dir": "~/.zen/game",
//!   "planes": [
//!     { "name": "origin", "dir": "~/.zen/astroportd", "http_listen": "127.0.0.1:18080", "kubo": "http://127.0.0.1:5001" },
//!     { "name": "zen1", "swarm_key": "~/.zen/game/zen1/swarm.key", "http_listen": "127.0.0.1:18081", "p2p_listen": ["/ip4/0.0.0.0/tcp/4101"] }
//!   ] }
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Erreur de configuration des plans.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlaneError {
    /// Configuration illisible.
    #[error("configuration des plans illisible : {0}")]
    Config(String),
    /// Un plan viole une règle.
    #[error("plan « {plane} » : {why}")]
    Rule {
        /// Nom du plan.
        plane: String,
        /// Règle violée.
        why: String,
    },
}

fn rule(plane: &str, why: impl Into<String>) -> PlaneError {
    PlaneError::Rule { plane: plane.to_owned(), why: why.into() }
}

/// Fichier `planes.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanesFile {
    /// Dossier de blobs commun (défaut : `<racine>/pool`).
    pub pool: Option<String>,
    /// Dossier `~/.zen/game` d'ORIGIN et du NODE (défaut : `~/.zen/game`).
    pub game_dir: Option<String>,
    /// Racine des dossiers des plans (défaut : `~/.zen/astroportd`) ; un plan peut préciser le sien (`dir`).
    pub root: Option<String>,
    /// Les plans.
    pub planes: Vec<PlaneSpec>,
}

/// Un plan tel que l'écrit le Capitaine.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaneSpec {
    /// Nom court : `origin`, `zen1`…
    pub name: String,
    /// Dossier du plan (base `uplanet.db`, snapshots) ; défaut : `<racine>/<nom>`. Pour ORIGIN déjà en service, son dossier actuel.
    pub dir: Option<String>,
    /// Dossier `game` du plan (comptes dans `nostr/`) ; défaut : `game_dir` pour ORIGIN, `<game_dir>/<nom>` sinon.
    pub game: Option<String>,
    /// Clé de réseau privé : sa présence fait de ce plan une UPlanet privée ; absente, c'est ORIGIN.
    pub swarm_key: Option<String>,
    /// Écoute HTTP du plan (défaut : `127.0.0.1:18080` + rang).
    pub http_listen: Option<String>,
    /// Écoutes libp2p du plan.
    #[serde(default)]
    pub p2p_listen: Vec<String>,
    /// API de Kubo : **réservé à ORIGIN**.
    pub kubo: Option<String>,
    /// Autres options de `astroportd serve`, telles quelles.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Un plan prêt à servir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane {
    /// Nom.
    pub name: String,
    /// ORIGIN (sans `swarm.key`) ?
    pub origin: bool,
    /// Base `uplanet.db`.
    pub db: PathBuf,
    /// Dossier `game` du plan.
    pub game: PathBuf,
    /// Écoute HTTP.
    pub http: String,
    /// Arguments de `astroportd serve` (sans le nom du programme).
    pub args: Vec<String>,
}

fn expand(p: &str, home: &Path) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if p == "~" => home.to_owned(),
        None => PathBuf::from(p),
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 32 && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !n.starts_with('-') && !n.ends_with('-')
}

/// Lit `planes.json`.
pub fn parse(text: &str) -> Result<PlanesFile, PlaneError> {
    serde_json::from_str(text).map_err(|e| PlaneError::Config(e.to_string()))
}

/// Valide les plans et fabrique les arguments de chacun. `home` : `$HOME`, pour les chemins en `~/`.
pub fn plan(file: &PlanesFile, home: &Path) -> Result<Vec<Plane>, PlaneError> {
    if file.planes.is_empty() {
        return Err(PlaneError::Config("aucun plan".into()));
    }
    let root = file.root.as_deref().map_or_else(|| home.join(".zen/astroportd"), |r| expand(r, home));
    let pool = file.pool.as_deref().map_or_else(|| root.join("pool"), |p| expand(p, home));
    let game_root = file.game_dir.as_deref().map_or_else(|| home.join(".zen/game"), |g| expand(g, home));
    let (mut names, mut dbs, mut games, mut ports, mut keys) = (HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new());
    let mut out = Vec::new();
    let mut origins = 0;
    for (rank, s) in file.planes.iter().enumerate() {
        if !valid_name(&s.name) {
            return Err(rule(&s.name, "nom invalide (lettres minuscules, chiffres et tirets, 32 caractères au plus)"));
        }
        if !names.insert(s.name.clone()) {
            return Err(rule(&s.name, "nom déjà pris"));
        }
        let origin = s.swarm_key.is_none();
        if origin {
            origins += 1;
            if origins > 1 {
                return Err(rule(&s.name, "un seul plan sans `swarm.key` (ORIGIN) par station"));
            }
        }
        if !origin && s.kubo.is_some() {
            return Err(rule(&s.name, "Kubo n'appartient qu'à ORIGIN : une UPlanet privée n'a jamais de Kubo (ses CID ne doivent pas sortir de son essaim)"));
        }
        if let Some(k) = &s.swarm_key {
            if !keys.insert(expand(k, home)) {
                return Err(rule(&s.name, "cette `swarm.key` sert déjà un autre plan : deux plans ne partagent jamais leur réseau privé"));
            }
        }
        let dir = s.dir.as_deref().map_or_else(|| root.join(&s.name), |d| expand(d, home));
        let db = dir.join("uplanet.db");
        if !dbs.insert(db.clone()) {
            return Err(rule(&s.name, format!("la base {} sert déjà un autre plan", db.display())));
        }
        let game = s.game.as_deref().map_or_else(|| if origin { game_root.clone() } else { game_root.join(&s.name) }, |g| expand(g, home));
        if !games.insert(game.clone()) {
            return Err(rule(&s.name, format!("le dossier de comptes {} sert déjà un autre plan", game.display())));
        }
        let http = s.http_listen.clone().unwrap_or_else(|| format!("127.0.0.1:{}", 18_080 + rank));
        if !ports.insert(http.clone()) {
            return Err(rule(&s.name, format!("l'écoute {http} sert déjà un autre plan")));
        }
        let mut args: Vec<String> = ["--db".into(), db.display().to_string(), "--blobs".into(), pool.display().to_string(), "--game-dir".into(), game.display().to_string(), "--http-listen".into(), http.clone()].to_vec();
        if let Some(k) = &s.swarm_key {
            args.extend(["--swarm-key".into(), expand(k, home).display().to_string()]);
        }
        if let Some(k) = &s.kubo {
            args.extend(["--kubo".into(), k.clone()]);
        }
        for l in &s.p2p_listen {
            args.extend(["--p2p-listen".into(), l.clone()]);
        }
        args.extend(s.args.iter().cloned());
        out.push(Plane { name: s.name.clone(), origin, db, game, http, args });
    }
    // Chaque plan déclare les bases des autres comme pairs du bassin commun.
    let all: Vec<PathBuf> = out.iter().map(|p| p.db.clone()).collect();
    for p in &mut out {
        for other in all.iter().filter(|d| **d != p.db) {
            p.args.extend(["--pool-peer".into(), other.display().to_string()]);
        }
    }
    Ok(out)
}

/// Prépare le dossier `game` d'un plan : il reçoit, par **liens symboliques**, les fichiers du NODE d'ORIGIN (`secret.nostr`,
/// `secret.NODE.dunikey`) : tous les plans ont donc le même `IPFSNODEID` et la même clé de NODE. Ne remplace jamais un fichier.
pub fn prepare(plane: &Plane, node_game: &Path) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(plane.game.join("nostr"))?;
    let mut linked = Vec::new();
    if plane.game != node_game {
        for file in ["secret.nostr", "secret.NODE.dunikey"] {
            let (src, dst) = (node_game.join(file), plane.game.join(file));
            if dst.symlink_metadata().is_err() && src.exists() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&src, &dst)?;
                linked.push(file.to_owned());
            }
        }
    }
    if let Some(parent) = plane.db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(linked)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/fred")
    }

    fn file(json: &str) -> PlanesFile {
        parse(json).unwrap()
    }

    fn arg_of(p: &Plane, flag: &str) -> Vec<String> {
        p.args.windows(2).filter(|w| w[0] == flag).map(|w| w[1].clone()).collect()
    }

    #[test]
    fn origin_and_a_private_plane_share_one_pool_and_each_declares_the_other_as_peer() {
        let planes = plan(
            &file(r#"{ "planes": [ { "name": "origin", "dir": "~/.zen/astroportd", "kubo": "http://127.0.0.1:5001" }, { "name": "zen1", "swarm_key": "~/.zen/game/zen1/swarm.key", "http_listen": "127.0.0.1:18081", "p2p_listen": ["/ip4/0.0.0.0/tcp/4101"] } ] }"#),
            &home(),
        )
        .unwrap();
        let (o, z) = (&planes[0], &planes[1]);
        assert!(o.origin && !z.origin);
        assert_eq!(o.db, PathBuf::from("/home/fred/.zen/astroportd/uplanet.db"), "ORIGIN garde sa base actuelle : aucune migration");
        assert_eq!(z.db, PathBuf::from("/home/fred/.zen/astroportd/zen1/uplanet.db"));
        assert_eq!((arg_of(o, "--blobs"), arg_of(z, "--blobs")), (vec!["/home/fred/.zen/astroportd/pool".to_owned()], vec!["/home/fred/.zen/astroportd/pool".to_owned()]), "un seul bassin");
        assert_eq!(arg_of(o, "--pool-peer"), [z.db.display().to_string()]);
        assert_eq!(arg_of(z, "--pool-peer"), [o.db.display().to_string()]);
        assert_eq!(arg_of(o, "--game-dir"), ["/home/fred/.zen/game"]);
        assert_eq!(arg_of(z, "--game-dir"), ["/home/fred/.zen/game/zen1"]);
        assert_eq!(arg_of(o, "--http-listen"), ["127.0.0.1:18080"]);
        assert_eq!((arg_of(o, "--kubo").len(), arg_of(z, "--kubo").len()), (1, 0));
        assert_eq!(arg_of(z, "--p2p-listen"), ["/ip4/0.0.0.0/tcp/4101"]);
        assert_eq!(arg_of(z, "--swarm-key"), ["/home/fred/.zen/game/zen1/swarm.key"]);
    }

    #[test]
    fn a_private_plane_never_gets_kubo_and_planes_never_overlap() {
        let e = |json: &str| plan(&file(json), &home()).unwrap_err().to_string();
        assert!(e(r#"{ "planes": [ { "name": "zen1", "swarm_key": "/k1", "kubo": "http://127.0.0.1:5001" } ] }"#).contains("Kubo n'appartient qu'à ORIGIN"));
        assert!(e(r#"{ "planes": [ { "name": "a" }, { "name": "b" } ] }"#).contains("un seul plan sans `swarm.key`"));
        assert!(e(r#"{ "planes": [ { "name": "z1", "swarm_key": "/k" }, { "name": "z2", "swarm_key": "/k" } ] }"#).contains("ne partagent jamais leur réseau privé"));
        assert!(e(r#"{ "planes": [ { "name": "z1", "swarm_key": "/k1", "dir": "/d" }, { "name": "z2", "swarm_key": "/k2", "dir": "/d" } ] }"#).contains("sert déjà un autre plan"));
        assert!(e(r#"{ "planes": [ { "name": "z1", "swarm_key": "/k1", "http_listen": "127.0.0.1:1" }, { "name": "z2", "swarm_key": "/k2", "http_listen": "127.0.0.1:1" } ] }"#).contains("l'écoute 127.0.0.1:1"));
        assert!(e(r#"{ "planes": [ { "name": "Zen 1", "swarm_key": "/k" } ] }"#).contains("nom invalide"));
        assert!(e(r#"{ "planes": [ { "name": "z", "swarm_key": "/k" }, { "name": "z", "swarm_key": "/k2" } ] }"#).contains("nom déjà pris"));
        assert!(e(r#"{ "planes": [] }"#).contains("aucun plan"));
        assert!(parse(r#"{ "planes": [ { "name": "z", "kubo_url": "x" } ] }"#).is_err(), "une faute de frappe dans la configuration est refusée");
    }

    #[test]
    fn a_zen_plane_only_station_needs_no_origin_and_no_kubo() {
        let planes = plan(&file(r#"{ "planes": [ { "name": "zen1", "swarm_key": "/k1" }, { "name": "zen2", "swarm_key": "/k2" } ] }"#), &home()).unwrap();
        assert!(planes.iter().all(|p| !p.origin && arg_of(p, "--kubo").is_empty()));
        assert_eq!(arg_of(&planes[1], "--http-listen"), ["127.0.0.1:18081"]);
    }

    #[test]
    fn the_node_files_are_linked_never_copied_and_never_replaced() {
        let dir = std::env::temp_dir().join(format!("astroportd-plane-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let node = dir.join("game");
        std::fs::create_dir_all(&node).unwrap();
        std::fs::write(node.join("secret.nostr"), "NSEC=…").unwrap();
        std::fs::write(node.join("secret.NODE.dunikey"), "pub: …").unwrap();
        let p = Plane { name: "zen1".into(), origin: false, db: dir.join("zen1/uplanet.db"), game: node.join("zen1"), http: "127.0.0.1:18081".into(), args: Vec::new() };
        assert_eq!(prepare(&p, &node).unwrap(), ["secret.nostr", "secret.NODE.dunikey"]);
        assert_eq!(std::fs::read_link(p.game.join("secret.nostr")).unwrap(), node.join("secret.nostr"), "même clé de NODE : même IPFSNODEID");
        assert!(p.game.join("nostr").is_dir() && dir.join("zen1").is_dir());
        assert!(prepare(&p, &node).unwrap().is_empty(), "rejouable : rien n'est refait");
        std::fs::remove_file(p.game.join("secret.nostr")).unwrap();
        std::fs::write(p.game.join("secret.nostr"), "propre à ce plan").unwrap();
        prepare(&p, &node).unwrap();
        assert_eq!(std::fs::read_to_string(p.game.join("secret.nostr")).unwrap(), "propre à ce plan", "un fichier existant n'est jamais remplacé");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
