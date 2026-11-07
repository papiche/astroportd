//! **Corbeille de la station** : `~/.zen/game/trash/<raison>/<nom>.<horodatage>`.
//!
//! Tout ce que la station met de côté au lieu de l'effacer tout de suite y va, rangé par raison :
//! - `ejected` : MULTIPASS éjecté par le Capitaine (jamais connecté, voir [`crate::register::Registrar::eject`]) ;
//! - `moved` : dossier d'un compte parti vers une autre station (voir [`crate::private_backup`]).
//!
//! La nuit de 20h12 efface ce qui y dort depuis plus de [`KEEP_SECS`] (30 jours) ; d'ici là, le Capitaine peut tout
//! récupérer à la main (`mv`). Les anciens emplacements (`nostr/.moved/`, `nostr.ejected/`) sont vidés par la même passe.
//! Une erreur est toujours dite, jamais avalée.

use std::path::{Path, PathBuf};

/// Durée de conservation avant effacement (30 jours).
pub const KEEP_SECS: i64 = 30 * 24 * 3600;

/// Dossier de la corbeille, à côté de `nostr/` dans `~/.zen/game`.
pub fn root(game: &Path) -> PathBuf {
    game.join("trash")
}

/// Range `src` dans `trash/<reason>/<name>.<at>` (déplacement, rien n'est copié). Renvoie le nouveau chemin.
pub fn put(game: &Path, reason: &str, name: &str, src: &Path, at: i64) -> std::io::Result<PathBuf> {
    let dir = root(game).join(reason);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(format!("{name}.{at}"));
    std::fs::rename(src, &dest)?;
    Ok(dest)
}

/// Date de mise à la corbeille, lue dans le nom (`<nom>.<horodatage>` ou l'ancien `<nom>-<horodatage>`).
fn stamped(name: &str) -> Option<i64> {
    let digits = name.rsplit(['.', '-']).next()?;
    (digits.len() >= 9 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten()
}

fn sweep(dir: &Path, now: i64, keep: i64) -> Result<usize, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("lecture de {} : {e}", dir.display())),
    };
    let mut gone = 0;
    for entry in entries.flatten() {
        let Some(at) = stamped(&entry.file_name().to_string_lossy()) else { continue };
        if now.saturating_sub(at) < keep {
            continue;
        }
        let path = entry.path();
        let r = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        r.map_err(|e| format!("effacement de {} : {e}", path.display()))?;
        gone += 1;
    }
    Ok(gone)
}

/// Passe de nuit : efface ce qui dort dans la corbeille (et les anciens emplacements) depuis plus de `keep` secondes.
pub fn purge(game: &Path, now: i64, keep: i64) -> Result<usize, String> {
    let mut gone = 0;
    let trash = root(game);
    match std::fs::read_dir(&trash) {
        Ok(reasons) => {
            for r in reasons.flatten().filter(|e| e.path().is_dir()) {
                gone += sweep(&r.path(), now, keep)?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("lecture de {} : {e}", trash.display())),
    }
    gone += sweep(&game.join("nostr").join(".moved"), now, keep)?;
    gone += sweep(&game.join("nostr.ejected"), now, keep)?;
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_put_aside_is_kept_thirty_days_then_erased() {
        let game = std::env::temp_dir().join(format!("astroportd-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&game);
        let acc = game.join("nostr/ana@ex.org");
        std::fs::create_dir_all(&acc).unwrap();
        std::fs::write(acc.join(".secret.nostr"), "x").unwrap();
        let at = 1_800_000_000;
        let dest = put(&game, "ejected", "ana@ex.org", &acc, at).unwrap();
        assert!(dest.ends_with("trash/ejected/ana@ex.org.1800000000") && !acc.exists());
        // Ancien emplacement des comptes déménagés : vidé par la même passe.
        std::fs::create_dir_all(game.join("nostr/.moved/bob@ex.org-1800000000")).unwrap();
        assert_eq!(purge(&game, at + KEEP_SECS - 1, KEEP_SECS).unwrap(), 0, "trop tôt");
        assert_eq!(purge(&game, at + KEEP_SECS, KEEP_SECS).unwrap(), 2);
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&game);
    }
}
