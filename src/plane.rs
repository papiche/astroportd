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
//!     { "name": "origin", "dir": "~/.zen/astroportd", "http_listen": "127.0.0.1:8090", "kubo": "http://127.0.0.1:5001" },
//!     { "name": "zen1", "swarm_key": "~/.zen/game/zen1/swarm.key", "http_listen": "127.0.0.1:8091", "p2p_listen": ["/ip4/0.0.0.0/tcp/4101"] }
//!   ] }
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanesFile {
    /// Dossier de blobs commun (défaut : `<racine>/pool`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Dossier `~/.zen/game` d'ORIGIN et du NODE (défaut : `~/.zen/game`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_dir: Option<String>,
    /// Racine des dossiers des plans (défaut : `~/.zen/astroportd`) ; un plan peut préciser le sien (`dir`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Les plans.
    pub planes: Vec<PlaneSpec>,
    /// HTTPS par la station elle-même pour tous ses plans (ports 80 et 443, certificat Let's Encrypt qui couvre le domaine de
    /// chaque plan) : plus besoin de reverse proxy. Absent : chaque plan écoute en clair sur son port (derrière un proxy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https: Option<HttpsFront>,
}

/// Réglages du front HTTPS des plans (`"https": { "email": "…" }` dans `planes.json`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpsFront {
    /// Contact Let's Encrypt (prévenu avant l'expiration d'un certificat) ; défaut : `captain_email` du `station.conf` d'ORIGIN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Autorité de test de Let's Encrypt (certificats non reconnus, quotas larges) : pour essayer sans risquer le quota.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staging: bool,
}

/// Un plan tel que l'écrit le Capitaine.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlaneSpec {
    /// Nom court : `origin`, `zen1`…
    pub name: String,
    /// Dossier du plan (base `uplanet.db`, snapshots) ; défaut : `<racine>/<nom>`. Pour ORIGIN déjà en service, son dossier actuel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Dossier `game` du plan (comptes dans `nostr/`) ; défaut : `game_dir` pour ORIGIN, `<game_dir>/<nom>` sinon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    /// Clé de réseau privé : sa présence fait de ce plan une UPlanet privée ; absente, c'est ORIGIN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm_key: Option<String>,
    /// Écoute HTTP du plan (défaut : `127.0.0.1:8090` + rang).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_listen: Option<String>,
    /// Écoutes libp2p du plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub p2p_listen: Vec<String>,
    /// API de Kubo : **réservé à ORIGIN**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kubo: Option<String>,
    /// Autres options de `astroportd serve`, telles quelles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    // Maillage actif par défaut : un plan sans écoute libp2p reçoit le premier port libre à partir de 4101, jamais celui
    // d'un autre plan (deux plans ne partagent ni réseau privé ni port).
    let mut p2p_used: HashSet<u16> = HashSet::new();
    for s in &file.planes {
        for port in s.p2p_listen.iter().filter_map(|l| port_of(l)) {
            if !p2p_used.insert(port) {
                return Err(rule(&s.name, format!("le port libp2p {port} sert déjà un autre plan")));
            }
        }
    }
    let mut next_p2p = 4_101u16;
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
        let http = s.http_listen.clone().unwrap_or_else(|| format!("127.0.0.1:{}", 8_090 + rank));
        if !ports.insert(http.clone()) {
            return Err(rule(&s.name, format!("l'écoute {http} sert déjà un autre plan")));
        }
        let mut args: Vec<String> = ["--db".into(), db.display().to_string(), "--blobs".into(), pool.display().to_string(), "--game-dir".into(), game.display().to_string(), "--http-listen".into(), http.clone()].to_vec();
        if let Some(k) = &s.swarm_key {
            args.extend(["--swarm-key".into(), expand(k, home).display().to_string()]);
        }
        // Les cartes des stations d'une constellation ne se mêlent pas à celles d'une autre : ORIGIN garde `~/.zen/tmp/swarm`
        // (celui qu'Astroport.ONE lit aussi), chaque autre plan a le sien.
        if !origin && !s.args.iter().any(|x| x == "--swarm-dir" || x.starts_with("--swarm-dir=")) {
            args.extend(["--swarm-dir".into(), dir.join("swarm").display().to_string()]);
        }
        if let Some(k) = &s.kubo {
            args.extend(["--kubo".into(), k.clone()]);
        }
        for l in &s.p2p_listen {
            args.extend(["--p2p-listen".into(), l.clone()]);
        }
        if s.p2p_listen.is_empty() && !s.args.iter().any(|x| x == "--no-p2p") {
            while p2p_used.contains(&next_p2p) {
                next_p2p = next_p2p.checked_add(1).ok_or_else(|| rule(&s.name, "aucun port libp2p libre"))?;
            }
            p2p_used.insert(next_p2p);
            args.extend(["--p2p-listen".into(), format!("/ip4/0.0.0.0/tcp/{next_p2p}")]);
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

/// Retire `--plane NOM` (ou `--plane=NOM`) des arguments : rend le nom demandé et le reste.
pub fn take_plane_flag(raw: Vec<String>) -> (Option<String>, Vec<String>) {
    let (mut name, mut rest, mut it) = (None, Vec::new(), raw.into_iter());
    while let Some(a) = it.next() {
        if a == "--plane" {
            name = it.next();
        } else if let Some(v) = a.strip_prefix("--plane=") {
            name = Some(v.to_owned());
        } else {
            rest.push(a);
        }
    }
    (name, rest)
}

/// Options que `--plane` ajoute à une commande, **sans jamais remplacer** une option déjà donnée. Pour `serve`, toute la configuration du plan
/// (`full`) : base, blobs, comptes, écoutes, `swarm.key`, pairs du bassin ; pour les autres commandes, seulement `--db`, `--blobs`, `--game-dir`
/// et `--swarm-key` (les commandes qui n'en connaissent pas certaines les ignorent : voir [`without_flag`]).
pub fn inject(raw: &[String], plane: &Plane, full: bool) -> Vec<String> {
    let given = |flag: &str| raw.iter().any(|a| a == flag || a.starts_with(&format!("{flag}=")));
    let mut out = raw.to_vec();
    let mut i = 0;
    while i < plane.args.len() {
        let flag = plane.args[i].as_str();
        let value = plane.args.get(i + 1).cloned();
        i += 2;
        let wanted = full || matches!(flag, "--db" | "--blobs" | "--game-dir" | "--swarm-key");
        // `--pool-peer` et `--p2p-listen` se répètent ; les autres sont uniques.
        let repeatable = matches!(flag, "--pool-peer" | "--p2p-listen");
        if wanted && (repeatable || !given(flag)) && !(repeatable && given(flag) && !full) {
            if let Some(v) = value {
                out.extend([flag.to_owned(), v]);
            }
        }
    }
    out
}

/// `raw` sans l'option `flag` ni sa valeur (pour les commandes qui ne la connaissent pas).
pub fn without_flag(raw: &[String], flag: &str) -> Vec<String> {
    let (mut out, mut skip) = (Vec::new(), false);
    for a in raw {
        if skip {
            skip = false;
        } else if a == flag {
            skip = true;
        } else if !a.starts_with(&format!("{flag}=")) {
            out.push(a.clone());
        }
    }
    out
}

/// Plans servis par CE processus (renseigné par `astroportd planes`) ; vide quand le démon est lancé par `serve`.
static RUNNING: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Note les plans servis par ce processus.
pub fn set_running(names: Vec<String>) {
    let _ = RUNNING.set(names);
}

/// Plans servis par ce processus (vide : un seul plan, lancé par `serve`).
pub fn running() -> &'static [String] {
    RUNNING.get().map_or(&[], Vec::as_slice)
}

static FRONT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Note que ce processus route par nom d'hôte (`planes --front`) : l'interface peut alors proposer le changement de plan.
pub fn set_front(on: bool) {
    let _ = FRONT.set(on);
}

/// Ce processus route-t-il par nom d'hôte ?
pub fn front() -> bool {
    FRONT.get().copied().unwrap_or(false)
}

/// Dossier de blobs du plan dont la base est `db`, d'après `planes.json` s'il existe (le bassin commun des plans) ; `None` sans
/// fichier, ou si aucun plan n'a cette base. Pour qu'une commande hors `serve` (`pull-webdav`) écrive là où le plan lit.
pub fn blobs_for_db(home: &Path, db: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(default_path(home)).ok()?;
    let planes = plan(&parse(&text).ok()?, home).ok()?;
    let want = std::fs::canonicalize(db).unwrap_or_else(|_| db.to_owned());
    let p = planes.iter().find(|p| std::fs::canonicalize(&p.db).unwrap_or_else(|_| p.db.clone()) == want)?;
    p.args.windows(2).find(|w| w[0] == "--blobs").map(|w| PathBuf::from(&w[1]))
}

/// Chemin de `planes.json` par défaut.
pub fn default_path(home: &Path) -> PathBuf {
    home.join(".zen/astroportd/planes.json")
}

/// Configuration actuelle : le fichier s'il existe, sinon **la station telle qu'elle tourne aujourd'hui** (un seul plan ORIGIN, dans son
/// dossier actuel) : ajouter un premier plan ne déplace donc rien. Le booléen dit si le fichier existait.
pub fn load(path: &Path, home: &Path) -> Result<(PlanesFile, bool), PlaneError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok((parse(&text)?, true)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let origin = PlaneSpec { name: "origin".into(), dir: Some("~/.zen/astroportd".into()), game: None, swarm_key: None, http_listen: Some("127.0.0.1:8090".into()), p2p_listen: Vec::new(), kubo: Some(crate::init::DEFAULT_KUBO.into()), args: Vec::new() };
            let _ = home;
            Ok((PlanesFile { pool: None, game_dir: None, root: None, planes: vec![origin], https: None }, false))
        }
        Err(e) => Err(PlaneError::Config(format!("{} : {e}", path.display()))),
    }
}

/// Écrit `planes.json` (écriture atomique : fichier voisin puis renommage).
pub fn save(path: &Path, file: &PlanesFile) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(file).map_err(std::io::Error::other)? + "\n")?;
    std::fs::rename(&tmp, path)
}

/// Un plan à ajouter.
#[derive(Debug, Clone, Default)]
pub struct NewPlane {
    /// Nom court.
    pub name: String,
    /// Contenu d'une `swarm.key` reçue d'un Capitaine ; `None` : en créer une neuve (on fonde cette UPlanet).
    pub swarm_key: Option<String>,
    /// Écoute HTTP ; défaut : la première libre à partir de `127.0.0.1:8090`.
    pub http_listen: Option<String>,
    /// Port libp2p ; défaut : le premier libre à partir de 4101.
    pub p2p_port: Option<u16>,
}

/// Ce que l'ajout a fait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// Fichier de la clé de réseau privé du plan.
    pub swarm_key_path: PathBuf,
    /// Contenu de la clé **si elle vient d'être créée** : à copier sur chaque station qui rejoindra cette UPlanet.
    pub created_key: Option<String>,
    /// Écoute HTTP choisie.
    pub http_listen: String,
    /// Écoute libp2p choisie.
    pub p2p_listen: String,
}

fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit([':', '/']).next().and_then(|p| p.parse().ok())
}

/// Ajoute un plan à `file` : nom, clé de réseau privé (créée ou reçue, écrite en 0600 sous `<game>/<nom>/swarm.key`, jamais écrasée),
/// ports libres. L'ensemble est **validé comme au démarrage** ; en cas de refus, rien n'est laissé sur le disque.
pub fn add(file: &mut PlanesFile, new: &NewPlane, home: &Path) -> Result<Added, PlaneError> {
    let name = new.name.trim();
    let game_root = file.game_dir.as_deref().map_or_else(|| home.join(".zen/game"), |g| expand(g, home));
    let key_path = game_root.join(name).join("swarm.key");
    if !valid_name(name) {
        return Err(rule(name, "nom invalide (lettres minuscules, chiffres et tirets, 32 caractères au plus)"));
    }
    if file.planes.iter().any(|p| p.name == name) {
        return Err(rule(name, "nom déjà pris"));
    }
    if key_path.exists() {
        return Err(rule(name, format!("{} existe déjà : on n'écrase jamais une clé de réseau privé", key_path.display())));
    }
    let used_http: HashSet<u16> = file.planes.iter().enumerate().filter_map(|(i, p)| p.http_listen.as_deref().and_then(port_of).or(u16::try_from(8_090 + i).ok())).collect();
    let used_p2p: HashSet<u16> = file.planes.iter().flat_map(|p| p.p2p_listen.iter().filter_map(|a| port_of(a))).collect();
    let free = |start: u16, used: &HashSet<u16>| (start..start + 200).find(|p| !used.contains(p));
    let http = new.http_listen.clone().or_else(|| free(8_090, &used_http).map(|p| format!("127.0.0.1:{p}"))).ok_or_else(|| rule(name, "aucun port HTTP libre"))?;
    let p2p_port = new.p2p_port.or_else(|| free(4_101, &used_p2p)).ok_or_else(|| rule(name, "aucun port libp2p libre"))?;
    let p2p = format!("/ip4/0.0.0.0/tcp/{p2p_port}");
    let (text, created) = match &new.swarm_key {
        Some(t) => (t.trim().to_owned() + "\n", None),
        None => {
            let t = astro_p2p::generate_swarm_key().map_err(|e| rule(name, e.to_string()))?;
            (t.clone(), Some(t))
        }
    };
    astro_p2p::parse_swarm_key(&text).map_err(|e| rule(name, format!("swarm.key illisible : {e}")))?;
    let spec = PlaneSpec { name: name.to_owned(), dir: None, game: None, swarm_key: Some(key_path.display().to_string()), http_listen: Some(http.clone()), p2p_listen: vec![p2p.clone()], kubo: None, args: Vec::new() };
    let mut candidate = file.clone();
    candidate.planes.push(spec.clone());
    plan(&candidate, home)?; // mêmes règles qu'au démarrage, avant d'écrire quoi que ce soit
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(key_path.parent().unwrap_or(&game_root))?;
        std::fs::write(&key_path, &text)?;
        #[cfg(unix)]
        std::fs::set_permissions(&key_path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        Ok(())
    };
    write().map_err(|e| rule(name, format!("clé non écrite : {e}")))?;
    file.planes.push(spec);
    Ok(Added { swarm_key_path: key_path, created_key: created, http_listen: http, p2p_listen: p2p })
}

/// Retire un plan de la configuration. **Ne supprime aucune donnée** (base, comptes, clé) ; ORIGIN n'est jamais retiré.
pub fn remove(file: &mut PlanesFile, name: &str) -> Result<(), PlaneError> {
    let i = file.planes.iter().position(|p| p.name == name).ok_or_else(|| rule(name, "plan inconnu"))?;
    if file.planes[i].swarm_key.is_none() {
        return Err(rule(name, "ORIGIN est le plan de base de la station : il n'est pas retiré (pour ne servir que ẐEN : « Basculer en ẐEN »)"));
    }
    file.planes.remove(i);
    Ok(())
}

/// Où garder la description d'ORIGIN pendant une bascule en ẐEN seul (à côté de `planes.json`, pour que celui-ci reste lisible
/// par les versions qui refusent les champs inconnus).
pub fn paused_origin_path(planes_path: &Path) -> PathBuf {
    planes_path.with_file_name("planes.origin-paused.json")
}

/// ORIGIN mis en pause par [`stop_origin`] : sa description, et la constellation ẐEN qui a repris son adresse HTTP.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PausedOrigin {
    /// Description d'ORIGIN (ports figés).
    pub origin: PlaneSpec,
    /// Plan qui a repris l'adresse HTTP d'ORIGIN.
    pub heir: Option<String>,
    /// Adresse HTTP qu'avait ce plan avant, rendue par [`restore_origin`].
    pub heir_http: Option<String>,
    /// Écoutes libp2p qu'avait ce plan avant, rendues par [`restore_origin`].
    #[serde(default)]
    pub heir_p2p: Vec<String>,
}

/// Écrit dans chaque plan ses ports **effectifs** (HTTP et libp2p) : un port par défaut dépend du rang du plan, et retirer ou
/// remettre ORIGIN décalerait les rangs, donc les ports (une constellation changerait de port P2P et ses adresses de relais
/// deviendraient fausses).
fn freeze(file: &mut PlanesFile, home: &Path) -> Result<(), PlaneError> {
    let planes = plan(file, home)?;
    for (spec, p) in file.planes.iter_mut().zip(planes) {
        spec.http_listen = Some(p.http.clone());
        if spec.p2p_listen.is_empty() && !spec.args.iter().any(|x| x == "--no-p2p") {
            // Les écoutes ajoutées par défaut précèdent les arguments libres du plan.
            let auto = &p.args[..p.args.len() - spec.args.len()];
            spec.p2p_listen = auto.windows(2).filter(|w| w[0] == "--p2p-listen").map(|w| w[1].clone()).collect();
        }
    }
    Ok(())
}

/// **Bascule en ẐEN** : ORIGIN n'est plus servi au prochain démarrage et la constellation `heir` (la seule constellation ẐEN
/// si `None`) **prend sa place** : son adresse HTTP (le DNS et le proxy mènent à elle) et son port libp2p (la redirection de
/// la box sert telle quelle). Les autres plans gardent leurs ports. Rien n'est effacé : base, comptes et dossier d'ORIGIN
/// restent sur le disque.
pub fn stop_origin(file: &mut PlanesFile, home: &Path, heir: Option<&str>) -> Result<PausedOrigin, PlaneError> {
    let Some(i) = file.planes.iter().position(|p| p.swarm_key.is_none()) else { return Err(rule("origin", "ORIGIN n'est déjà plus servi")) };
    let zen: Vec<String> = file.planes.iter().filter(|p| p.swarm_key.is_some()).map(|p| p.name.clone()).collect();
    let heir = match (heir, zen.as_slice()) {
        (_, []) => return Err(rule("origin", "ajoutez d'abord une constellation ẐEN (sa swarm.key) : sans elle, la station ne servirait plus rien")),
        (Some(h), _) if zen.iter().any(|z| z == h) => h.to_owned(),
        (Some(h), _) => return Err(rule(h, "pas une constellation ẐEN de cette station")),
        (None, [one]) => one.clone(),
        (None, _) => return Err(rule("origin", format!("plusieurs constellations ẐEN ({}) : choisissez celle qui reprend l'adresse d'ORIGIN", zen.join(", ")))),
    };
    freeze(file, home)?;
    let origin = file.planes.remove(i);
    let h = file.planes.iter_mut().find(|p| p.name == heir).ok_or_else(|| rule(&heir, "plan inconnu"))?;
    let heir_http = std::mem::replace(&mut h.http_listen, origin.http_listen.clone());
    // Une station sans P2P côté ORIGIN (`--no-p2p`) n'a pas de port à céder : l'héritière garde le sien.
    let heir_p2p = if origin.p2p_listen.is_empty() { h.p2p_listen.clone() } else { std::mem::replace(&mut h.p2p_listen, origin.p2p_listen.clone()) };
    plan(file, home)?;
    Ok(PausedOrigin { origin, heir: Some(heir), heir_http, heir_p2p })
}

/// Remet ORIGIN en service, en tête, avec ses ports d'avant ; la constellation qui avait repris son adresse HTTP retrouve la
/// sienne. Sans description gardée : celle d'une station neuve. Refusé si ORIGIN est déjà servi.
pub fn restore_origin(file: &mut PlanesFile, home: &Path, kept: Option<PausedOrigin>) -> Result<(), PlaneError> {
    if file.planes.iter().any(|p| p.swarm_key.is_none()) {
        return Err(rule("origin", "ORIGIN est déjà servi"));
    }
    freeze(file, home)?;
    let kept = kept.filter(|k| k.origin.swarm_key.is_none());
    let spec = kept.as_ref().map_or_else(|| PlaneSpec { name: "origin".into(), dir: Some("~/.zen/astroportd".into()), game: None, swarm_key: None, http_listen: Some("127.0.0.1:8090".into()), p2p_listen: Vec::new(), kubo: Some(crate::init::DEFAULT_KUBO.into()), args: Vec::new() }, |k| k.origin.clone());
    if file.planes.iter().any(|p| p.name == spec.name) {
        return Err(rule(&spec.name, "un plan porte déjà ce nom"));
    }
    if let Some(k) = &kept {
        if let Some(h) = file.planes.iter_mut().find(|p| Some(&p.name) == k.heir.as_ref()) {
            if h.http_listen == spec.http_listen {
                h.http_listen.clone_from(&k.heir_http);
            }
            if !spec.p2p_listen.is_empty() && h.p2p_listen == spec.p2p_listen {
                h.p2p_listen.clone_from(&k.heir_p2p);
            }
        }
    }
    file.planes.insert(0, spec);
    plan(file, home)?;
    Ok(())
}

/// État d'un plan pour l'affichage.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlaneInfo {
    /// Nom.
    pub name: String,
    /// ORIGIN ?
    pub origin: bool,
    /// Écoute HTTP.
    pub http: String,
    /// Base.
    pub db: String,
    /// La base existe déjà (le plan a déjà tourné).
    pub db_exists: bool,
    /// Servi par ce processus.
    pub running: bool,
    /// Domaine public du plan ([`public_domain`]).
    pub domain: Option<String>,
}

/// Décrit les plans de `file` (sans rien créer).
pub fn describe(file: &PlanesFile, home: &Path) -> Result<Vec<PlaneInfo>, PlaneError> {
    let served = running();
    Ok(plan(file, home)?.into_iter().map(|p| PlaneInfo { running: served.contains(&p.name) || (served.is_empty() && p.origin), db_exists: p.db.exists(), domain: public_domain(&p), name: p.name, origin: p.origin, http: p.http, db: p.db.display().to_string() }).collect())
}

/// `station.conf` d'un plan : celui de son dossier (là où `serve` le lit, à côté de sa base).
pub fn conf_of(p: &Plane) -> PathBuf {
    p.db.parent().map_or_else(|| PathBuf::from("station.conf"), |d| d.join("station.conf"))
}

/// Domaine public d'un plan : `--public-domain` de ses arguments, sinon `domain` de son `station.conf` (réglé depuis son cockpit
/// ou depuis l'écran des constellations). Un plan écoute toujours en clair sur son port : ce domaine lui arrive par le front
/// HTTPS des plans ou par un reverse proxy.
pub fn public_domain(p: &Plane) -> Option<String> {
    let mut it = p.args.iter();
    while let Some(a) = it.next() {
        let v = if a == "--public-domain" { it.next().cloned() } else { a.strip_prefix("--public-domain=").map(str::to_owned) };
        if let Some(d) = v.and_then(|v| crate::station_domain::normalize_domain(&v).ok()) {
            return Some(d);
        }
    }
    crate::station_domain::domain_from_conf(&conf_of(p)).map(|(d, _)| d)
}

/// Domaines servis par le front, `(domaine, plan)`. Deux plans ne partagent jamais un domaine : le plan est connu par le nom
/// d'hôte **avant** toute authentification.
pub fn front_domains(planes: &[Plane]) -> Result<Vec<(String, String)>, PlaneError> {
    let mut out: Vec<(String, String)> = Vec::new();
    for p in planes {
        if let Some(d) = public_domain(p) {
            if let Some((_, other)) = out.iter().find(|(x, _)| *x == d) {
                return Err(rule(&p.name, format!("le domaine {d} sert déjà le plan « {other} »")));
            }
            out.push((d, p.name.clone()));
        }
    }
    Ok(out)
}

/// Règle (ou retire, `None`) le domaine public du plan `name` : ligne `domain` de son `station.conf`, prise au redémarrage.
pub fn set_domain(file: &PlanesFile, home: &Path, name: &str, domain: Option<&str>) -> Result<Option<String>, PlaneError> {
    let planes = plan(file, home)?;
    let p = planes.iter().find(|p| p.name == name).ok_or_else(|| PlaneError::Config(format!("plan « {name} » inconnu")))?;
    if p.args.iter().any(|a| a == "--public-domain" || a.starts_with("--public-domain=")) {
        return Err(rule(name, "son domaine est fixé par `--public-domain` dans planes.json : changez-le là"));
    }
    let domain = domain.map(str::trim).filter(|d| !d.is_empty()).map(crate::station_domain::normalize_domain).transpose().map_err(|e| rule(name, e))?;
    if let Some(d) = &domain
        && let Some(other) = planes.iter().filter(|o| o.name != name).find(|o| public_domain(o).as_deref() == Some(d))
    {
        return Err(rule(name, format!("le domaine {d} sert déjà le plan « {} »", other.name)));
    }
    let conf = conf_of(p);
    if let Some(dir) = conf.parent() {
        std::fs::create_dir_all(dir).map_err(|e| PlaneError::Config(format!("{} : {e}", dir.display())))?;
    }
    crate::station_domain::set_conf_value(&conf, "domain", domain.as_deref().unwrap_or("")).map_err(|e| PlaneError::Config(format!("{} : {e}", conf.display())))?;
    Ok(domain)
}

static FRONT_TLS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Note que ce processus sert lui-même HTTPS pour ses plans (`planes.json` → `https`).
pub fn set_front_tls(on: bool) {
    let _ = FRONT_TLS.set(on);
}

/// Ce processus sert-il HTTPS pour ses plans ? (Le cockpit ne parle alors ni de reverse proxy ni de ports pris.)
pub fn front_tls() -> bool {
    FRONT_TLS.get().copied().unwrap_or(false)
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
    // Le bassin de blobs commun existe dès que le premier plan est préparé.
    if let Some(pool) = plane.args.windows(2).find(|w| w[0] == "--blobs").map(|w| &w[1]) {
        std::fs::create_dir_all(pool)?;
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
            &file(r#"{ "planes": [ { "name": "origin", "dir": "~/.zen/astroportd", "kubo": "http://127.0.0.1:5001" }, { "name": "zen1", "swarm_key": "~/.zen/game/zen1/swarm.key", "http_listen": "127.0.0.1:8091", "p2p_listen": ["/ip4/0.0.0.0/tcp/4101"] } ] }"#),
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
        assert_eq!(arg_of(o, "--http-listen"), ["127.0.0.1:8090"]);
        assert_eq!((arg_of(o, "--kubo").len(), arg_of(z, "--kubo").len()), (1, 0));
        assert_eq!(arg_of(z, "--p2p-listen"), ["/ip4/0.0.0.0/tcp/4101"]);
        assert_eq!(arg_of(o, "--p2p-listen"), ["/ip4/0.0.0.0/tcp/4102"], "maillage actif par défaut, sur un port que nul autre plan ne prend");
        assert_eq!(arg_of(z, "--swarm-key"), ["/home/fred/.zen/game/zen1/swarm.key"]);
    }

    #[test]
    fn each_plane_gets_its_own_mesh_port_unless_it_opts_out() {
        let planes = plan(&file(r#"{ "planes": [ { "name": "origin" }, { "name": "zen1", "swarm_key": "/k1" }, { "name": "zen2", "swarm_key": "/k2", "args": ["--no-p2p"] }, { "name": "zen3", "swarm_key": "/k3", "p2p_listen": ["/ip4/0.0.0.0/tcp/4102"] } ] }"#), &home()).unwrap();
        let ports: Vec<Vec<String>> = planes.iter().map(|p| arg_of(p, "--p2p-listen")).collect();
        assert_eq!(ports, [vec!["/ip4/0.0.0.0/tcp/4101".to_owned()], vec!["/ip4/0.0.0.0/tcp/4103".to_owned()], vec![], vec!["/ip4/0.0.0.0/tcp/4102".to_owned()]]);
        let e = plan(&file(r#"{ "planes": [ { "name": "origin", "p2p_listen": ["/ip4/0.0.0.0/tcp/4101"] }, { "name": "zen1", "swarm_key": "/k1", "p2p_listen": ["/ip4/0.0.0.0/tcp/4101"] } ] }"#), &home()).unwrap_err().to_string();
        assert!(e.contains("4101"), "{e}");
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
    fn origin_can_be_paused_for_zen_and_restored_exactly() {
        let home = Path::new("/h");
        let mut f = file(r#"{ "planes": [ { "name": "origin", "dir": "/o", "kubo": "http://127.0.0.1:5001", "args": ["--lat", "43.6"] } ] }"#);
        assert!(stop_origin(&mut f, home, None).unwrap_err().to_string().contains("constellation ẐEN"), "seul : la station ne servirait plus rien");
        // Ports par défaut (rang) : ORIGIN 8090 / 4101, zen1 8091 / 4102.
        let mut f = file(r#"{ "planes": [ { "name": "origin", "dir": "/o", "kubo": "http://127.0.0.1:5001", "args": ["--lat", "43.6"] }, { "name": "zen1", "swarm_key": "/k1" } ] }"#);
        let before = plan(&f, home).unwrap();
        assert_eq!((before[1].http.as_str(), arg_of(&before[1], "--p2p-listen")), ("127.0.0.1:8091", vec!["/ip4/0.0.0.0/tcp/4102".to_owned()]));
        let kept = stop_origin(&mut f, home, None).unwrap();
        let after = plan(&f, home).unwrap();
        assert_eq!(after.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["zen1"]);
        assert_eq!(after[0].http, "127.0.0.1:8090", "zen1 reprend l'adresse HTTP d'ORIGIN : DNS et proxy inchangés");
        assert_eq!(arg_of(&after[0], "--p2p-listen"), ["/ip4/0.0.0.0/tcp/4101"], "et son port P2P : la redirection 4101 de la box sert telle quelle");
        assert!(stop_origin(&mut f, home, None).is_err(), "déjà arrêté");
        restore_origin(&mut f, home, Some(kept)).unwrap();
        let back = plan(&f, home).unwrap();
        assert_eq!(back.iter().map(|p| (p.name.as_str(), p.http.as_str())).collect::<Vec<_>>(), [("origin", "127.0.0.1:8090"), ("zen1", "127.0.0.1:8091")], "chacun retrouve son adresse");
        assert_eq!((arg_of(&back[0], "--p2p-listen"), arg_of(&back[1], "--p2p-listen")), (vec!["/ip4/0.0.0.0/tcp/4101".to_owned()], vec!["/ip4/0.0.0.0/tcp/4102".to_owned()]));
        assert_eq!((f.planes[0].dir.as_deref(), f.planes[0].args.clone()), (Some("/o"), vec!["--lat".to_owned(), "43.6".to_owned()]), "ORIGIN remis tel quel");
        assert!(restore_origin(&mut f, home, None).is_err(), "déjà servi");
        // Plusieurs constellations ẐEN : il faut dire laquelle reprend l'adresse.
        let mut g = file(r#"{ "planes": [ { "name": "origin" }, { "name": "zen1", "swarm_key": "/k1" }, { "name": "zen2", "swarm_key": "/k2" } ] }"#);
        assert!(stop_origin(&mut g, home, None).unwrap_err().to_string().contains("choisissez"));
        assert!(stop_origin(&mut g, home, Some("origin")).is_err(), "ORIGIN n'est pas une héritière");
        stop_origin(&mut g, home, Some("zen2")).unwrap();
        let zen = plan(&g, home).unwrap();
        assert_eq!(zen.iter().map(|p| (p.name.as_str(), p.http.as_str())).collect::<Vec<_>>(), [("zen1", "127.0.0.1:8091"), ("zen2", "127.0.0.1:8090")]);
        assert_eq!((arg_of(&zen[0], "--p2p-listen"), arg_of(&zen[1], "--p2p-listen")), (vec!["/ip4/0.0.0.0/tcp/4102".to_owned()], vec!["/ip4/0.0.0.0/tcp/4101".to_owned()]), "seule l'héritière change de port");
    }

    #[test]
    fn a_zen_plane_only_station_needs_no_origin_and_no_kubo() {
        let planes = plan(&file(r#"{ "planes": [ { "name": "zen1", "swarm_key": "/k1" }, { "name": "zen2", "swarm_key": "/k2" } ] }"#), &home()).unwrap();
        assert!(planes.iter().all(|p| !p.origin && arg_of(p, "--kubo").is_empty()));
        assert_eq!(arg_of(&planes[1], "--http-listen"), ["127.0.0.1:8091"]);
    }

    #[test]
    fn adding_a_plane_creates_or_takes_the_key_picks_free_ports_and_refuses_what_the_startup_would_refuse() {
        let home = std::env::temp_dir().join(format!("astroportd-planeadd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let path = default_path(&home);
        let (mut file, existed) = load(&path, &home).unwrap();
        assert!(!existed && file.planes.len() == 1 && file.planes[0].name == "origin", "sans fichier : la station actuelle, ORIGIN seul");
        // Fonder une UPlanet : clé neuve, ports libres après ORIGIN (8090).
        let a = add(&mut file, &NewPlane { name: "zen1".into(), ..NewPlane::default() }, &home).unwrap();
        assert_eq!((a.http_listen.as_str(), a.p2p_listen.as_str()), ("127.0.0.1:8091", "/ip4/0.0.0.0/tcp/4101"));
        let key = a.created_key.clone().unwrap();
        assert_eq!(std::fs::read_to_string(&a.swarm_key_path).unwrap(), key);
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&a.swarm_key_path).unwrap().permissions()) & 0o777, 0o600, "clé de réseau en 0600");
        // Rejoindre une UPlanet existante avec la clé reçue ; ports suivants.
        let other = astro_p2p::generate_swarm_key().unwrap();
        let b = add(&mut file, &NewPlane { name: "zen2".into(), swarm_key: Some(other.clone()), ..NewPlane::default() }, &home).unwrap();
        assert_eq!((b.http_listen.as_str(), b.p2p_listen.as_str(), b.created_key), ("127.0.0.1:8092", "/ip4/0.0.0.0/tcp/4102", None));
        assert_eq!(std::fs::read_to_string(&b.swarm_key_path).unwrap().trim(), other.trim());
        // Refus : doublon, nom invalide, clé illisible (rien n'est écrit), clé existante jamais écrasée.
        assert!(add(&mut file, &NewPlane { name: "zen1".into(), ..NewPlane::default() }, &home).unwrap_err().to_string().contains("nom déjà pris"));
        assert!(add(&mut file, &NewPlane { name: "Zen 3".into(), ..NewPlane::default() }, &home).unwrap_err().to_string().contains("nom invalide"));
        let bad = add(&mut file, &NewPlane { name: "zen3".into(), swarm_key: Some("pas une clé".into()), ..NewPlane::default() }, &home).unwrap_err().to_string();
        assert!(bad.contains("swarm.key illisible") && !home.join(".zen/game/zen3").exists(), "{bad}");
        assert_eq!(file.planes.len(), 3);
        // Enregistrement puis relecture identiques, et description.
        save(&path, &file).unwrap();
        let (again, existed) = load(&path, &home).unwrap();
        assert!(existed);
        assert_eq!(again.planes.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["origin", "zen1", "zen2"]);
        let info = describe(&again, &home).unwrap();
        assert_eq!((info[0].origin, info[0].running, info[1].running), (true, true, false), "hors `planes`, seul ORIGIN est servi");
        // Retrait : ORIGIN jamais ; la clé et les données restent.
        assert!(remove(&mut file, "origin").unwrap_err().to_string().contains("plan de base"));
        remove(&mut file, "zen2").unwrap();
        assert!(b.swarm_key_path.exists() && file.planes.len() == 2);
        assert!(remove(&mut file, "zen9").unwrap_err().to_string().contains("plan inconnu"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn plane_flag_is_taken_out_and_its_paths_are_added_without_replacing_what_was_given() {
        let planes = plan(&file(r#"{ "planes": [ { "name": "origin", "dir": "/o" }, { "name": "zen1", "swarm_key": "/k1", "dir": "/z", "p2p_listen": ["/ip4/0.0.0.0/tcp/4101"] } ] }"#), &home()).unwrap();
        let z = &planes[1];
        let argv = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(take_plane_flag(argv(&["doctor", "--plane", "zen1", "-x"])), (Some("zen1".into()), argv(&["doctor", "-x"])));
        assert_eq!(take_plane_flag(argv(&["doctor", "--plane=zen1"])).0.as_deref(), Some("zen1"));
        assert_eq!(take_plane_flag(argv(&["doctor"])).0, None);
        let short = inject(&argv(&["pool-status"]), z, false);
        assert_eq!(short, argv(&["pool-status", "--db", "/z/uplanet.db", "--blobs", "/home/fred/.zen/astroportd/pool", "--game-dir", "/home/fred/.zen/game/zen1", "--swarm-key", "/k1"]));
        let given = inject(&argv(&["pool-status", "--db", "/autre.db"]), z, false);
        assert!(given.windows(2).any(|w| w == ["--db", "/autre.db"]) && !given.contains(&"/z/uplanet.db".to_owned()), "une option donnée n'est jamais remplacée");
        let full = inject(&argv(&["serve"]), z, true);
        assert!(full.windows(2).any(|w| w == ["--pool-peer", "/o/uplanet.db"]) && full.windows(2).any(|w| w == ["--p2p-listen", "/ip4/0.0.0.0/tcp/4101"]) && full.contains(&"--http-listen".to_owned()), "serve reçoit toute la configuration du plan");
        assert_eq!(without_flag(&short, "--swarm-key"), argv(&["pool-status", "--db", "/z/uplanet.db", "--blobs", "/home/fred/.zen/astroportd/pool", "--game-dir", "/home/fred/.zen/game/zen1"]));
    }

    #[test]
    fn the_node_files_are_linked_never_copied_and_never_replaced() {
        let dir = std::env::temp_dir().join(format!("astroportd-plane-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let node = dir.join("game");
        std::fs::create_dir_all(&node).unwrap();
        std::fs::write(node.join("secret.nostr"), "NSEC=…").unwrap();
        std::fs::write(node.join("secret.NODE.dunikey"), "pub: …").unwrap();
        let p = Plane { name: "zen1".into(), origin: false, db: dir.join("zen1/uplanet.db"), game: node.join("zen1"), http: "127.0.0.1:8091".into(), args: Vec::new() };
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

    #[test]
    fn each_plane_has_its_own_public_domain_and_two_planes_never_share_one() {
        let dir = std::env::temp_dir().join(format!("astroportd-plane-dom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = file(&format!(r#"{{ "root": "{}", "planes": [ {{ "name": "origin" }}, {{ "name": "zen1", "swarm_key": "/k1" }}, {{ "name": "zen2", "swarm_key": "/k2", "args": ["--public-domain", "Zen2.Exemple.org"] }} ] }}"#, dir.display()));
        let planes = plan(&f, &home()).unwrap();
        assert_eq!(public_domain(&planes[2]).as_deref(), Some("zen2.exemple.org"), "--public-domain des arguments, normalisé");
        assert_eq!(public_domain(&planes[1]), None);
        // Réglé depuis l'interface : écrit dans le station.conf du plan, relu par `serve` et par le front.
        assert_eq!(set_domain(&f, &home(), "zen1", Some("https://zen1.exemple.org/")).unwrap().as_deref(), Some("zen1.exemple.org"));
        assert_eq!(public_domain(&planes[1]).as_deref(), Some("zen1.exemple.org"));
        assert_eq!(front_domains(&planes).unwrap(), [("zen1.exemple.org".to_owned(), "zen1".to_owned()), ("zen2.exemple.org".to_owned(), "zen2".to_owned())]);
        assert!(set_domain(&f, &home(), "origin", Some("zen1.exemple.org")).unwrap_err().to_string().contains("sert déjà le plan « zen1 »"));
        assert!(set_domain(&f, &home(), "zen2", Some("autre.org")).unwrap_err().to_string().contains("--public-domain"));
        assert!(set_domain(&f, &home(), "zen1", Some("pas un domaine")).is_err());
        // Retiré : plus de domaine.
        assert_eq!(set_domain(&f, &home(), "zen1", None).unwrap(), None);
        assert_eq!(public_domain(&planes[1]), None);
        // Le bloc `https` de planes.json se relit et se réécrit à l'identique.
        let g = parse(r#"{ "https": { "email": "cap@exemple.org" }, "planes": [ { "name": "origin" } ] }"#).unwrap();
        assert_eq!(g.https, Some(HttpsFront { email: Some("cap@exemple.org".into()), staging: false }));
        assert_eq!(serde_json::to_value(&g).unwrap()["https"], serde_json::json!({ "email": "cap@exemple.org" }));
        let _ = std::fs::remove_dir_all(dir);
    }
}
