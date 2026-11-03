//! Nom de domaine de la station, réglé depuis le cockpit, et redémarrage demandé par le Capitaine.
//!
//! Le Capitaine n'a plus à relancer `astroportd init --domain` dans un terminal : le cockpit vérifie le nom (forme, DNS,
//! ports 80/443 de cette machine), l'écrit dans `station.conf`, puis propose de **redémarrer** la station. Au démarrage,
//! `serve` sans `--domain` reprend le domaine de `station.conf` ([`domain_from_conf`]) et obtient seul son certificat.
//!
//! Redémarrer = s'arrêter proprement avec un code d'échec : l'unité systemd écrite par `init` (`Restart=on-failure`)
//! relance alors la station. Hors systemd, rien ne la relancerait : le cockpit donne la commande au lieu du bouton.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Signal de redémarrage : notifié par l'API, attendu par `serve::run`.
pub static RESTART: Notify = Notify::const_new();
/// `serve::run` (une seule constellation par processus) sait redémarrer ; `planes` ne l'arme pas.
static ARMED: AtomicBool = AtomicBool::new(false);
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Domaine de `station.conf` écarté au démarrage (ports indisponibles) : la cause, pour le cockpit.
static REFUSED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Note pourquoi le domaine enregistré n'a pas été appliqué.
pub fn set_refused(why: String) {
    let _ = REFUSED.set(why);
}

/// Cause du refus du domaine enregistré, s'il y en a eu un.
pub fn refused() -> Option<&'static str> {
    REFUSED.get().map(String::as_str)
}

/// Appelé par `serve::run` : le redémarrage depuis le cockpit devient possible.
pub fn arm() {
    ARMED.store(true, Ordering::SeqCst);
}

/// Le bouton « Redémarrer » a-t-il un sens ici ? Il faut une boucle qui écoute (`serve`) et systemd pour relancer.
pub fn restart_available() -> bool {
    ARMED.load(Ordering::SeqCst) && std::env::var_os("INVOCATION_ID").is_some()
}

/// Demande l'arrêt (après la réponse HTTP : `delay`).
pub fn request_restart(delay: std::time::Duration) {
    REQUESTED.store(true, Ordering::SeqCst);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        RESTART.notify_one();
    });
}

/// L'arrêt en cours vient-il d'une demande de redémarrage ?
pub fn restart_requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Nom de domaine acceptable : lettres, chiffres, tirets, au moins deux étiquettes, 253 caractères au plus.
pub fn normalize_domain(input: &str) -> Result<String, String> {
    let d = input.trim().trim_end_matches('.').to_ascii_lowercase();
    let d = d.strip_prefix("https://").or_else(|| d.strip_prefix("http://")).unwrap_or(&d).trim_end_matches('/').to_owned();
    let labels: Vec<&str> = d.split('.').collect();
    let label_ok = |l: &&str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if d.len() > 253 || labels.len() < 2 || !labels.iter().all(label_ok) || labels.last().is_some_and(|t| t.chars().all(|c| c.is_ascii_digit())) {
        return Err(format!("« {} » n'est pas un nom de domaine (exemple : station.mondomaine.org)", input.trim()));
    }
    Ok(d)
}

/// Remplace (ou ajoute) la ligne `clé = valeur` d'un `station.conf`, sans toucher au reste. Fichier en `0600`.
pub fn set_conf_value(path: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut found = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| match l.split_once('=') {
            Some((k, _)) if !l.trim_start().starts_with('#') && k.trim() == key => {
                found = true;
                format!("{key} = {value}")
            }
            _ => l.to_owned(),
        })
        .collect();
    if !found {
        lines.push(format!("{key} = {value}"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(path)?, (lines.join("\n") + "\n").as_bytes())?;
    // `mode` ne vaut qu'à la création : un fichier déjà là garde ses droits, on les resserre.
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}

/// Domaine et email de contact ACME repris de `station.conf` (`domain`, `captain_email`) pour un `serve` lancé sans
/// `--domain`. Rien si une écoute HTTP est imposée (`--http-listen` : derrière un proxy, c'est lui qui tient le TLS).
pub fn domain_from_conf(conf: &Path, http_listen_given: bool) -> Option<(String, Option<String>)> {
    if http_listen_given {
        return None;
    }
    let conf = crate::init::read_conf(conf);
    let get = |k: &str| conf.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
    let domain = normalize_domain(&get("domain")?).ok()?;
    Some((domain, get("captain_email")))
}

/// État d'un port de cette machine.
pub fn port_state(port: u16) -> &'static str {
    match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(_) => "free",
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => "busy",
        Err(_) => "privileged",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_is_cleaned_and_checked() {
        assert_eq!(normalize_domain(" HTTPS://Station.Exemple.org/ ").unwrap(), "station.exemple.org");
        assert_eq!(normalize_domain("station.exemple.org.").unwrap(), "station.exemple.org");
        for bad in ["localhost", "exemple", "-a.org", "a..org", "a b.org", "192.168.1.10", "évé.org", "a.org/chemin"] {
            assert!(normalize_domain(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_conf_keeps_its_other_lines_and_serve_reads_the_domain_back() {
        let dir = std::env::temp_dir().join(format!("astroportd-domain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("station.conf");
        std::fs::write(&conf, "# commentaire\nmode = origin\ndomain =\ncaptain_email = cap@exemple.org\nkind.7 = read=all\n").unwrap();
        assert_eq!(domain_from_conf(&conf, false), None, "domaine vide : HTTP local");
        set_conf_value(&conf, "domain", "station.exemple.org").unwrap();
        let text = std::fs::read_to_string(&conf).unwrap();
        assert!(text.starts_with("# commentaire\nmode = origin\ndomain = station.exemple.org\n") && text.contains("kind.7 = read=all"), "{text}");
        assert_eq!(domain_from_conf(&conf, false), Some(("station.exemple.org".into(), Some("cap@exemple.org".into()))));
        assert_eq!(domain_from_conf(&conf, true), None, "derrière un proxy (--http-listen), le TLS reste au proxy");
        set_conf_value(&conf, "lat", "43.6").unwrap();
        assert!(std::fs::read_to_string(&conf).unwrap().ends_with("lat = 43.6\n"), "clé absente : ajoutée");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&conf).unwrap().permissions()) & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }
}
