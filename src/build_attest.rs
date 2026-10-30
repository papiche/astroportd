//! Attestation de build : chaque station publie la version et l'empreinte SHA-256 de son binaire (kind 30853, signé
//! par le NODE). L'écran du Capitaine compare ce que les autres stations annoncent à ce que la sienne exécute, pour
//! voir d'un coup d'œil si le code est identique d'une station à l'autre.
//!
//! L'attestation est **déclarative** : elle prouve ce que le NODE dit exécuter, pas ce qu'il exécute. Elle repère
//! une dérive honnête (mise à jour oubliée, binaire recompilé) ; elle n'arrête pas une station qui mentirait.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use astro_bro::publisher::Publisher;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Ré-attestation périodique : un pair qui démarre plus tard n'attend pas la prochaine nuit pour voir cette station.
pub const REFRESH: Duration = Duration::from_secs(6 * 3600);

/// Ce que cette station exécute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    /// Version du paquet (`CARGO_PKG_VERSION`).
    pub version: String,
    /// SHA-256 du binaire en cours d'exécution, en hexadécimal.
    pub sha256: String,
    /// Cible : `<arch>-<os>`. Deux cibles différentes donnent forcément des binaires différents.
    pub target: String,
}

impl BuildInfo {
    /// Lit et hache le binaire courant. **Bloquant** : à appeler depuis `spawn_blocking`.
    pub fn current() -> std::io::Result<Self> {
        let mut file = std::fs::File::open(std::env::current_exe()?)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Self { version: env!("CARGO_PKG_VERSION").to_owned(), sha256: hex::encode(hasher.finalize()), target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS) })
    }

    /// Événement 30853 `d = build`, signé par le NODE (un seul par signataire : le plus récent remplace l'ancien).
    pub fn event(&self, peer_id: Option<&str>, secret: &[u8; 32], now: u64) -> Result<Event, String> {
        let mut tags = vec![
            vec!["d".to_owned(), "build".to_owned()],
            vec!["version".to_owned(), self.version.clone()],
            vec!["sha256".to_owned(), self.sha256.clone()],
            vec!["target".to_owned(), self.target.clone()],
        ];
        if let Some(p) = peer_id {
            tags.push(vec!["station".to_owned(), p.to_owned()]);
        }
        UnsignedEvent { created_at: now, kind: kind::STATION_BUILD, tags, content: String::new() }.sign(secret).map_err(|e| e.to_string())
    }

    /// Sérialisation pour l'API : `{ version, sha256, target }`.
    pub fn json(&self) -> Value {
        serde_json::json!({ "version": self.version, "sha256": self.sha256, "target": self.target })
    }
}

/// Résultat de la comparaison d'une station à la nôtre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Même version, même cible, même empreinte : code identique.
    Identical,
    /// Même version et même cible mais empreinte différente : binaire recompilé ou modifié. À examiner.
    Diverges,
    /// Version différente : mise à jour à faire d'un côté.
    OtherVersion,
    /// Même version, autre cible (ex. aarch64 contre x86_64) : les empreintes ne sont pas comparables.
    OtherTarget,
    /// Pas d'attestation reçue de cette station.
    Unknown,
}

impl Verdict {
    /// Code stable pour l'API et l'interface.
    pub fn code(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::Diverges => "diverges",
            Self::OtherVersion => "other_version",
            Self::OtherTarget => "other_target",
            Self::Unknown => "unknown",
        }
    }
}

/// Compare l'attestation `theirs` (tags `version`, `sha256`, `target`) à `mine`.
pub fn compare(mine: &BuildInfo, theirs: Option<(&str, &str, &str)>) -> Verdict {
    let Some((version, sha256, target)) = theirs else { return Verdict::Unknown };
    if version != mine.version {
        Verdict::OtherVersion
    } else if target != mine.target {
        Verdict::OtherTarget
    } else if sha256.eq_ignore_ascii_case(&mine.sha256) {
        Verdict::Identical
    } else {
        Verdict::Diverges
    }
}

/// Publie l'attestation au démarrage puis toutes les [`REFRESH`], jusqu'à l'arrêt.
pub async fn run<P: Publisher>(info: BuildInfo, peer_id: Option<String>, secret: [u8; 32], publisher: Arc<P>, mut stop: tokio::sync::watch::Receiver<bool>) {
    loop {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        match info.event(peer_id.as_deref(), &secret, now) {
            Ok(event) => {
                if let Err(e) = publisher.publish(event).await {
                    eprintln!("Attestation de build : publication impossible : {e}");
                }
            }
            Err(e) => eprintln!("Attestation de build : signature impossible : {e}"),
        }
        tokio::select! {
            () = tokio::time::sleep(REFRESH) => {}
            _ = stop.changed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(sha: &str) -> BuildInfo {
        BuildInfo { version: "0.2.2-beta".into(), sha256: sha.repeat(64 / sha.len()), target: "x86_64-linux".into() }
    }

    #[test]
    fn le_binaire_courant_a_une_empreinte_stable() {
        let a = BuildInfo::current().unwrap();
        assert_eq!(a.sha256.len(), 64);
        assert_eq!(a, BuildInfo::current().unwrap());
    }

    #[test]
    fn verdicts() {
        let me = info("ab");
        let sha = me.sha256.clone();
        assert_eq!(compare(&me, Some(("0.2.2-beta", &sha, "x86_64-linux"))), Verdict::Identical);
        assert_eq!(compare(&me, Some(("0.2.2-beta", &info("cd").sha256, "x86_64-linux"))), Verdict::Diverges);
        assert_eq!(compare(&me, Some(("0.2.1-beta", &sha, "x86_64-linux"))), Verdict::OtherVersion);
        assert_eq!(compare(&me, Some(("0.2.2-beta", &info("cd").sha256, "aarch64-linux"))), Verdict::OtherTarget);
        assert_eq!(compare(&me, None), Verdict::Unknown);
    }

    #[test]
    fn l_evenement_est_signe_et_porte_les_tags() {
        let e = info("ab").event(Some("12D3KooWabc"), &[7u8; 32], 1_000).unwrap();
        assert_eq!(e.kind, kind::STATION_BUILD);
        assert_eq!(e.tag_value("d"), Some("build"));
        assert_eq!(e.tag_value("station"), Some("12D3KooWabc"));
        assert!(e.verify().is_ok());
    }
}
