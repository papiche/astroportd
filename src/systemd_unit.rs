//! Unité systemd utilisateur de la station (`~/.config/systemd/user/astroportd.service`) : `serve` (une constellation) ou
//! `planes` (ORIGIN et des UPlanet privées dans un seul processus).
//!
//! `astroportd init --systemd` choisit le mode selon la présence de `planes.json` ; `plane-setup add` bascule l'unité existante de
//! `serve` à `planes` (sans quoi le nouveau plan ne démarrerait jamais). Seule la ligne `ExecStart` (et la description) est
//! réécrite : le reste de l'unité, que le Capitaine a pu modifier, est conservé. L'ancienne unité est sauvegardée à côté.

use std::path::{Path, PathBuf};

/// Mode de lancement de la station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `astroportd serve` : une constellation.
    Serve,
    /// `astroportd planes` : tous les plans de `planes.json`.
    Planes,
}

/// Options de `serve` que `planes` pose lui-même depuis `planes.json` : jamais reportées dans `args` d'un plan.
const MANAGED: &[&str] = &["--db", "--blobs", "--game-dir", "--snapshot-dir", "--http-listen", "--swarm-key", "--swarm-dir", "--p2p-listen", "--kubo", "--pool-peer"];

/// Chemin de l'unité d'un utilisateur.
pub fn unit_path(user_home: &Path) -> PathBuf {
    user_home.join(".config/systemd/user/astroportd.service")
}

/// Mode voulu : `planes` dès que `planes.json` existe.
pub fn detect_mode(planes_json: &Path) -> Mode {
    if planes_json.is_file() { Mode::Planes } else { Mode::Serve }
}

/// Ligne `ExecStart` de `planes` ; `--config` seulement si le fichier n'est pas à son emplacement par défaut.
pub fn planes_exec(exe: &Path, config: Option<&Path>) -> String {
    match config {
        Some(c) => format!("{} planes --config {}", exe.display(), c.display()),
        None => format!("{} planes", exe.display()),
    }
}

/// Description de l'unité pour les plans donnés.
pub fn planes_description(names: &[String]) -> String {
    format!("astroportd (plans : {})", names.join(", "))
}

/// Unité complète.
pub fn render(description: &str, exec: &str) -> String {
    format!("[Unit]\nDescription={description}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart={exec}\nRestart=on-failure\nRestartSec=5\nUMask=0077\nLimitNOFILE=65536\n\n[Install]\nWantedBy=default.target\n")
}

/// Commande de la ligne `ExecStart`.
pub fn exec_of(unit: &str) -> Option<&str> {
    unit.lines().find_map(|l| l.trim().strip_prefix("ExecStart=")).map(str::trim)
}

/// Mode de l'unité d'après son `ExecStart` ; `None` si ce n'est pas une commande `astroportd serve|planes`.
pub fn mode_of(unit: &str) -> Option<Mode> {
    let mut words = exec_of(unit)?.split_whitespace();
    let exe = words.next()?;
    if !exe.rsplit('/').next()?.starts_with("astroportd") {
        return None;
    }
    match words.next()? {
        "serve" => Some(Mode::Serve),
        "planes" => Some(Mode::Planes),
        _ => None,
    }
}

/// Options propres à la station dans un `ExecStart` de `serve` (`--lat 43.6 --no-stun`…), sans celles que `planes` gère.
/// `None` si la ligne contient des guillemets : on ne devine pas le découpage de systemd.
pub fn serve_extras(exec: &str) -> Option<Vec<String>> {
    if exec.contains(['"', '\'', '\\']) {
        return None;
    }
    let words: Vec<&str> = exec.split_whitespace().skip_while(|w| *w != "serve").skip(1).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let flag = words[i];
        let value = words.get(i + 1).filter(|v| !v.starts_with("--"));
        let skip = if value.is_some() { 2 } else { 1 };
        let name = flag.split('=').next().unwrap_or(flag);
        if !MANAGED.contains(&name) {
            out.extend(words[i..i + skip].iter().map(|w| (*w).to_owned()));
        }
        i += skip;
    }
    Some(out)
}

/// Remplace `ExecStart` (et `Description`, si donnée) en gardant tout le reste.
pub fn rewrite(unit: &str, exec: &str, description: Option<&str>) -> String {
    let mut out: Vec<String> = unit
        .lines()
        .map(|l| match l.trim_start() {
            t if t.starts_with("ExecStart=") => format!("ExecStart={exec}"),
            t if description.is_some() && t.starts_with("Description=") => format!("Description={}", description.unwrap_or_default()),
            _ => l.to_owned(),
        })
        .collect();
    out.push(String::new());
    out.join("\n")
}

/// Résultat de [`switch_to_planes`].
#[derive(Debug, PartialEq, Eq)]
pub enum Switch {
    /// Pas d'unité : rien à faire (la station n'est pas lancée par systemd).
    NoUnit,
    /// L'unité lance déjà `planes` : seule la description a été remise à jour.
    AlreadyPlanes,
    /// L'unité lance autre chose que `astroportd serve|planes` : laissée telle quelle.
    Foreign,
    /// Basculée de `serve` à `planes` ; `extras` = options propres à la station à reporter dans `args` du plan ORIGIN
    /// (`None` : ligne trop spéciale pour être lue, à reporter à la main), `backup` = ancienne unité.
    Switched {
        /// Options propres à la station.
        extras: Option<Vec<String>>,
        /// Sauvegarde de l'ancienne unité.
        backup: PathBuf,
    },
}

/// Bascule l'unité existante vers `planes`. N'écrit rien si l'unité est absente ou étrangère.
pub fn switch_to_planes(unit: &Path, exe: &Path, config: Option<&Path>, names: &[String], now: u64) -> std::io::Result<Switch> {
    let text = match std::fs::read_to_string(unit) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Switch::NoUnit),
        Err(e) => return Err(e),
    };
    let desc = planes_description(names);
    match mode_of(&text) {
        None => Ok(Switch::Foreign),
        Some(Mode::Planes) => {
            let new = rewrite(&text, exec_of(&text).unwrap_or_default(), Some(&desc));
            if new != text {
                std::fs::write(unit, new)?;
            }
            Ok(Switch::AlreadyPlanes)
        }
        Some(Mode::Serve) => {
            let extras = exec_of(&text).and_then(serve_extras);
            let backup = unit.with_extension(format!("service.bak-{now}"));
            std::fs::copy(unit, &backup)?;
            std::fs::write(unit, rewrite(&text, &planes_exec(exe, config), Some(&desc)))?;
            Ok(Switch::Switched { extras, backup })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVE: &str = "[Unit]\nDescription=astroportd (constellation ORIGIN)\n\n[Service]\nExecStart=/usr/bin/astroportd serve --db /h/uplanet.db --blobs /h/blobs --game-dir /g --snapshot-dir /h/snapshots --lat 43.6 --lon 1.44 --no-stun --face-threads 2 --kubo http://127.0.0.1:5001\nRestart=on-failure\nEnvironment=RUST_LOG=debug\n";

    #[test]
    fn detecte_le_mode_de_l_unite() {
        assert_eq!(mode_of(SERVE), Some(Mode::Serve));
        assert_eq!(mode_of(&render("d", "/x/astroportd planes")), Some(Mode::Planes));
        assert_eq!(mode_of("[Service]\nExecStart=/usr/bin/ipfs daemon\n"), None);
    }

    #[test]
    fn extras_sans_les_options_gerees_par_planes() {
        let e = serve_extras(exec_of(SERVE).unwrap()).unwrap();
        assert_eq!(e, ["--lat", "43.6", "--lon", "1.44", "--no-stun", "--face-threads", "2"]);
        assert_eq!(serve_extras("/x/astroportd serve --domain \"a b\""), None);
        assert_eq!(serve_extras("/x/astroportd serve --db=/h/db --no-p2p").unwrap(), ["--no-p2p"]);
    }

    #[test]
    fn reecrit_execstart_et_garde_le_reste() {
        let out = rewrite(SERVE, "/usr/bin/astroportd planes", Some("astroportd (plans : origin, zen1)"));
        assert!(out.contains("ExecStart=/usr/bin/astroportd planes\n"));
        assert!(out.contains("Description=astroportd (plans : origin, zen1)"));
        assert!(out.contains("Environment=RUST_LOG=debug") && out.contains("Restart=on-failure"));
    }

    #[test]
    fn config_hors_emplacement_par_defaut() {
        assert_eq!(planes_exec(Path::new("/x/astroportd"), Some(Path::new("/c/planes.json"))), "/x/astroportd planes --config /c/planes.json");
    }

    #[test]
    fn bascule_sauvegarde_et_ne_touche_pas_aux_unites_etrangeres() {
        let dir = std::env::temp_dir().join(format!("unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (u, names) = (dir.join("astroportd.service"), ["origin".to_owned(), "zen1".to_owned()]);
        let exe = Path::new("/usr/bin/astroportd");
        assert_eq!(switch_to_planes(&u, exe, None, &names, 1).unwrap(), Switch::NoUnit);

        std::fs::write(&u, SERVE).unwrap();
        let sw = switch_to_planes(&u, exe, None, &names, 7).unwrap();
        assert!(matches!(sw, Switch::Switched { .. }));
        if let Switch::Switched { extras, backup } = sw {
            assert_eq!(extras.unwrap().len(), 7);
            assert_eq!(std::fs::read_to_string(backup).unwrap(), SERVE);
        }
        assert_eq!(mode_of(&std::fs::read_to_string(&u).unwrap()), Some(Mode::Planes));
        assert_eq!(switch_to_planes(&u, exe, None, &names, 8).unwrap(), Switch::AlreadyPlanes);

        std::fs::write(&u, "[Service]\nExecStart=/usr/bin/ipfs daemon\n").unwrap();
        assert_eq!(switch_to_planes(&u, exe, None, &names, 9).unwrap(), Switch::Foreign);
        assert_eq!(std::fs::read_to_string(&u).unwrap(), "[Service]\nExecStart=/usr/bin/ipfs daemon\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
