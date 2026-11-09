//! `astroportd doctor` : diagnostic de la station, sans rien modifier ni démarrer le démon.
//!
//! Vérifie l'identité locale, les permissions 0600 des secrets et de la base, l'état WAL et l'intégrité de
//! `uplanet.db`, la validité des certificats ACME et l'horloge (garde-fou RTC, prochain 20h12 solaire).
//! Code de sortie 1 si une vérification échoue ; un avertissement n'est pas un échec.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use astro_core::solar::{self, Coordinates};
use astro_lifecycle::discovery::{Layout, discover};
use clap::Args;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

/// Options de `astroportd doctor`.
#[derive(Args)]
pub struct DoctorArgs {
    /// Base `uplanet.db` à examiner.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Dossier `~/.zen/game` (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
    #[arg(long)]
    game_dir: Option<PathBuf>,
    /// Latitude de la station (calcul du 20h12 solaire) ; sinon lue dans la base.
    #[arg(long, allow_hyphen_values = true)]
    lat: Option<f64>,
    /// Longitude de la station.
    #[arg(long, allow_hyphen_values = true)]
    lon: Option<f64>,
    /// Sortie JSON (pour la supervision).
    #[arg(long)]
    json: bool,
}

/// Gravité d'une vérification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Conforme.
    Ok,
    /// À surveiller.
    Warn,
    /// Défaillant.
    Fail,
}

impl Level {
    fn mark(self) -> &'static str {
        match self {
            Self::Ok => "✔",
            Self::Warn => "⚠",
            Self::Fail => "✘",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// Résultat d'une vérification.
#[derive(Debug, Clone)]
pub struct Check {
    /// Sujet.
    pub name: &'static str,
    /// Gravité.
    pub level: Level,
    /// Explication, avec la commande qui corrige quand il y en a une.
    pub detail: String,
}

fn check(name: &'static str, level: Level, detail: impl Into<String>) -> Check {
    Check { name, level, detail: detail.into() }
}

/// Une fenêtre de renouvellement de 30 jours : en deçà, le démon aurait déjà dû renouveler.
const RENEW_WINDOW_DAYS: i64 = 30;

/// Jugement sur un certificat dont la validité finit à `not_after` (secondes UNIX).
pub fn cert_level(not_after: i64, now: i64) -> (Level, String) {
    let days = (not_after - now).div_euclid(86_400);
    if not_after <= now {
        (Level::Fail, format!("expiré depuis {} jour(s)", (now - not_after) / 86_400))
    } else if days < RENEW_WINDOW_DAYS {
        (Level::Warn, format!("expire dans {days} jour(s), sous la fenêtre de renouvellement ({RENEW_WINDOW_DAYS} j) : consulter les journaux du démon"))
    } else {
        (Level::Ok, format!("valide encore {days} jour(s)"))
    }
}

/// Fichiers dont les droits dépassent 0600 : `(chemin, mode)`. Un fichier absent est ignoré.
#[cfg(unix)]
pub fn loose_permissions(paths: &[PathBuf]) -> Vec<(PathBuf, u32)> {
    use std::os::unix::fs::PermissionsExt;
    paths.iter().filter_map(|p| std::fs::metadata(p).ok().map(|m| (p.clone(), m.permissions().mode() & 0o777))).filter(|(_, mode)| mode & 0o077 != 0).collect()
}

/// Sans Unix, pas de modes à vérifier.
#[cfg(not(unix))]
pub fn loose_permissions(_: &[PathBuf]) -> Vec<(PathBuf, u32)> {
    Vec::new()
}

/// Fichiers secrets connus de `game` : clés du NODE et `.secret*` de chaque compte.
fn secret_files(game: &Path) -> Vec<PathBuf> {
    let mut out = vec![game.join("secret.nostr"), game.join("secret.NODE.dunikey")];
    if let Ok(accounts) = std::fs::read_dir(game.join("nostr")) {
        for account in accounts.flatten().filter(|e| e.path().is_dir()) {
            if let Ok(files) = std::fs::read_dir(account.path()) {
                out.extend(files.flatten().filter(|f| f.file_name().to_string_lossy().starts_with(".secret")).map(|f| f.path()));
            }
        }
    }
    out.retain(|p| p.is_file());
    out
}

fn db_checks(db: &Path, now: i64, out: &mut Vec<Check>) -> Option<Connection> {
    if !db.is_file() {
        out.push(check("base", Level::Fail, format!("{} introuvable", db.display())));
        return None;
    }
    astro_db::register_sqlite_vec();
    let conn = match Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            out.push(check("base", Level::Fail, format!("ouverture impossible : {e}")));
            return None;
        }
    };
    let mode: Result<String, _> = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0));
    match mode.as_deref() {
        Ok("wal") => {
            let wal = std::fs::metadata(format!("{}-wal", db.display())).map_or(0, |m| m.len());
            if wal > 256 << 20 {
                out.push(check("base WAL", Level::Warn, format!("journal WAL de {} Mo : points de contrôle en retard (lecteur bloquant ?)", wal >> 20)));
            } else {
                out.push(check("base WAL", Level::Ok, format!("mode WAL, journal de {} Ko", wal >> 10)));
            }
        }
        Ok(other) => out.push(check("base WAL", Level::Fail, format!("mode « {other} » : le démon exige le mode WAL"))),
        Err(e) => out.push(check("base WAL", Level::Fail, e.to_string())),
    }
    let quick: Result<String, _> = conn.query_row("PRAGMA quick_check", [], |r| r.get(0));
    match quick.as_deref() {
        Ok("ok") => {
            let version: String = conn.query_row("SELECT value FROM schema_info WHERE key = 'schema_version'", [], |r| r.get(0)).unwrap_or_else(|_| "inconnue".into());
            out.push(check("intégrité", Level::Ok, format!("quick_check ok, schéma v{version}")));
        }
        Ok(msg) => out.push(check("intégrité", Level::Fail, format!("{msg} : restaurer depuis `astroportd replay` (Nostr et IPFS sont la vérité)"))),
        Err(e) => out.push(check("intégrité", Level::Fail, e.to_string())),
    }
    match astro_ui::acme::stored_certs(&conn) {
        Ok(certs) if certs.is_empty() => out.push(check("certificats", Level::Warn, "aucun certificat ACME stocké (HTTPS non utilisé, ou jamais émis)")),
        Ok(certs) => {
            for (domains, not_after) in certs {
                let (level, text) = cert_level(not_after, now);
                out.push(check("certificat", level, format!("{} : {text}", domains.join(", "))));
            }
        }
        Err(e) => out.push(check("certificats", Level::Fail, e.to_string())),
    }
    Some(conn)
}

fn clock_checks(conn: Option<&Connection>, lat: Option<f64>, lon: Option<f64>, out: &mut Vec<Check>) {
    let now = match solar::system_now_checked() {
        Ok(n) => n,
        Err(e) => {
            out.push(check("horloge", Level::Fail, format!("{e} : régler l'heure (NTP) avant tout prélèvement ou purge")));
            return;
        }
    };
    let (mut lat, mut lon) = (lat, lon);
    if lat.is_none() || lon.is_none() {
        let found = conn.and_then(|c| c.query_row("SELECT lat, lon FROM identities WHERE lat IS NOT NULL AND lon IS NOT NULL ORDER BY (role = 'node') DESC, (role = 'captain') DESC LIMIT 1", [], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))).ok());
        if let Some((a, o)) = found {
            (lat, lon) = (Some(a), Some(o));
        }
    }
    let day = astro_core::week::date_string(now);
    match (lat, lon) {
        (Some(la), Some(lo)) => match Coordinates::new(la, lo).and_then(|c| solar::next_20h12(now, &c)) {
            Ok(t) => out.push(check("horloge solaire", Level::Ok, format!("{day} ; prochain 20h12 solaire : {} {:02}:{:02} UTC (dans {} min)", astro_core::week::date_string(t), t % 86_400 / 3600, t % 3600 / 60, (t - now) / 60))),
            Err(e) => out.push(check("horloge solaire", Level::Fail, e.to_string())),
        },
        _ => out.push(check("horloge solaire", Level::Warn, format!("{day} ; coordonnées inconnues : le 20h12 solaire n'est pas calculable (--lat et --lon)"))),
    }
}

/// Descripteurs de fichiers : chaque tunnel, chaque pair libp2p et chaque client en consomme. Sous 8 192 la station
/// refuse des connexions dès qu'elle partage un peu de GPU.
pub fn nofile_level(limit: u64) -> (Level, String) {
    if limit >= 8192 {
        (Level::Ok, format!("{limit} descripteurs de fichiers"))
    } else {
        (Level::Warn, format!("{limit} descripteurs seulement : `LimitNOFILE=65536` dans [Service] de l'unité astroportd (ou `ulimit -n 65536` avant `serve`)"))
    }
}

/// Limite souple « Max open files » lue dans le texte de `/proc/<pid>/limits`.
pub fn parse_nofile(limits: &str) -> Option<u64> {
    let line = limits.lines().find(|l| l.starts_with("Max open files"))?;
    line.split_whitespace().nth(3)?.parse().ok().or(Some(u64::MAX)) // « unlimited »
}

/// `PasswordAuthentication` de `sshd_config` : la première valeur gagne et rien ne compte après un bloc `Match`.
/// `None` : la directive est absente (le défaut d'OpenSSH est `yes`).
pub fn sshd_password_auth(config: &str) -> Option<bool> {
    for line in config.lines() {
        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
        match (words.next().map(str::to_ascii_lowercase).as_deref(), words.next().map(str::to_ascii_lowercase).as_deref()) {
            (Some("match"), _) => return None,
            (Some("passwordauthentication"), Some(v)) => return Some(v == "yes"),
            _ => {}
        }
    }
    None
}

/// Plafond du journal systemd (`SystemMaxUse`) : sans lui, le journal peut occuper jusqu'à 10 % du disque.
pub fn journald_cap(config: &str) -> Option<String> {
    config.lines().map(|l| l.split('#').next().unwrap_or("").trim()).filter_map(|l| l.strip_prefix("SystemMaxUse")).filter_map(|r| r.trim_start().strip_prefix('=')).map(|v| v.trim().to_owned()).rfind(|v| !v.is_empty())
}

/// Texte de `fichier` et, avant lui, des `*.conf` de `fichier.d/` (l'ordre d'application d'OpenSSH et de systemd).
fn with_dropins(file: &str, dropins_first: bool) -> Option<String> {
    let main = std::fs::read_to_string(file).ok();
    let mut dropins: Vec<_> = std::fs::read_dir(format!("{}.d", file.trim_end_matches(".conf"))).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "conf")).collect();
    dropins.sort();
    let extra: String = dropins.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect::<Vec<_>>().join("\n");
    match (main, extra.is_empty()) {
        (None, true) => None,
        (m, _) => Some(if dropins_first { format!("{extra}\n{}", m.unwrap_or_default()) } else { format!("{}\n{extra}", m.unwrap_or_default()) }),
    }
}

/// Réglages de la machine dont dépend la station et que le démon ne peut ni ne doit changer lui-même : lus, jamais modifiés.
fn system_checks(out: &mut Vec<Check>) {
    if let Some(n) = std::fs::read_to_string("/proc/self/limits").ok().as_deref().and_then(parse_nofile) {
        let (level, detail) = nofile_level(n);
        out.push(check("système/fichiers", level, format!("{detail} (limite de ce shell ; le service a la sienne, voir son unité)")));
    }
    let unit = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/systemd/user/astroportd.service"));
    if let Some(text) = unit.as_ref().and_then(|u| std::fs::read_to_string(u).ok()) {
        let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
        if !user.is_empty() && !Path::new(&format!("/var/lib/systemd/linger/{user}")).exists() {
            out.push(check("système/linger", Level::Warn, "l'unité utilisateur ne démarre qu'avec une session ouverte : `loginctl enable-linger $USER` pour qu'elle démarre au boot"));
        } else if !user.is_empty() {
            out.push(check("système/linger", Level::Ok, "l'unité démarre au boot sans session ouverte"));
        }
        let limit = text.lines().find_map(|l| l.trim().strip_prefix("LimitNOFILE=")).map(str::trim);
        match limit {
            Some(v) => out.push(check("système/unité", Level::Ok, format!("LimitNOFILE={v}"))),
            None => out.push(check("système/unité", Level::Warn, "pas de LimitNOFILE dans l'unité astroportd : la limite par défaut de systemd (1 024 en souple) s'applique à la station")),
        }
    }
    // sshd : mot de passe interdit dès qu'une station partage SSH par ses tunnels (le jumeau SSH de l'identité suffit).
    if let Some(cfg) = with_dropins("/etc/ssh/sshd_config", true) {
        match sshd_password_auth(&cfg) {
            Some(false) => out.push(check("système/sshd", Level::Ok, "PasswordAuthentication no")),
            Some(true) => out.push(check("système/sshd", Level::Warn, "PasswordAuthentication yes : `PasswordAuthentication no` dans sshd_config (clés seules : le tunnel SSH expose ce port à des amis)")),
            None => out.push(check("système/sshd", Level::Warn, "PasswordAuthentication non fixé (défaut d'OpenSSH : yes) : écrire `PasswordAuthentication no` dans sshd_config")),
        }
    }
    if let Some(cfg) = with_dropins("/etc/systemd/journald.conf", false) {
        match journald_cap(&cfg) {
            Some(v) => out.push(check("système/journal", Level::Ok, format!("SystemMaxUse={v}"))),
            None => out.push(check("système/journal", Level::Warn, "journal systemd sans plafond : `SystemMaxUse=500M` dans /etc/systemd/journald.conf.d/ (il peut sinon prendre 10 % du disque)")),
        }
    }
}


/// Exécute le diagnostic.
pub fn run(a: DoctorArgs) -> Result<(), Box<dyn std::error::Error>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let layout = match &a.game_dir {
        Some(dir) => Layout::new(dir),
        None => Layout::from_env()?,
    };
    let game = match &a.game_dir {
        Some(d) => d.clone(),
        None => std::env::var_os("ASTRO_GAME_DIR").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zen/game"))).unwrap_or_default(),
    };
    let mut checks = Vec::new();

    match discover(&layout) {
        Ok(d) => {
            checks.push(check("identité", Level::Ok, format!("NODE {} (PeerID {}), {} MULTIPASS hébergé(s){}", &d.node.nostr_hex[..8], d.node.peer_id, d.multipasses.len(), d.captain_email.as_deref().map(|c| format!(", Capitaine {c}")).unwrap_or_default())));
            for p in &d.problems {
                checks.push(check("identité", Level::Warn, format!("{} : {}", p.path.display(), p.what)));
            }
        }
        Err(e) => checks.push(check("identité", Level::Fail, format!("{e} (--game-dir ou ASTRO_GAME_DIR)"))),
    }

    // Y-Level : SSH ⇔ IPFSNODEID ⇔ Ğ1 ⇔ Nostr. Un lien SSH absent ou différent est signalé, pas bloquant (--no-ssh existe).
    let ssh = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh")).unwrap_or_default();
    for c in astro_lifecycle::ylevel::verify(&astro_lifecycle::ylevel::Paths { game: game.clone(), ssh }, None) {
        if c.name == "NODE" {
            continue; // déjà couvert par « identité »
        }
        let level = if c.ok { Level::Ok } else if c.name == "SSH" { Level::Warn } else { Level::Fail };
        checks.push(check(if c.name == "SSH" { "y-level/ssh" } else { "y-level" }, level, format!("{} : {}", c.name, c.detail)));
    }

    let mut files = secret_files(&game);
    if let Some(db) = &a.db {
        files.extend([db.clone(), PathBuf::from(format!("{}-wal", db.display())), PathBuf::from(format!("{}-shm", db.display()))].into_iter().filter(|p| p.is_file()));
    }
    let loose = loose_permissions(&files);
    if loose.is_empty() {
        checks.push(check("permissions", Level::Ok, format!("{} fichier(s) secret(s) en 0600", files.len())));
    } else {
        let list = loose.iter().map(|(p, m)| format!("{} ({m:04o})", p.display())).collect::<Vec<_>>().join(", ");
        checks.push(check("permissions", Level::Fail, format!("{list} : chmod 600 sur chacun")));
    }

    let conn = match &a.db {
        Some(db) => db_checks(db, now, &mut checks),
        None => {
            checks.push(check("base", Level::Warn, "non examinée (--db <uplanet.db>)"));
            None
        }
    };
    clock_checks(conn.as_ref(), a.lat, a.lon, &mut checks);
    system_checks(&mut checks);

    let fails = checks.iter().filter(|c| c.level == Level::Fail).count();
    if a.json {
        let list: Vec<_> = checks.iter().map(|c| json!({ "check": c.name, "level": c.level.name(), "detail": c.detail })).collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "ok": fails == 0, "checks": list }))?);
    } else {
        for c in &checks {
            println!("{} {:<16} {}", c.level.mark(), c.name, c.detail);
        }
        let warns = checks.iter().filter(|c| c.level == Level::Warn).count();
        println!("\n{} vérification(s), {fails} échec(s), {warns} avertissement(s)", checks.len());
    }
    if fails > 0 { Err(format!("{fails} vérification(s) en échec").into()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    #[test]
    fn certificates_are_judged_against_the_renewal_window() {
        assert_eq!(cert_level(NOW - 2 * DAY, NOW).0, Level::Fail);
        assert_eq!(cert_level(NOW + 10 * DAY, NOW).0, Level::Warn);
        assert_eq!(cert_level(NOW + 29 * DAY + 3600, NOW).0, Level::Warn);
        let (level, text) = cert_level(NOW + 60 * DAY + 5, NOW);
        assert_eq!((level, text.as_str()), (Level::Ok, "valide encore 60 jour(s)"));
    }

    #[cfg(unix)]
    #[test]
    fn only_files_readable_by_others_are_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("astro-doctor-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("nostr/alice@example.org")).unwrap();
        let (good, bad, node) = (dir.join("nostr/alice@example.org/.secret.nostr"), dir.join("nostr/alice@example.org/.secret.dunikey"), dir.join("secret.nostr"));
        for (f, mode) in [(&good, 0o600), (&bad, 0o644), (&node, 0o640)] {
            std::fs::write(f, "x").unwrap();
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        std::fs::write(dir.join("nostr/alice@example.org/notes.txt"), "pas un secret").unwrap();
        let files = secret_files(&dir);
        assert_eq!(files.len(), 3, "{files:?}");
        let mut loose = loose_permissions(&files);
        loose.sort();
        assert_eq!(loose, [(bad, 0o644), (node, 0o640)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_database_that_is_not_in_wal_mode_or_is_missing_fails() {
        let dir = std::env::temp_dir().join(format!("astro-doctor-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut out = Vec::new();
        assert!(db_checks(&dir.join("absente.db"), NOW, &mut out).is_none());
        assert_eq!(out[0].level, Level::Fail);

        let plain = dir.join("delete-mode.db");
        let c = Connection::open(&plain).unwrap();
        c.execute_batch("CREATE TABLE t(x)").unwrap();
        drop(c);
        let mut out = Vec::new();
        db_checks(&plain, NOW, &mut out);
        assert!(out.iter().any(|c| c.name == "base WAL" && c.level == Level::Fail), "{out:?}");

        let good = dir.join("uplanet.db");
        drop(astro_db::open(&good).unwrap());
        let mut out = Vec::new();
        db_checks(&good, NOW, &mut out);
        assert!(out.iter().any(|c| c.name == "base WAL" && c.level == Level::Ok), "{out:?}");
        assert!(out.iter().any(|c| c.name == "intégrité" && c.level == Level::Ok), "{out:?}");
        assert!(out.iter().any(|c| c.name == "certificats" && c.level == Level::Warn), "{out:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_solar_clock_check_reports_the_next_20h12() {
        let mut out = Vec::new();
        clock_checks(None, Some(45.76), Some(4.83), &mut out);
        assert_eq!(out[0].level, Level::Ok, "{out:?}");
        assert!(out[0].detail.contains("prochain 20h12 solaire"));
        let mut out = Vec::new();
        clock_checks(None, None, None, &mut out);
        assert_eq!(out[0].level, Level::Warn);
    }

    #[test]
    fn system_settings_are_read_from_their_text() {
        assert_eq!(nofile_level(1024).0, Level::Warn);
        assert_eq!(nofile_level(65536).0, Level::Ok);
        let limits = "Limit                     Soft Limit           Hard Limit           Units\nMax open files            1024                 524288               files\n";
        assert_eq!(parse_nofile(limits), Some(1024));
        assert_eq!(parse_nofile("Max open files            unlimited            unlimited            files\n"), Some(u64::MAX));
        assert_eq!(parse_nofile("rien"), None);
        // sshd : la première valeur gagne ; après `Match`, on ne juge plus ; commentaires ignorés ; casse indifférente.
        assert_eq!(sshd_password_auth("# PasswordAuthentication yes\nPasswordAuthentication no\nPasswordAuthentication yes\n"), Some(false));
        assert_eq!(sshd_password_auth("passwordauthentication YES\n"), Some(true));
        assert_eq!(sshd_password_auth("Port 22\nMatch User bob\n  PasswordAuthentication no\n"), None, "dans un bloc Match : ne vaut pas pour tous");
        assert_eq!(sshd_password_auth(""), None);
        // journald : la dernière valeur non vide gagne.
        assert_eq!(journald_cap("[Journal]\n#SystemMaxUse=\nSystemMaxUse=200M\nSystemMaxUse=500M # plafond\n").as_deref(), Some("500M"));
        assert_eq!(journald_cap("[Journal]\n#SystemMaxUse=\nSystemKeepFree=2G\n"), None);
    }
}
