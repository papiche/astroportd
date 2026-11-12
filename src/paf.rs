//! **PAF de la machine** (participation aux frais) : propre à chaque station, fixée par son Capitaine.
//!
//! La PAF est une charge hebdomadaire en Ẑ : 1×PAF revient au NODE (l'Armateur, qui prête la machine), 2×PAF au MULTIPASS du
//! Capitaine (l'administrateur système). La station règle donc chaque semaine **3×PAF** (voir [`crate::settlement`]).
//!
//! Elle est lue dans `station.conf` (`paf = 14` ou `paf = 14.50`, en Ẑ par semaine). Valeur absente : pas de PAF, l'économie
//! de la station reste coupée. L'option `--weekly-costs` de `serve` (en centimes, déjà 3×PAF) l'emporte si elle est donnée.
//! La coopérative peut publier une PAF par défaut (réglage `PAF`) : elle sert de point de départ, jamais d'ordre.
//!
//! Au Capitaine qui rejoint une constellation, on **propose** une PAF d'après ce que la machine apporte :
//!
//! - l'énergie qu'elle consomme (mesurée par le plugin `energy`, sinon estimée d'après son profil) au prix du kWh ;
//! - sa capacité de calcul et de stockage, d'après son Power-Score : 1 Ẑ par tranche de 10 points.
//!
//! - l'**effort d'autosuffisance** quand un régulateur solaire (plugin `victron`) produit : l'énergie gratuite ne vient plus en
//!   déduction, elle **hausse** la PAF (+3 Ẑ par tiers de la consommation couverte, 2 tiers au plus), et le **Bingo résilience**
//!   (+3 Ẑ de plus) tombe quand la production du mois dépasse la consommation.
//!
//! Les PAF vont par **paliers de 3 Ẑ** (le tiers qui revient au NODE reste un montant rond) ; la PAF par défaut est de 9 Ẑ.
//! C'est une estimation, arrondie au palier supérieur ; le Capitaine la confirme ou la change.

use std::path::Path;

use astro_p2p::routing::{Hardware, Tier};
use serde_json::{Value, json};

/// Plafond d'une PAF hebdomadaire : 1 000 Ẑ (garde-fou contre une faute de frappe).
pub const MAX_PAF_CENTS: u64 = 100_000;
/// Heures d'une semaine.
const WEEK_HOURS: u64 = 168;
/// Palier de la PAF (Ẑ en centimes) : les montants sont des multiples de 3 Ẑ.
pub const STEP_CENTS: u64 = 300;
/// PAF par défaut (et plancher de l'estimation) : 9 Ẑ par semaine.
pub const DEFAULT_PAF_CENTS: u64 = 900;
/// Mesures solaires et de consommation du mois, en dessous desquelles on ne juge pas (Wh).
const MIN_WINDOW_WH: u64 = 1_000;
/// Prix du kWh retenu pour l'estimation, en centimes de Ẑ (0,20 €).
pub const KWH_PRICE_CENTS: u64 = 20;

/// Puissance moyenne supposée sans mesure, selon le profil (W).
pub fn assumed_watts(tier: Tier) -> u64 {
    match tier {
        Tier::Light => 5,
        Tier::Standard => 40,
        Tier::Brain => 150,
    }
}

/// Montant décimal (`14`, `14.5`, `14.50`) en centimes ; `None` si autre chose (pas de signe, pas de virgule, 2 décimales au plus).
pub fn cents_of(text: &str) -> Option<u64> {
    let (int, frac) = text.trim().split_once('.').unwrap_or((text.trim(), ""));
    if int.is_empty() || frac.len() > 2 || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let frac2: String = frac.chars().chain("00".chars()).take(2).collect();
    int.parse::<u64>().ok()?.checked_mul(100)?.checked_add(frac2.parse::<u64>().ok()?)
}

/// Centimes en texte (`1450` → `14.50`).
pub fn text_of(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// PAF enregistrée dans `station.conf` (centimes de Ẑ par semaine) ; `None` si absente, illisible ou nulle.
pub fn from_conf(conf: &Path) -> Option<u64> {
    crate::init::read_conf(conf).into_iter().find(|(k, _)| k == "paf").and_then(|(_, v)| cents_of(&v)).filter(|c| *c > 0)
}

/// Enregistre la PAF dans `station.conf` ; `0` la retire (économie coupée).
pub fn save(conf: &Path, cents: u64) -> Result<(), String> {
    if cents > MAX_PAF_CENTS {
        return Err(format!("PAF trop élevée : {} Ẑ par semaine au plus", MAX_PAF_CENTS / 100));
    }
    if cents % STEP_CENTS != 0 {
        return Err(format!("la PAF va par paliers de {} Ẑ (3, 6, 9, 12…) : {} Ẑ n'en est pas un", STEP_CENTS / 100, text_of(cents)));
    }
    crate::station_domain::set_conf_value(conf, "paf", &text_of(cents)).map_err(|e| format!("station.conf : {e}"))
}

/// Charge hebdomadaire réglée par la station (3×PAF), en centimes : `flag` (`--weekly-costs`) d'abord, sinon `station.conf`.
pub fn weekly_costs(flag: u64, conf: &Path) -> u64 {
    if flag > 0 { flag } else { from_conf(conf).map_or(0, |p| p.saturating_mul(3)) }
}

/// Production solaire et consommation du mois en cours, mesurées sur la même durée (Wh).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Solar {
    /// Production du mois (plugin `victron`).
    pub production_wh: f64,
    /// Consommation du mois (PowerJoular).
    pub consumption_wh: f64,
}

/// PAF proposée et ses éléments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// PAF proposée (centimes de Ẑ par semaine, par paliers de 3 Ẑ, 9 Ẑ au moins).
    pub paf_cents: u64,
    /// Part de l'énergie (centimes).
    pub energy_cents: u64,
    /// Part de la capacité (centimes).
    pub capacity_cents: u64,
    /// Effort d'autosuffisance (centimes) : +3 Ẑ par tiers de la consommation couverte par le solaire, 2 tiers au plus.
    pub solar_cents: u64,
    /// Bingo résilience (centimes) : la production du mois dépasse la consommation.
    pub bingo_cents: u64,
    /// Part de la consommation couverte par le solaire, en %, plafonnée à 100 (`None` : pas de mesure solaire jugeable).
    pub coverage_pct: Option<u64>,
    /// Puissance moyenne retenue (W).
    pub watts: u64,
    /// `measured` (plugin `energy`) ou `assumed` (profil).
    pub watts_source: &'static str,
}

/// Effort d'autosuffisance et Bingo d'après la production et la consommation du mois : `(solar, bingo, couverture %)`.
pub fn solar_bonus(solar: Option<Solar>) -> (u64, u64, Option<u64>) {
    let Some(s) = solar.filter(|s| s.production_wh.is_finite() && s.consumption_wh.is_finite() && s.production_wh > 0.0) else { return (0, 0, None) };
    let (prod, cons) = (s.production_wh.round() as u64, s.consumption_wh.round() as u64);
    if cons < MIN_WINDOW_WH {
        return (0, 0, None);
    }
    let thirds = (prod.saturating_mul(3) / cons).min(2);
    let bingo = if prod > cons { STEP_CENTS } else { 0 };
    (thirds * STEP_CENTS, bingo, Some((prod.saturating_mul(100) / cons).min(100)))
}

/// Estime la PAF d'une machine. `measured_w` : puissance moyenne mesurée (plugin `energy`) ; `solar` : mois solaire (plugin `victron`).
pub fn suggest(hw: &Hardware, measured_w: Option<f64>, solar: Option<Solar>) -> Suggestion {
    let measured = measured_w.filter(|w| w.is_finite() && *w > 0.0);
    // Nombre entier de watts : la précision d'une estimation ne va pas plus loin.
    let watts = measured.map_or_else(|| assumed_watts(hw.tier()), |w| w.round().max(1.0) as u64);
    let energy_cents = (watts * WEEK_HOURS * KWH_PRICE_CENTS).div_ceil(1000);
    let capacity_cents = (hw.score().max(0.0).round() as u64) * 10;
    let (solar_cents, bingo_cents, coverage_pct) = solar_bonus(solar);
    let paf_cents = (energy_cents + capacity_cents + solar_cents + bingo_cents).div_ceil(STEP_CENTS).max(DEFAULT_PAF_CENTS / STEP_CENTS) * STEP_CENTS;
    Suggestion { paf_cents, energy_cents, capacity_cents, solar_cents, bingo_cents, coverage_pct, watts, watts_source: if measured.is_some() { "measured" } else { "assumed" } }
}

/// Vue pour l'écran du Capitaine. `active` : charge hebdomadaire (3×PAF) réglée depuis le démarrage de la station.
pub fn view(conf: &Path, active: u64, hw: &Hardware, measured_w: Option<f64>, solar: Option<Solar>, coop_default: Option<&str>) -> Value {
    let s = suggest(hw, measured_w, solar);
    let paf = from_conf(conf);
    let saved_costs = paf.map_or(0, |p| p.saturating_mul(3));
    let source = if active == 0 { "none" } else if active == saved_costs { "conf" } else { "flag" };
    json!({
        "paf_cents": paf,
        "weekly_costs_cents": active,
        "source": source,
        // Enregistrée mais pas encore réglée : la station lit la PAF au démarrage.
        "pending_restart": source != "flag" && saved_costs != active,
        "max_cents": MAX_PAF_CENTS,
        "coop_default_cents": coop_default.and_then(cents_of).or(Some(DEFAULT_PAF_CENTS)),
        "step_cents": STEP_CENTS,
        "suggestion": {
            "paf_cents": s.paf_cents, "energy_cents": s.energy_cents, "capacity_cents": s.capacity_cents,
            "solar_cents": s.solar_cents, "bingo_cents": s.bingo_cents, "coverage_pct": s.coverage_pct,
            "watts": s.watts, "watts_source": s.watts_source, "kwh_price_cents": KWH_PRICE_CENTS,
            "power_score": (hw.score() * 10.0).round() / 10.0, "tier": hw.tier().label(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(vram: f64, cores: u32, ram: f64) -> Hardware {
        Hardware { vram_gb: vram, cpu_cores: cores, ram_gb: ram }
    }

    #[test]
    fn montants_decimaux() {
        assert_eq!(cents_of("14"), Some(1400));
        assert_eq!(cents_of("14.5"), Some(1450));
        assert_eq!(cents_of(" 0.05 "), Some(5));
        assert_eq!(cents_of("14,5"), None);
        assert_eq!(cents_of("-3"), None);
        assert_eq!(cents_of("1.234"), None);
        assert_eq!(cents_of(""), None);
        assert_eq!(text_of(1450), "14.50");
        assert_eq!(text_of(5), "0.05");
    }

    #[test]
    fn conf_lue_ecrite_et_charges() {
        let dir = std::env::temp_dir().join(format!("paf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("station.conf");
        std::fs::write(&conf, "mode = origin\n").unwrap();
        assert_eq!(from_conf(&conf), None);
        assert_eq!(weekly_costs(0, &conf), 0, "sans PAF, l'économie reste coupée");
        save(&conf, 1500).unwrap();
        assert_eq!(from_conf(&conf), Some(1500));
        assert_eq!(weekly_costs(0, &conf), 4500, "3×PAF");
        assert_eq!(weekly_costs(999, &conf), 999, "l'option de la commande l'emporte");
        assert!(std::fs::read_to_string(&conf).unwrap().contains("mode = origin"), "le reste du fichier est intact");
        save(&conf, 0).unwrap();
        assert_eq!(from_conf(&conf), None, "0 retire la PAF");
        assert!(save(&conf, MAX_PAF_CENTS + 1).is_err());
        assert!(save(&conf, 1000).unwrap_err().contains("paliers"), "10 Ẑ n'est pas un palier de 3");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn estimation_mesuree_et_supposee() {
        // 40 W mesurés : 40×168×20/1000 = 134,4 → 135 centimes ; Power-Score ≈ capacité.
        let h = hw(0.0, 12, 39.0);
        let s = suggest(&h, Some(40.0), None);
        assert_eq!((s.watts, s.watts_source, s.energy_cents), (40, "measured", 135));
        assert_eq!(s.paf_cents % STEP_CENTS, 0, "par paliers de 3 Ẑ");
        assert!(s.paf_cents >= s.energy_cents + s.capacity_cents);
        let a = suggest(&h, None, None);
        assert_eq!(a.watts_source, "assumed");
        assert_eq!(a.watts, assumed_watts(h.tier()));
        // Une mesure absurde est ignorée.
        assert_eq!(suggest(&h, Some(f64::NAN), None).watts_source, "assumed");
        assert_eq!(suggest(&h, Some(0.0), None).watts_source, "assumed");
    }

    #[test]
    fn la_vue_dit_d_ou_vient_la_charge_et_si_un_redemarrage_attend() {
        let dir = std::env::temp_dir().join(format!("paf-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("station.conf");
        let h = hw(0.0, 4, 8.0);
        let v = view(&conf, 0, &h, None, None, Some("15"));
        assert_eq!((v["source"].as_str(), v["pending_restart"].as_bool(), v["coop_default_cents"].as_u64()), (Some("none"), Some(false), Some(1500)));
        save(&conf, 900).unwrap();
        let v = view(&conf, 0, &h, None, None, None);
        assert_eq!(v["pending_restart"], true, "PAF enregistrée, pas encore réglée");
        let v = view(&conf, 2700, &h, None, None, None);
        assert_eq!((v["source"].as_str(), v["pending_restart"].as_bool()), (Some("conf"), Some(false)));
        let v = view(&conf, 777, &h, None, None, None);
        assert_eq!(v["source"], "flag", "--weekly-costs a la priorité");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn une_petite_machine_paie_moins_qu_un_serveur_gpu() {
        let pi = suggest(&hw(0.0, 4, 4.0), None, None);
        let gpu = suggest(&hw(24.0, 16, 64.0), None, None);
        assert!(pi.paf_cents < gpu.paf_cents);
        assert_eq!(pi.paf_cents, DEFAULT_PAF_CENTS, "la plus petite machine paie la PAF par défaut : 9 Ẑ");
    }

    #[test]
    fn le_solaire_hausse_la_paf_et_le_bingo_recompense_la_resilience() {
        let wh = |p: f64, c: f64| Some(Solar { production_wh: p, consumption_wh: c });
        assert_eq!(solar_bonus(None), (0, 0, None));
        assert_eq!(solar_bonus(wh(0.0, 30_000.0)), (0, 0, None), "pas de production : rien");
        assert_eq!(solar_bonus(wh(5_000.0, 500.0)), (0, 0, None), "fenêtre trop courte : on ne juge pas");
        assert_eq!(solar_bonus(wh(5_000.0, 30_000.0)), (0, 0, Some(16)), "moins d'un tiers couvert");
        assert_eq!(solar_bonus(wh(10_000.0, 30_000.0)), (300, 0, Some(33)), "un tiers : +3 Ẑ");
        assert_eq!(solar_bonus(wh(20_000.0, 30_000.0)), (600, 0, Some(66)), "deux tiers : +6 Ẑ");
        assert_eq!(solar_bonus(wh(30_000.0, 30_000.0)), (600, 0, Some(100)), "égalité : pas de Bingo, il faut dépasser");
        assert_eq!(solar_bonus(wh(45_000.0, 30_000.0)), (600, 300, Some(100)), "Bingo résilience : +3 Ẑ de plus");
        let h = hw(0.0, 12, 39.0);
        let sans = suggest(&h, Some(40.0), None);
        let avec = suggest(&h, Some(40.0), wh(45_000.0, 30_000.0));
        assert!(avec.paf_cents >= sans.paf_cents + 900 - STEP_CENTS, "le solaire fait monter la PAF d'au plus 9 Ẑ : {} -> {}", sans.paf_cents, avec.paf_cents);
        assert!(avec.paf_cents > sans.paf_cents);
        assert_eq!(avec.paf_cents % STEP_CENTS, 0);
    }
}
