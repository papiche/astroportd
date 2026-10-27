//! Routage intelligent du swarm (héritage d'`astrosystemctl connect <service> [power|closest|random]`).
//!
//! Chaque station publie dans son `12345.json` un Power-Score `capacities.power_score`, ses coordonnées
//! `STATION_LAT` / `STATION_LON` et les services qu'elle fournit (`dragon_services`). Ce module calcule le score du
//! matériel local et choisit, parmi les nœuds qualifiés pour un service, le meilleur selon le mode :
//!
//! - `power` : Power-Score le plus élevé ;
//! - `closest` : distance de Haversine la plus courte entre les coordonnées UMAP ;
//! - `random` : tirage équilibré parmi les trois plus puissants (répartition de charge).
//!
//! Aucun repli silencieux : si `closest` ne peut pas s'appliquer (coordonnées locales inconnues), la sélection le
//! dit ([`Selection::fell_back`]) au lieu de faire croire qu'elle a mesuré une distance.

use std::str::FromStr;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Rayon moyen de la Terre, en kilomètres.
const EARTH_RADIUS_KM: f64 = 6371.0;

/// Power-Score : `(VRAM Go × 4) + (cœurs CPU × 2) + (RAM Go / 2)`.
pub fn power_score(vram_gb: f64, cpu_cores: u32, ram_gb: f64) -> f64 {
    vram_gb * 4.0 + f64::from(cpu_cores) * 2.0 + ram_gb / 2.0
}

/// Profil d'une station selon son score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// 0–10 : Raspberry Pi, consommateur uniquement.
    Light,
    /// 11–40 : PC bureautique, petits modèles locaux.
    Standard,
    /// 41 et plus : GPU dédié, fournisseur du swarm.
    Brain,
}

impl Tier {
    /// Profil du score `score`.
    pub fn of(score: f64) -> Self {
        if score > 40.0 {
            Self::Brain
        } else if score > 10.0 {
            Self::Standard
        } else {
            Self::Light
        }
    }

    /// Nom affiché.
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Standard => "Standard",
            Self::Brain => "Brain",
        }
    }
}

/// Matériel de la station.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hardware {
    /// Mémoire vidéo totale, Go.
    pub vram_gb: f64,
    /// Cœurs logiques.
    pub cpu_cores: u32,
    /// Mémoire vive totale, Go.
    pub ram_gb: f64,
}

impl Hardware {
    /// Power-Score.
    pub fn score(&self) -> f64 {
        power_score(self.vram_gb, self.cpu_cores, self.ram_gb)
    }

    /// Profil.
    pub fn tier(&self) -> Tier {
        Tier::of(self.score())
    }

    /// Mesure le matériel local. VRAM : `ASTRO_VRAM_GB` (surcharge) sinon `nvidia-smi` (3 s au plus) ; 0 sans GPU
    /// NVIDIA. **Bloquant** : à appeler depuis `spawn_blocking`.
    pub fn detect() -> Self {
        let cpu_cores = std::thread::available_parallelism().map_or(1, |n| u32::try_from(n.get()).unwrap_or(1));
        let ram_gb = std::fs::read_to_string("/proc/meminfo").ok().and_then(|m| parse_meminfo_gb(&m)).unwrap_or(0.0);
        let vram_gb = std::env::var("ASTRO_VRAM_GB").ok().and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or_else(nvidia_vram_gb);
        Self { vram_gb, cpu_cores, ram_gb }
    }
}

fn parse_meminfo_gb(meminfo: &str) -> Option<f64> {
    let kb: f64 = meminfo.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.split_whitespace().next()?.parse().ok()?;
    Some(kb / 1024.0 / 1024.0)
}

fn nvidia_vram_gb() -> f64 {
    let Ok(mut child) = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return 0.0;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(3) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return 0.0;
            }
        }
    }
    let Ok(out) = child.wait_with_output() else { return 0.0 };
    // Une ligne par carte, en Mio.
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse::<f64>().ok()).sum::<f64>() / 1024.0
}

/// Mode de sélection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Power-Score le plus élevé.
    #[default]
    Power,
    /// Le plus proche géographiquement.
    Closest,
    /// Tirage parmi les trois plus puissants.
    Random,
}

impl FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "power" => Ok(Self::Power),
            "closest" => Ok(Self::Closest),
            "random" => Ok(Self::Random),
            other => Err(format!("mode inconnu « {other} » (power, closest ou random)")),
        }
    }
}

/// Un nœud du swarm, tel que publié dans son `12345.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Identifiant (`IPFSNODEID`).
    pub node_id: String,
    /// Power-Score annoncé.
    pub power_score: f64,
    /// Latitude UMAP.
    pub lat: Option<f64>,
    /// Longitude UMAP.
    pub lon: Option<f64>,
    /// Services DRAGON fournis.
    pub services: Vec<String>,
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

impl Candidate {
    /// Lit un `12345.json` : `capacities.power_score`, `STATION_LAT`, `STATION_LON`, `dragon_services`.
    pub fn from_station_json(node_id: &str, json: &Value) -> Self {
        let services = json
            .get("dragon_services")
            .and_then(Value::as_str)
            .map(|s| s.split(|c: char| c.is_whitespace() || c == ',').map(|t| t.trim().trim_end_matches(".sh")).filter(|t| !t.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default();
        Self {
            node_id: node_id.to_owned(),
            power_score: number(json.pointer("/capacities/power_score")).unwrap_or(0.0),
            lat: number(json.get("STATION_LAT")),
            lon: number(json.get("STATION_LON")),
            services,
        }
    }

    fn position(&self) -> Option<(f64, f64)> {
        Some((self.lat?, self.lon?))
    }
}

/// Lit les `<dossier>/<node_id>/12345.json` des pairs (`~/.zen/tmp/swarm`). Un dossier absent ou un fichier illisible
/// est ignoré. **Bloquant** : à appeler depuis `spawn_blocking`.
pub fn read_swarm_dir(dir: &std::path::Path) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path().join("12345.json")).ok()?;
            let json = serde_json::from_str::<Value>(&text).ok()?;
            Some(Candidate::from_station_json(&e.file_name().to_string_lossy(), &json))
        })
        .collect()
}

/// Distance de Haversine en kilomètres entre deux points `(latitude, longitude)` en degrés.
pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (d_lat, d_lon) = ((b.0 - a.0).to_radians(), (b.1 - a.1).to_radians());
    let h = (d_lat / 2.0).sin().powi(2) + a.0.to_radians().cos() * b.0.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    EARTH_RADIUS_KM * 2.0 * h.sqrt().atan2((1.0 - h).sqrt())
}

/// Résultat d'une sélection.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection<'a> {
    /// Nœud retenu.
    pub node: &'a Candidate,
    /// Mode réellement appliqué.
    pub mode: Mode,
    /// `true` si le mode demandé n'a pas pu s'appliquer et que `Power` l'a remplacé.
    pub fell_back: bool,
    /// Distance en km (mode `closest` seulement).
    pub distance_km: Option<f64>,
}

/// Les nœuds qui fournissent `service`.
pub fn qualified<'a>(candidates: &'a [Candidate], service: &str) -> Vec<&'a Candidate> {
    candidates.iter().filter(|c| c.services.iter().any(|s| s == service)).collect()
}

/// Plus puissants d'abord ; à score égal, identifiant le plus petit (ordre total, résultat reproductible).
fn by_power(a: &&Candidate, b: &&Candidate) -> std::cmp::Ordering {
    b.power_score.total_cmp(&a.power_score).then_with(|| a.node_id.cmp(&b.node_id))
}

/// Choisit le meilleur nœud pour `service`. `origin` : coordonnées de cette station (mode `closest`) ; `rand` :
/// un entier aléatoire fourni par l'appelant (mode `random`), ce qui garde la fonction pure et testable.
pub fn select<'a>(candidates: &'a [Candidate], service: &str, mode: Mode, origin: Option<(f64, f64)>, rand: u64) -> Option<Selection<'a>> {
    let mut pool = qualified(candidates, service);
    if pool.is_empty() {
        return None;
    }
    pool.sort_by(by_power);
    match mode {
        Mode::Power => Some(Selection { node: pool[0], mode, fell_back: false, distance_km: None }),
        Mode::Random => {
            let top = &pool[..pool.len().min(3)];
            let pick = top[usize::try_from(rand % top.len() as u64).unwrap_or(0)];
            Some(Selection { node: pick, mode, fell_back: false, distance_km: None })
        }
        Mode::Closest => {
            let Some(here) = origin else {
                return Some(Selection { node: pool[0], mode: Mode::Power, fell_back: true, distance_km: None });
            };
            // Les nœuds sans coordonnées sont écartés (on ne sait pas où ils sont).
            let best = pool.iter().filter_map(|c| c.position().map(|p| (haversine_km(here, p), *c))).min_by(|a, b| a.0.total_cmp(&b.0).then_with(|| by_power(&a.1, &b.1)));
            match best {
                Some((km, node)) => Some(Selection { node, mode, fell_back: false, distance_km: Some(km) }),
                None => Some(Selection { node: pool[0], mode: Mode::Power, fell_back: true, distance_km: None }),
            }
        }
    }
}

/// Entier aléatoire pour le mode `random`.
pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    // Sans aléa système on retombe sur 0 : `random` devient « le plus puissant », jamais un échec de routage.
    let _ = getrandom::fill(&mut b);
    u64::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn node(id: &str, score: f64, pos: Option<(f64, f64)>, services: &[&str]) -> Candidate {
        Candidate { node_id: id.into(), power_score: score, lat: pos.map(|p| p.0), lon: pos.map(|p| p.1), services: services.iter().map(|s| (*s).to_owned()).collect() }
    }

    const PARIS: (f64, f64) = (48.8566, 2.3522);
    const LYON: (f64, f64) = (45.7640, 4.8357);
    const MARSEILLE: (f64, f64) = (43.2965, 5.3698);

    #[test]
    fn the_power_score_formula_and_tiers() {
        // RTX 3090 : 24 Go de VRAM, 16 cœurs, 64 Go de RAM.
        let brain = Hardware { vram_gb: 24.0, cpu_cores: 16, ram_gb: 64.0 };
        assert_eq!(brain.score(), 24.0 * 4.0 + 16.0 * 2.0 + 32.0);
        assert_eq!(brain.tier(), Tier::Brain);
        assert_eq!(Hardware { vram_gb: 0.0, cpu_cores: 4, ram_gb: 1.0 }.tier(), Tier::Light);
        assert_eq!(Hardware { vram_gb: 0.0, cpu_cores: 8, ram_gb: 16.0 }.tier(), Tier::Standard);
        assert_eq!(Tier::of(41.0), Tier::Brain);
        assert_eq!(Tier::of(40.0), Tier::Standard);
        assert_eq!(Tier::of(10.0), Tier::Light);
    }

    #[test]
    fn meminfo_is_read_in_gigabytes() {
        let gb = parse_meminfo_gb("MemTotal:       16384000 kB\nMemFree: 1 kB\n").unwrap();
        assert!((gb - 15.625).abs() < 1e-9);
    }

    #[test]
    fn haversine_matches_known_distances() {
        assert!((haversine_km(PARIS, LYON) - 392.0).abs() < 3.0);
        assert!((haversine_km(PARIS, MARSEILLE) - 661.0).abs() < 4.0);
        assert_eq!(haversine_km(PARIS, PARIS), 0.0);
    }

    #[test]
    fn a_station_json_is_read_with_the_legacy_field_names() {
        let j = json!({ "capacities": { "power_score": 54 }, "STATION_LAT": "45.76", "STATION_LON": 4.83, "dragon_services": "ollama.sh comfyui, qdrant" });
        let c = Candidate::from_station_json("12D3KooWabc", &j);
        assert_eq!((c.power_score, c.lat, c.lon), (54.0, Some(45.76), Some(4.83)));
        assert_eq!(c.services, ["ollama", "comfyui", "qdrant"]);
        let empty = Candidate::from_station_json("x", &json!({}));
        assert_eq!((empty.power_score, empty.lat, empty.services.len()), (0.0, None, 0));
    }

    fn swarm() -> Vec<Candidate> {
        vec![
            node("a-lyon", 54.0, Some(LYON), &["ollama", "comfyui"]),
            node("b-marseille", 90.0, Some(MARSEILLE), &["ollama"]),
            node("c-paris", 20.0, Some(PARIS), &["ollama"]),
            node("d-nulle-part", 70.0, None, &["ollama"]),
            node("e-sans-ollama", 999.0, Some(PARIS), &["qdrant"]),
            node("f-bas", 5.0, Some(LYON), &["ollama"]),
        ]
    }

    #[test]
    fn power_picks_the_strongest_qualified_node() {
        let s = swarm();
        let sel = select(&s, "ollama", Mode::Power, None, 0).unwrap();
        assert_eq!(sel.node.node_id, "b-marseille", "e-sans-ollama est plus fort mais ne fournit pas le service");
        assert!(!sel.fell_back);
        assert!(select(&s, "inconnu", Mode::Power, None, 0).is_none());
    }

    #[test]
    fn closest_uses_haversine_and_skips_nodes_without_coordinates() {
        let s = swarm();
        let sel = select(&s, "ollama", Mode::Closest, Some(PARIS), 0).unwrap();
        assert_eq!((sel.node.node_id.as_str(), sel.mode), ("c-paris", Mode::Closest));
        assert_eq!(sel.distance_km, Some(0.0));
        let from_nice = select(&s, "ollama", Mode::Closest, Some((43.7102, 7.2620)), 0).unwrap();
        assert_eq!(from_nice.node.node_id, "b-marseille");
        assert!(from_nice.distance_km.unwrap() > 150.0);
    }

    #[test]
    fn closest_without_local_coordinates_says_it_fell_back_to_power() {
        let s = swarm();
        let sel = select(&s, "ollama", Mode::Closest, None, 0).unwrap();
        assert_eq!((sel.node.node_id.as_str(), sel.mode, sel.fell_back), ("b-marseille", Mode::Power, true));
    }

    #[test]
    fn random_draws_only_among_the_top_three_and_covers_all_of_them() {
        let s = swarm();
        let mut seen = std::collections::BTreeSet::new();
        for r in 0..30u64 {
            seen.insert(select(&s, "ollama", Mode::Random, None, r).unwrap().node.node_id.clone());
        }
        // Top 3 des fournisseurs d'ollama : b (90), d (70), a (54).
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), ["a-lyon", "b-marseille", "d-nulle-part"]);
        // Un seul qualifié : toujours lui.
        assert_eq!(select(&s, "comfyui", Mode::Random, None, 12345).unwrap().node.node_id, "a-lyon");
    }

    #[test]
    fn ties_are_broken_by_node_id_for_reproducible_choices() {
        let s = vec![node("z", 50.0, None, &["x"]), node("a", 50.0, None, &["x"])];
        assert_eq!(select(&s, "x", Mode::Power, None, 0).unwrap().node.node_id, "a");
    }

    #[test]
    fn modes_parse() {
        assert_eq!("Closest".parse::<Mode>(), Ok(Mode::Closest));
        assert!("rapide".parse::<Mode>().is_err());
    }
}
