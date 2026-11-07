//! `astroportd init` (assistant d'installation) et `astroportd ylevel` (vérification de l'identité de la station).
//!
//! `init` enchaîne : horloge → rituel Y-Level (clé SSH = PeerID IPFS = Ğ1 = Nostr) → choix de la constellation
//! (ORIGIN ouverte ou ẐEN privée) → dossiers de la station → unité systemd facultative. Interactif dans un
//! terminal, entièrement pilotable par options (`--yes`) pour les scripts et l'installeur.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use astro_lifecycle::ylevel::{self, InitOptions, Paths, Source};
use clap::{Args, ValueEnum};

/// `UPLANETNAME` d'une constellation ORIGIN (pas de `swarm.key`) : 64 zéros, comme `tools/my.sh`.
pub const ORIGIN_UPLANETNAME: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Constellation à rejoindre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Réseau ouvert, bac à sable : 1 Ẑen = 0,1 Ğ1.
    Origin,
    /// Réseau privé (swarm.key) : 1 Ẑen = 1 €.
    Zen,
}

/// Options de `astroportd init`.
#[derive(Args)]
pub struct InitArgs {
    /// Constellation : `origin` (ouverte) ou `zen` (privée, demande une swarm.key). Demandé si absent (terminal).
    #[arg(long)]
    mode: Option<Mode>,
    /// Email du Capitaine : conservé dans station.conf, et son MULTIPASS est créé à la fin de l'installation (code PASS affiché).
    #[arg(long)]
    email: Option<String>,
    /// Latitude de la station (20h12 solaire vrai).
    #[arg(long, allow_hyphen_values = true, requires = "lon")]
    lat: Option<f64>,
    /// Longitude de la station.
    #[arg(long, allow_hyphen_values = true, requires = "lat")]
    lon: Option<f64>,
    /// Fichier `swarm.key` d'une constellation ẐEN existante (reçu d'un Capitaine).
    #[arg(long, conflicts_with = "found_constellation")]
    swarm_key: Option<PathBuf>,
    /// Fonde SA constellation ẐEN : génère une `swarm.key` neuve (à copier ensuite sur chaque station).
    #[arg(long)]
    found_constellation: bool,
    /// Nom de domaine public : HTTPS automatique (Let's Encrypt) quand les ports 80/443 sont libres. Demandé si absent (terminal).
    #[arg(long)]
    domain: Option<String>,
    /// Dossier de la station (défaut : `~/.zen/astroportd`).
    #[arg(long)]
    home: Option<PathBuf>,
    /// Dossier `~/.zen/game` (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
    #[arg(long)]
    game_dir: Option<PathBuf>,
    /// Dossier SSH (défaut : `~/.ssh`).
    #[arg(long)]
    ssh_dir: Option<PathBuf>,
    /// Ne touche pas aux clés SSH (le lien SSH ⇔ IPFS ne sera pas établi).
    #[arg(long)]
    no_ssh: bool,
    /// Accepte de dériver à nouveau une clé SSH qui est déjà la clé Y de la station : l'identité (IPFS, Ğ1, Nostr) change.
    #[arg(long)]
    rederive_y: bool,
    /// `~/.ipfs/config` de kubo à aligner sur le PeerID de la station (sauvegarde `config.bkp`).
    #[arg(long)]
    kubo_config: Option<PathBuf>,
    /// Fils de calcul de la reconnaissance de visages locale, inscrits dans la commande de démarrage (0 = au choix d'ONNX Runtime).
    #[arg(long, default_value_t = 0)]
    face_threads: usize,
    /// Fils de calcul des embeddings locaux de BRO (bge-m3), inscrits dans la commande de démarrage (0 = au choix d'ONNX Runtime).
    #[arg(long, default_value_t = 0)]
    embed_threads: usize,
    /// Visio entre membres : `relais` (STUN + relais d'appel TURN, recommandé), `stun` (STUN seul) ou `aucune`.
    /// Demandé si absent (terminal) ; défaut : relais.
    #[arg(long, value_parser = ["relais", "stun", "aucune"])]
    visio: Option<String>,
    /// Visio : serveur STUN ou TURN supplémentaire donné aux navigateurs (`stun:hôte:port`, `turn:hôte:port`), répétable.
    #[arg(long = "ice-server")]
    ice_servers: Vec<String>,
    /// Écrit l'unité systemd utilisateur `~/.config/systemd/user/astroportd.service`.
    #[arg(long)]
    systemd: bool,
    /// Aucune question : valeurs par défaut (ORIGIN) et options données.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Rejoint la constellation d'une **station marraine** avec le jeton qu'elle a donné (`astroportd parrainer`) :
    /// constellation, swarm.key (ẐEN, reçue chiffrée) et amorçage P2P sans recopie de fichier.
    #[arg(long, conflicts_with_all = ["swarm_key", "found_constellation"])]
    pub join: Option<String>,
    /// Offre déchiffrée (remplie par l'appelant avant `init`, qui reste synchrone).
    #[arg(skip)]
    pub joined: Option<crate::sponsor::Payload>,
}

/// Options de `astroportd ylevel`.
#[derive(Args)]
pub struct YlevelArgs {
    /// Sort en code 1 si une correspondance échoue.
    #[arg(long)]
    verify: bool,
    /// Dossier `~/.zen/game`.
    #[arg(long)]
    game_dir: Option<PathBuf>,
    /// Dossier SSH.
    #[arg(long)]
    ssh_dir: Option<PathBuf>,
    /// `~/.ipfs/config` de kubo à comparer.
    #[arg(long)]
    kubo_config: Option<PathBuf>,
}

type Res = Result<(), Box<dyn std::error::Error>>;

/// Ce qu'`init` laisse à l'appelant pour créer, dans la foulée, le compte du Capitaine (opération asynchrone).
pub struct InitDone {
    /// Dossier de la station.
    pub home: PathBuf,
    /// Dossier `~/.zen/game`.
    pub game: PathBuf,
    /// Email du Capitaine (vide : à créer plus tard).
    pub captain_email: String,
    /// Adresse de l'interface web, pour la consigne finale.
    pub web_url: String,
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| "HOME introuvable : renseigner --home, --game-dir et --ssh-dir".to_owned())
}

fn game_dir(arg: Option<PathBuf>) -> Result<PathBuf, String> {
    match arg.or_else(|| std::env::var_os("ASTRO_GAME_DIR").map(PathBuf::from)) {
        Some(d) => Ok(d),
        None => Ok(home_dir()?.join(".zen/game")),
    }
}

fn ssh_dir(arg: Option<PathBuf>) -> Result<PathBuf, String> {
    arg.map_or_else(|| home_dir().map(|h| h.join(".ssh")), Ok)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Nom de la machine (`/etc/hostname`, `HOSTNAME`, puis la commande `hostname`) ; `station` à défaut.
pub fn hostname() -> String {
    // `/etc/hostname` (Linux), puis `HOSTNAME`, puis la commande `hostname` (macOS n'a ni l'un ni l'autre sous launchd).
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::process::Command::new("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "station".to_owned())
}

struct Asker {
    interactive: bool,
}

impl Asker {
    fn new(yes: bool) -> Self {
        Self { interactive: !yes && std::io::stdin().is_terminal() }
    }

    /// Pose une question ; rend la réponse (vide = défaut). Sans terminal, rend toujours le défaut.
    fn ask(&self, question: &str, default: &str) -> String {
        if !self.interactive {
            return default.to_owned();
        }
        if default.is_empty() { print!("{question} : ") } else { print!("{question} [{default}] : ") }
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() {
            return default.to_owned();
        }
        let line = line.trim();
        if line.is_empty() { default.to_owned() } else { line.to_owned() }
    }
}

/// État d'un port d'écoute de l'interface web.
#[derive(PartialEq)]
enum Port {
    Free,
    /// Déjà pris (Nginx Proxy Manager, nginx, IPFS…).
    Busy,
    /// Libre mais réservé aux comptes privilégiés (< 1024) : il faut `setcap` ou un autre port.
    Privileged,
}

fn probe_port(port: u16) -> Port {
    match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(_) => Port::Free,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Port::Busy,
        Err(_) => Port::Privileged,
    }
}

/// Port local de repli quand 80/443 sont pris : le reverse proxy (NPM) y renvoie le domaine.
const BEHIND_PROXY_LISTEN: &str = "127.0.0.1:8090";

fn parse_coord(s: &str, name: &str, max: f64) -> Result<f64, String> {
    let v: f64 = s.trim().replace(',', ".").parse().map_err(|_| format!("{name} : « {s} » n'est pas un nombre"))?;
    if v.abs() > max { Err(format!("{name} : {v} hors de [-{max}, {max}]")) } else { Ok(v) }
}

fn write_private(path: &Path, content: &str) -> Res {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(content.as_bytes())?;
    Ok(())
}

/// Lignes `clé = valeur` d'un `station.conf` (absent ou illisible : liste vide).
pub fn read_conf(path: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().filter(|l| !l.trim_start().starts_with('#')).filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())).collect()
}

/// Dernière ligne non vide d'une `swarm.key` : le secret partagé, donc `UPLANETNAME` (comme `UPlanetSharedSecret`).
fn shared_secret(swarm_key: &str) -> Option<String> {
    swarm_key.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_owned)
}

/// `astroportd init`.
pub fn init(a: InitArgs) -> Result<InitDone, Box<dyn std::error::Error>> {
    let ask = Asker::new(a.yes);
    println!("astroportd — installation de la station\n");

    // 0. Horloge : un Raspberry Pi sans pile démarre en 1970.
    if let Err(e) = astro_core::solar::ensure_clock_sane(now()) {
        println!("⚠  Horloge : {e}\n   Les échéances (20h12 solaire) refuseront de tourner. Réglez l'heure (timedatectl set-ntp true).\n");
    }

    // 1. Constellation (imposée par la marraine en cas de parrainage).
    let joined_mode = a.joined.as_ref().map(|j| if j.swarm_key.is_some() { Mode::Zen } else { Mode::Origin });
    if let (Some(want), Some(given)) = (joined_mode, a.mode) {
        if want != given {
            return Err("--mode contredit la constellation de la station marraine : retirez --mode".into());
        }
    }
    let mode = match joined_mode.or(a.mode) {
        Some(m) => m,
        None => {
            println!("Quelle constellation rejoindre ?\n  1) ORIGIN — réseau ouvert, bac à sable pour apprendre (1 Ẑen = 0,1 Ğ1)\n  2) ẐEN    — réseau privé de confiance, réelle (1 Ẑen = 1 €), demande une swarm.key");
            if ask.ask("Votre choix (1 ou 2)", "1") == "2" { Mode::Zen } else { Mode::Origin }
        }
    };
    let home = a.home.clone().map_or_else(|| home_dir().map(|h| h.join(".zen/astroportd")), Ok)?;
    let game = game_dir(a.game_dir.clone())?;
    let ssh = ssh_dir(a.ssh_dir.clone())?;

    // swarm.key avant de toucher aux clés : une erreur ici ne doit rien laisser derrière elle.
    let (uplanetname, swarm_fp) = match mode {
        Mode::Origin => {
            if a.swarm_key.is_some() || a.found_constellation {
                return Err("--swarm-key / --found-constellation vont avec --mode zen : en ORIGIN le réseau est ouvert".into());
            }
            (ORIGIN_UPLANETNAME.to_owned(), None)
        }
        Mode::Zen => {
            let text = if let Some(k) = a.joined.as_ref().and_then(|j| j.swarm_key.clone()) {
                k
            } else if a.found_constellation {
                astro_p2p::generate_swarm_key()?
            } else {
                let path = match a.swarm_key.clone() {
                    Some(p) => p,
                    None => {
                        let p = ask.ask("Chemin de votre swarm.key (reçue d'un Capitaine) — ou « nouvelle » pour fonder votre constellation", "");
                        if p.is_empty() {
                            return Err("ẐEN : swarm.key obligatoire (--swarm-key FICHIER, ou --found-constellation pour fonder la vôtre)".into());
                        }
                        if p == "nouvelle" { PathBuf::new() } else { PathBuf::from(p) }
                    }
                };
                if path.as_os_str().is_empty() { astro_p2p::generate_swarm_key()? } else { std::fs::read_to_string(&path).map_err(|e| format!("{} : {e}", path.display()))? }
            };
            let psk = astro_p2p::parse_swarm_key(&text).map_err(|e| format!("swarm.key invalide : {e}"))?;
            let name = shared_secret(&text).ok_or("swarm.key vide")?;
            write_private(&home.join("swarm.key"), &text)?;
            (name, Some(astro_p2p::fingerprint(&psk)))
        }
    };

    // 2. Identité : rituel Y-Level.
    println!("\n① Rituel Y-Level — la clé SSH, l'identité IPFS, le portefeuille Ğ1 et la clé Nostr de la station ne font qu'un");
    let paths = Paths { game: game.clone(), ssh };
    let mut rederive = a.rederive_y;
    let report = loop {
        let opts = InitOptions { no_ssh: a.no_ssh, comment: format!("astroportd@{}", hostname()), kubo_config: a.kubo_config.clone(), rederive };
        match ylevel::init(&paths, &opts, now()) {
            Err(e @ ylevel::YLevelError::AlreadyY(_)) if !a.yes && !rederive => {
                println!("   ⚠ {e}.\n     Pour garder votre identité, retrouvez `secret.june` (SALT/PEPPER) dans ~/.zen/game ou une sauvegarde.");
                if ask.ask("Dériver quand même une NOUVELLE identité ? (oui/non)", "non") != "oui" {
                    return Err("Y-Level interrompu : identité existante conservée, rien n'a été modifié".into());
                }
                rederive = true;
            }
            other => break other?,
        }
    };
    let how = match report.source {
        Source::SecretJune => "secret.june existant (il fait foi, rien n'est recalculé)",
        Source::SshKey => "dérivée de votre clé SSH existante",
        Source::Fresh => "aucune clé SSH trouvée : une clé d'origine a été créée",
    };
    println!("   Secrets    : {how}");
    println!("   IPFSNODEID : {}", report.peer_id);
    println!("   Ğ1         : {}", report.g1_pubkey);
    println!("   Nostr (HEX): {}", report.nostr_hex);
    if a.no_ssh {
        println!("   SSH        : non touché (--no-ssh) : le lien SSH ⇔ IPFS n'est pas établi");
    } else {
        println!("   SSH        : {}", report.ssh_public);
    }
    for k in &report.kept {
        println!("   ↪ ancienne clé SSH conservée : {}", k.display());
    }

    // 3. Position (20h12 solaire). Un `init` rejoué garde ce qui avait été saisi (station.conf), sauf nouvelle valeur.
    println!("\n② Position de la station (pour le 20h12 solaire vrai)");
    let previous = read_conf(&home.join("station.conf"));
    let prev = |k: &str| previous.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let coords = match a.lat.zip(a.lon) {
        Some(c) => Some(c),
        None if !prev("lat").is_empty() && !prev("lon").is_empty() => parse_coord(&prev("lat"), "latitude", 90.0).ok().zip(parse_coord(&prev("lon"), "longitude", 180.0).ok()),
        None => {
            let (la, lo) = (ask.ask("Latitude (ex. 43.60, vide pour ignorer)", ""), ask.ask("Longitude (ex. 1.44)", ""));
            if la.is_empty() || lo.is_empty() { None } else { Some((parse_coord(&la, "latitude", 90.0)?, parse_coord(&lo, "longitude", 180.0)?)) }
        }
    };
    // Fils de calcul de l'inférence locale : l'option l'emporte, sinon ce que le précédent `init` avait retenu (0 = au choix d'ONNX Runtime).
    let threads = |arg: usize, key: &str| if arg > 0 { arg } else { prev(key).parse().unwrap_or(0) };
    let (face_threads, embed_threads) = (threads(a.face_threads, "face_threads"), threads(a.embed_threads, "embed_threads"));
    let email = a.email.clone().unwrap_or_else(|| ask.ask("Email du Capitaine (son compte est créé à la fin ; vide = plus tard)", &prev("captain_email")));

    // Web : domaine + certificat. Ports 80/443 libres → astroportd gère lui-même HTTPS (ACME) ; pris (NPM…) → il reste local.
    let (p80, p443) = (probe_port(80), probe_port(443));
    let behind_proxy = p80 == Port::Busy || p443 == Port::Busy;
    let domain = a.domain.clone().unwrap_or_else(|| prev("domain"));
    let domain = match (domain.is_empty(), behind_proxy) {
        (true, false) => ask.ask("Nom de domaine public pour HTTPS automatique (vide = HTTP local seulement)", ""),
        // Derrière un proxy, la station doit quand même connaître son adresse publique : balise 30850 (bouton « Rejoindre »),
        // liens envoyés aux membres, NIP-98, relai annoncé.
        (true, true) => ask.ask("Ports 80/443 pris (reverse proxy ?) : nom de domaine public que ce proxy renvoie vers la station (vide = aucun)", ""),
        _ => domain,
    };

    // Visio : la station sert elle-même STUN (et un relais d'appel TURN) sur le port UDP de l'interface ; il faut l'ouvrir
    // sur la box pour que deux membres derrière leurs box s'appellent en direct.
    let ui_port = if behind_proxy { BEHIND_PROXY_LISTEN.rsplit(':').next().unwrap_or("8090").to_owned() } else if domain.is_empty() { "18080".to_owned() } else { "443".to_owned() };
    let visio = match a.visio.clone().or_else(|| Some(prev("visio")).filter(|v| !v.is_empty())) {
        Some(v) => v,
        None if a.yes => "relais".to_owned(),
        None => {
            println!("\n③ Visio entre membres (appels vidéo directs, sans service tiers)");
            println!("  1) STUN + relais d'appel (recommandé) : marche même entre deux réseaux très fermés ; ouvrez le port {ui_port} en UDP sur la box");
            println!("  2) STUN seul : plus léger ; un appel entre deux réseaux très fermés peut échouer ; même port UDP {ui_port}");
            println!("  3) aucune : pas de visio directe servie par cette station");
            match ask.ask("Votre choix (1, 2 ou 3)", "1").as_str() { "2" => "stun", "3" => "aucune", _ => "relais" }.to_owned()
        }
    };
    let ice: Vec<String> = if !a.ice_servers.is_empty() {
        a.ice_servers.clone()
    } else if a.yes || visio == "aucune" {
        prev("ice_servers").split_whitespace().map(str::to_owned).collect()
    } else {
        let given = ask.ask("Serveur STUN/TURN supplémentaire (ex. stun:stun.exemple.org:3478, vide = aucun)", &prev("ice_servers"));
        given.split_whitespace().map(str::to_owned).collect()
    };
    if let Some(bad) = ice.iter().find(|u| !(u.starts_with("stun:") || u.starts_with("turn:") || u.starts_with("turns:"))) {
        return Err(format!("serveur de visio invalide : {bad} (attendu stun:hôte:port ou turn:hôte:port)").into());
    }

    // 4. Dossiers + station.conf.
    // `nostr/` : un dossier par MULTIPASS hébergé (vide au départ ; `serve` l'exige).
    for dir in [home.clone(), home.join("blobs"), home.join("snapshots"), game.join("nostr")] {
        std::fs::create_dir_all(&dir)?;
    }
    let conf = format!(
        "# astroportd — station.conf (écrit par `astroportd init`, relisible et modifiable)\nmode = {}\nuplanetname = {uplanetname}\nipfsnodeid = {}\ncaptain_email = {email}\ndomain = {domain}\nlat = {}\nlon = {}\nface_threads = {face_threads}\nembed_threads = {embed_threads}\nvisio = {visio}\nice_servers = {}\n",
        if mode == Mode::Zen { "zen" } else { "origin" },
        report.peer_id,
        coords.map_or(String::new(), |c| c.0.to_string()),
        coords.map_or(String::new(), |c| c.1.to_string()),
        ice.join(" "),
    );
    let conf = match &a.joined {
        Some(j) => format!("{conf}sponsor = {}\n", j.from),
        None => conf,
    };
    write_private(&home.join("station.conf"), &conf)?;

    // 5. Commande de démarrage.
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("astroportd"));
    let mut serve = vec![
        exe.display().to_string(),
        "serve".into(),
        format!("--db {}", home.join("uplanet.db").display()),
        format!("--blobs {}", home.join("blobs").display()),
        format!("--game-dir {}", game.display()),
        format!("--snapshot-dir {}", home.join("snapshots").display()),
    ];
    let bootstrap = a.joined.as_ref().map(|j| j.bootstrap.clone()).unwrap_or_default();
    if mode == Mode::Zen {
        serve.push(format!("--swarm-key {}", home.join("swarm.key").display()));
    }
    if mode == Mode::Zen || !bootstrap.is_empty() {
        serve.push("--p2p-listen /ip4/0.0.0.0/tcp/4101".into());
    }
    // Parrainage : la station s'amorce sur sa marraine (elle n'est pas seule dans l'essaim dès le premier démarrage).
    for b in &bootstrap {
        serve.push(format!("--p2p-bootstrap {b}"));
    }
    if behind_proxy {
        serve.push(format!("--http-listen {BEHIND_PROXY_LISTEN}"));
        if !domain.is_empty() {
            serve.push(format!("--public-domain {domain}"));
        }
    } else if !domain.is_empty() {
        serve.push(format!("--domain {domain}"));
        if !email.is_empty() {
            serve.push(format!("--acme-email {email}"));
        }
    }
    if let Some((la, lo)) = coords {
        serve.push(format!("--lat {la} --lon {lo}"));
    }
    match visio.as_str() {
        "aucune" => serve.push("--no-stun".into()),
        "stun" => serve.push("--no-turn".into()),
        _ => {}
    }
    for u in &ice {
        serve.push(format!("--ice-server {u}"));
    }
    if face_threads > 0 {
        serve.push(format!("--face-threads {face_threads}"));
    }
    if embed_threads > 0 {
        serve.push(format!("--embed-threads {embed_threads}"));
    }
    // Un uCloud repris d'Astroport.ONE (import-ucloud) n'a ses blobs que dans IPFS : sans --kubo, il devient illisible.
    // ORIGIN seulement : une UPlanet ẐEN n'a jamais de Kubo (étanchéité des constellations).
    if mode == Mode::Origin && crate::legacy_ucloud::count_ipfs_only(&home.join("uplanet.db")).unwrap_or(0) > 0 {
        serve.push("--kubo http://127.0.0.1:5001".into());
    }
    let serve = serve.join(" ");
    if a.systemd {
        let unit = format!("[Unit]\nDescription=astroportd ({})\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart={serve}\nRestart=on-failure\nRestartSec=5\nUMask=0077\n\n[Install]\nWantedBy=default.target\n", if mode == Mode::Zen { "constellation ẐEN" } else { "constellation ORIGIN" });
        let path = home_dir()?.join(".config/systemd/user/astroportd.service");
        write_private(&path, &unit)?;
        println!("\n④ Unité systemd écrite : {}\n   systemctl --user daemon-reload && systemctl --user enable --now astroportd\n   (loginctl enable-linger $USER pour qu'elle démarre sans session ouverte)", path.display());
    }

    println!("\n── Station prête ({}) ──", if mode == Mode::Zen { "constellation ẐEN" } else { "constellation ORIGIN" });
    if let Some(fp) = swarm_fp {
        println!("Empreinte de la swarm.key : {fp}  (deux stations de la même constellation doivent afficher la même)");
        if a.found_constellation {
            println!("Votre constellation est fondée : copiez {} sur chaque station qui la rejoint (--swarm-key).", home.join("swarm.key").display());
        }
    }
    if let Some(j) = &a.joined {
        println!("Marraine :        {} : la station s'amorce sur elle au démarrage ({} adresse(s) P2P)", j.from, j.bootstrap.len());
    }
    println!("Démarrer :        {serve}\nVérifier :        astroportd doctor --game-dir {} && astroportd ylevel --verify", game.display());
    if behind_proxy {
        println!("Web :             les ports 80/443 sont pris (Nginx Proxy Manager ?) : astroportd écoute sur http://{BEHIND_PROXY_LISTEN}/");
        println!("                  dans NPM : Proxy Host {} → http://{BEHIND_PROXY_LISTEN} (WebSockets activés), certificat SSL Let's Encrypt géré par NPM", if domain.is_empty() { "<votre domaine>" } else { &domain });
        println!("                  proxy sur une autre machine : remplacez --http-listen {BEHIND_PROXY_LISTEN} par 0.0.0.0:8090 ; visio : ouvrez le port UDP 8090 sur la box");
    } else if !domain.is_empty() {
        println!("Interface web :   https://{domain}/   (certificat Let's Encrypt obtenu et renouvelé par astroportd ; jeton : astroportd token <email>)");
        println!("                  avant le démarrage : le DNS de {domain} doit pointer sur cette machine, et les ports 80 et 443 être ouverts (box/pare-feu)");
        if p80 == Port::Privileged || p443 == Port::Privileged {
            println!("                  ports < 1024 refusés à un utilisateur : sudo setcap 'cap_net_bind_service=+ep' {}   (ou lancer avec --http-listen/--https-listen sur d'autres ports)", exe.display());
        }
        println!("                  essai sans quota : ajoutez --acme-staging à la commande");
    } else {
        println!("Interface web :   http://127.0.0.1:18080/   (jeton : astroportd token <email>)   — HTTPS : relancez `astroportd init --domain votre.domaine`");
    }
    let web_url = if behind_proxy && !domain.is_empty() {
        format!("https://{domain}/")
    } else if behind_proxy {
        format!("http://{BEHIND_PROXY_LISTEN}/")
    } else if !domain.is_empty() {
        format!("https://{domain}/")
    } else {
        "http://127.0.0.1:18080/".to_owned()
    };
    Ok(InitDone { home, game, captain_email: email, web_url })
}

/// `astroportd ylevel`.
pub fn ylevel_cmd(a: YlevelArgs) -> Res {
    let paths = Paths { game: game_dir(a.game_dir)?, ssh: ssh_dir(a.ssh_dir)? };
    let checks = ylevel::verify(&paths, a.kubo_config.as_deref());
    for c in &checks {
        println!("{} {:<12} {}", if c.ok { "✔" } else { "✘" }, c.name, c.detail);
    }
    if checks.iter().all(|c| c.ok) {
        println!("\nY-Level conforme : IPFSNODEID, Ğ1, Nostr et accès SSH partagent la même racine.");
        Ok(())
    } else if a.verify {
        Err("Y-Level non conforme".into())
    } else {
        println!("\nY-Level non conforme : `astroportd init` le rétablit (les anciennes clés sont conservées).");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_secret_is_the_last_line_of_the_swarm_key() {
        let key = "/key/swarm/psk/1.0.0/\n/base16/\nabcd\n\n";
        assert_eq!(shared_secret(key).as_deref(), Some("abcd"));
        assert_eq!(shared_secret("\n\n"), None);
        assert_eq!(ORIGIN_UPLANETNAME.len(), 64);
    }

    #[test]
    fn station_conf_is_read_back_ignoring_comments() {
        let dir = std::env::temp_dir().join(format!("astroportd-conf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("station.conf");
        std::fs::write(&p, "# c\nmode = zen\nlat = 43.6\ncaptain_email = a@b.c\n").unwrap();
        let c = read_conf(&p);
        assert!(c.contains(&("lat".to_owned(), "43.6".to_owned())) && c.contains(&("mode".to_owned(), "zen".to_owned())));
        assert!(read_conf(&dir.join("absent")).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn coordinates_accept_a_decimal_comma_and_reject_nonsense() {
        assert_eq!(parse_coord("43,60", "latitude", 90.0), Ok(43.6));
        assert!(parse_coord("91", "latitude", 90.0).is_err());
        assert!(parse_coord("nord", "latitude", 90.0).is_err());
    }
}
