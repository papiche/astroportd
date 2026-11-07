//! **Énergie de la station**, préparée par le plugin système `energy` (dépôt `papiche/astroportd-plugins`).
//!
//! Le plugin fait tourner les capteurs (PowerJoular pour la consommation, régulateurs Victron en Bluetooth pour la
//! production solaire) et dépose `<station>/plugins/energy/energy.json` au format `energy/v1` (schéma JSON et OpenAPI
//! dans `contracts/` de ce dépôt). La station **lit** ce fichier, le valide et le date ; elle n'exécute jamais le code
//! du plugin. Un fichier absent veut dire « plugin non installé » ; illisible ou de plus d'une heure, il est ignoré et
//! le cockpit dit pourquoi.
//!
//! Ce qui en sort : les tags `hw:energy_*` et `hw:solar_*` de la balise 30850 (mêmes noms qu'`ECONOMY.broadcast.sh`
//! d'Astroport.ONE), son contenu `hardware.energy` / `hardware.solar` (lu par la vue LIVE et le guide du Capitaine) et
//! la carte « Énergie » du cockpit.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

/// Format attendu.
pub const SCHEMA: &str = "energy/v1";
/// Au-delà, les mesures sont périmées (le plugin écrit toutes les 5 minutes).
pub const STALE_AFTER_SECS: u64 = 3_600;
/// Taille maximale lue : le fichier fait quelques centaines d'octets.
const MAX_BYTES: u64 = 64 * 1024;

/// Consommation (PowerJoular).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Consumption {
    /// Capteur.
    pub source: String,
    /// Puissance des 5 dernières minutes mesurées (W).
    pub power_w: Option<f64>,
    /// Dernier échantillon (secondes Unix).
    pub last_sample_at: Option<u64>,
    /// Aujourd'hui (kWh).
    pub today_kwh: f64,
    /// Ce mois-ci (kWh).
    pub month_kwh: f64,
    /// Depuis le début des mesures (kWh).
    pub total_kwh: f64,
    /// Puissance moyenne des 30 derniers jours (W).
    pub avg_w_recent: f64,
    /// Premier jour mesuré (`AAAA-MM-JJ`).
    pub since: String,
    /// Jours mesurés.
    pub days_recorded: u64,
}

/// Production solaire (Victron).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Production {
    /// Capteur.
    pub source: String,
    /// Puissance de l'heure écoulée, tous régulateurs (W).
    pub power_w: Option<f64>,
    /// Tension de batterie (V).
    pub battery_v: Option<f64>,
    /// État de charge du régulateur.
    #[serde(default)]
    pub charge_state: Option<String>,
    /// Régulateurs vus.
    #[serde(default)]
    pub devices: Option<u64>,
    /// Dernière lecture (secondes Unix).
    pub last_sample_at: Option<u64>,
    /// Aujourd'hui (Wh).
    pub today_wh: f64,
    /// Ce mois-ci (Wh).
    pub month_wh: f64,
    /// Depuis le début des mesures (Wh).
    pub total_wh: f64,
    /// Premier jour mesuré.
    pub since: Option<String>,
    /// Jours mesurés.
    pub days_recorded: u64,
}

/// Contenu d'un `energy.json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Reading {
    /// `energy/v1`.
    pub schema: String,
    /// Écriture du fichier (secondes Unix).
    pub generated_at: u64,
    /// Consommation, si un capteur la mesure.
    pub consumption: Option<Consumption>,
    /// Production, si un régulateur est lu.
    pub production: Option<Production>,
}

/// Ce que la station sait de son énergie.
#[derive(Debug, Clone, PartialEq)]
pub enum Energy {
    /// Pas de fichier : plugin non installé.
    Absent,
    /// Fichier illisible ou hors contrat.
    Invalid(String),
    /// Mesures de plus d'une heure (collecteur arrêté ?).
    Stale {
        /// Âge du fichier (secondes).
        age_secs: u64,
        /// Dernières mesures connues.
        reading: Reading,
    },
    /// Mesures à jour.
    Fresh(Reading),
}

/// Fichier contrat du plugin pour une station dont la base est dans `station_home`.
pub fn file(station_home: &Path) -> PathBuf {
    station_home.join("plugins").join("energy").join("energy.json")
}

fn finite(name: &str, v: f64) -> Result<(), String> {
    if v.is_finite() && v >= 0.0 { Ok(()) } else { Err(format!("{name} : valeur invalide ({v})")) }
}

/// Valide un `energy.json` déjà lu.
pub fn parse(text: &str) -> Result<Reading, String> {
    let r: Reading = serde_json::from_str(text).map_err(|e| format!("hors contrat {SCHEMA} : {e}"))?;
    if r.schema != SCHEMA {
        return Err(format!("format {} au lieu de {SCHEMA} (plugin trop récent ou trop ancien)", r.schema));
    }
    if let Some(c) = &r.consumption {
        for (n, v) in [("today_kwh", c.today_kwh), ("month_kwh", c.month_kwh), ("total_kwh", c.total_kwh), ("avg_w_recent", c.avg_w_recent), ("power_w", c.power_w.unwrap_or(0.0))] {
            finite(n, v)?;
        }
    }
    if let Some(p) = &r.production {
        for (n, v) in [("today_wh", p.today_wh), ("month_wh", p.month_wh), ("total_wh", p.total_wh), ("power_w", p.power_w.unwrap_or(0.0))] {
            finite(n, v)?;
        }
    }
    Ok(r)
}

/// Lit l'énergie de la station à l'instant `now`.
pub fn read(station_home: &Path, now: u64) -> Energy {
    let path = file(station_home);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Energy::Absent,
        Err(e) => return Energy::Invalid(format!("{} : {e}", path.display())),
    };
    if meta.len() > MAX_BYTES {
        return Energy::Invalid(format!("{} : {} octets, trop gros pour un {SCHEMA}", path.display(), meta.len()));
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return Energy::Invalid(format!("{} : {e}", path.display())),
    };
    match parse(&text) {
        Err(e) => Energy::Invalid(e),
        Ok(r) => {
            let age = now.saturating_sub(r.generated_at);
            if age > STALE_AFTER_SECS { Energy::Stale { age_secs: age, reading: r } } else { Energy::Fresh(r) }
        }
    }
}

fn num(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}")
}

/// Tags de la balise 30850 (noms d'`ECONOMY.broadcast.sh`), pour des mesures à jour.
pub fn beacon_tags(r: &Reading) -> Vec<Vec<String>> {
    let mut tags = Vec::new();
    let mut add = |k: &str, v: String| tags.push(vec![k.to_owned(), v]);
    if let Some(c) = &r.consumption {
        add("hw:energy_today_kwh", num(c.today_kwh, 3));
        add("hw:energy_month_kwh", num(c.month_kwh, 3));
        add("hw:energy_total_kwh", num(c.total_kwh, 3));
        add("hw:avg_w_recent", num(c.avg_w_recent, 1));
    }
    if let Some(p) = &r.production {
        add("hw:solar_power_w", num(p.power_w.unwrap_or(0.0), 0));
        add("hw:solar_yield_today_wh", num(p.today_wh, 0));
        add("hw:solar_month_wh", num(p.month_wh, 0));
        add("hw:solar_total_wh", num(p.total_wh, 0));
        add("hw:solar_battery_v", num(p.battery_v.unwrap_or(0.0), 2));
        add("hw:solar_charge_state", p.charge_state.clone().unwrap_or_else(|| "unknown".to_owned()));
    }
    tags
}

/// Contenu de la balise : `hardware.energy` / `hardware.solar`, comme le rapport d'Astroport.ONE. `None` sans mesure.
pub fn beacon_content(r: &Reading) -> Option<Value> {
    let mut hw = serde_json::Map::new();
    if let Some(c) = &r.consumption {
        hw.insert("energy".into(), json!({ "today_kwh": c.today_kwh, "month_kwh": c.month_kwh, "total_kwh": c.total_kwh, "avg_w_recent": c.avg_w_recent, "power_w": c.power_w, "days_recorded": c.days_recorded, "since": c.since }));
    }
    if let Some(p) = &r.production {
        hw.insert("solar".into(), json!({ "power_w": p.power_w.unwrap_or(0.0), "yield_today_wh": p.today_wh, "month_wh": p.month_wh, "total_wh": p.total_wh, "battery_v": p.battery_v, "charge_state": p.charge_state, "days_recorded": p.days_recorded }));
    }
    (!hw.is_empty()).then(|| json!({ "hardware": hw }))
}

/// Vue du cockpit : `{ state: absent|invalid|stale|fresh, error?, age_secs?, consumption?, production?, plugin }`.
pub fn view(e: &Energy) -> Value {
    let reading = |r: &Reading| json!({
        "generated_at": r.generated_at,
        "consumption": r.consumption.as_ref().map(|c| json!({ "source": c.source, "power_w": c.power_w, "today_kwh": c.today_kwh, "month_kwh": c.month_kwh, "total_kwh": c.total_kwh, "avg_w_recent": c.avg_w_recent, "since": c.since, "days_recorded": c.days_recorded })),
        "production": r.production.as_ref().map(|p| json!({ "source": p.source, "power_w": p.power_w, "battery_v": p.battery_v, "charge_state": p.charge_state, "devices": p.devices, "today_wh": p.today_wh, "month_wh": p.month_wh, "total_wh": p.total_wh })),
    });
    let mut v = match e {
        Energy::Absent => json!({ "state": "absent" }),
        Energy::Invalid(why) => json!({ "state": "invalid", "error": why }),
        Energy::Stale { age_secs, reading: r } => {
            let mut v = reading(r);
            v["state"] = json!("stale");
            v["age_secs"] = json!(age_secs);
            v
        }
        Energy::Fresh(r) => {
            let mut v = reading(r);
            v["state"] = json!("fresh");
            v
        }
    };
    v["plugin"] = json!("https://github.com/papiche/astroportd-plugins/tree/main/energy");
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"{"schema":"energy/v1","generated_at":1000,"consumption":{"source":"powerjoular","power_w":11.6,"last_sample_at":999,"today_kwh":0.13,"month_kwh":1.246,"total_kwh":21.25,"avg_w_recent":8.57,"since":"2026-07-16","days_recorded":105},"production":null}"#;

    #[test]
    fn a_fresh_reading_feeds_the_beacon_with_the_legacy_tag_names() {
        let dir = std::env::temp_dir().join(format!("astroportd-energy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read(&dir, 1000), Energy::Absent, "pas de plugin");
        std::fs::create_dir_all(dir.join("plugins/energy")).unwrap();
        std::fs::write(file(&dir), OK).unwrap();
        let fresh = read(&dir, 1200);
        assert!(matches!(fresh, Energy::Fresh(_)), "à jour");
        let Energy::Fresh(r) = fresh else { return };
        let tags = beacon_tags(&r);
        assert!(tags.contains(&vec!["hw:energy_today_kwh".to_owned(), "0.130".to_owned()]) && tags.contains(&vec!["hw:avg_w_recent".to_owned(), "8.6".to_owned()]));
        assert!(!tags.iter().any(|t| t[0].starts_with("hw:solar")), "pas de régulateur : pas de tags solaires");
        assert_eq!(beacon_content(&r).unwrap()["hardware"]["energy"]["avg_w_recent"], json!(8.57));
        assert!(matches!(read(&dir, 1000 + STALE_AFTER_SECS + 1), Energy::Stale { .. }), "collecteur arrêté depuis plus d'une heure");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_outside_the_contract_is_refused_with_its_reason() {
        assert!(parse(&OK.replace("energy/v1", "energy/v2")).unwrap_err().contains("energy/v2"));
        assert!(parse(&OK.replace("21.25", "-3")).unwrap_err().contains("total_kwh"));
        assert!(parse("{}").is_err());
        let both = OK.replace(r#""production":null"#, r#""production":{"source":"victron","power_w":170,"battery_v":13.1,"charge_state":"bulk","devices":2,"last_sample_at":990,"today_wh":950,"month_wh":9000,"total_wh":12000,"since":"2026-10-01","days_recorded":37}"#);
        let r = parse(&both).unwrap();
        assert!(beacon_tags(&r).contains(&vec!["hw:solar_yield_today_wh".to_owned(), "950".to_owned()]));
        assert_eq!(view(&Energy::Fresh(r))["production"]["devices"], json!(2));
    }
}
