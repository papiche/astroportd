//! Règle des 3×1/3 (`ZEN.COOPERATIVE.3x1-3.sh`) sur un surplus d'exploitation hebdomadaire.
//!
//! Ordre d'application, en centimes entiers :
//!
//! 1. `IMPOT` : provision d'impôt sur les sociétés — marginale sur le résultat cumulé de l'exercice
//!    (taux réduit jusqu'au seuil, taux normal au-delà) ;
//! 2. `NET` = surplus − provision ;
//! 3. prime du Capitaine = 1 % du `NET` (arrondie à l'inférieur) ;
//! 4. le reste est coupé en trois : `CASH` (trésorerie), `RnD` (recherche & développement), `ASSETS` (actifs durables).
//!
//! Les centimes d'arrondi (au plus 2) vont à `CASH`. Invariant vérifié par les tests :
//! `impôt + prime + CASH + RnD + ASSETS = surplus`, au centime.

use astro_core::Amount;

/// Réglages de la ventilation (points de base : 100 = 1 %).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitConfig {
    /// Prime de gestion du Capitaine, sur le `NET`.
    pub captain_bonus_bp: u32,
    /// Taux d'IS jusqu'au seuil (15 % = 1500).
    pub is_low_bp: u32,
    /// Taux d'IS au-delà du seuil (25 % = 2500).
    pub is_high_bp: u32,
    /// Seuil annuel du taux réduit (42 500 ẐEN).
    pub is_threshold: Amount,
}

impl Default for SplitConfig {
    fn default() -> Self {
        Self { captain_bonus_bp: 100, is_low_bp: 1500, is_high_bp: 2500, is_threshold: Amount(4_250_000) }
    }
}

/// Erreurs de ventilation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SplitError {
    /// Un taux dépasse 100 %.
    #[error("taux invalide (> 10000 points de base)")]
    BadRate,
    /// Dépassement arithmétique.
    #[error("dépassement de capacité")]
    Overflow,
}

/// Résultat de la ventilation d'un surplus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    /// Semaine ISO (`AAAA-Www`).
    pub week: String,
    /// Surplus réparti.
    pub surplus: Amount,
    /// Provision d'impôt sur les sociétés (`IMPOT`).
    pub is_provision: Amount,
    /// Surplus après provision.
    pub net: Amount,
    /// Prime du Capitaine.
    pub captain_bonus: Amount,
    /// Trésorerie opérationnelle (`CASH`).
    pub cash: Amount,
    /// Recherche & développement (`RnD`).
    pub rnd: Amount,
    /// Actifs durables (`ASSETS`).
    pub assets: Amount,
}

impl Allocation {
    /// Somme des parts : doit toujours égaler le surplus.
    pub fn total(&self) -> u64 {
        self.is_provision.0 + self.captain_bonus.0 + self.cash.0 + self.rnd.0 + self.assets.0
    }

    /// JSON de journal (montants en texte décimal exact).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "week": self.week, "surplus": self.surplus.to_string(), "is_provision": self.is_provision.to_string(),
            "net": self.net.to_string(), "captain_bonus": self.captain_bonus.to_string(),
            "cash": self.cash.to_string(), "rnd": self.rnd.to_string(), "assets": self.assets.to_string(),
        })
    }
}

impl SplitConfig {
    fn check(&self) -> Result<(), SplitError> {
        if [self.captain_bonus_bp, self.is_low_bp, self.is_high_bp].iter().any(|bp| *bp > 10_000) {
            return Err(SplitError::BadRate);
        }
        Ok(())
    }

    /// Impôt dû sur un résultat cumulé `profit` (barème à deux tranches, arrondi à l'inférieur).
    pub fn is_due(&self, profit: Amount) -> Amount {
        let low = profit.0.min(self.is_threshold.0);
        let high = profit.0.saturating_sub(self.is_threshold.0);
        let tax = (u128::from(low) * u128::from(self.is_low_bp) + u128::from(high) * u128::from(self.is_high_bp)) / 10_000;
        Amount(u64::try_from(tax).unwrap_or(u64::MAX))
    }

    /// Ventile `surplus`. `ytd_profit` : résultat déjà réalisé sur l'exercice avant cette semaine (pour la tranche d'IS).
    pub fn allocate(&self, week: &str, surplus: Amount, ytd_profit: Amount) -> Result<Allocation, SplitError> {
        self.check()?;
        let after = ytd_profit.checked_add(surplus).map_err(|_| SplitError::Overflow)?;
        let is_provision = Amount(self.is_due(after).0 - self.is_due(ytd_profit).0).min(surplus);
        let net = Amount(surplus.0 - is_provision.0);
        let captain_bonus = Amount(u64::try_from(u128::from(net.0) * u128::from(self.captain_bonus_bp) / 10_000).map_err(|_| SplitError::Overflow)?);
        let rest = net.0 - captain_bonus.0;
        let third = rest / 3;
        Ok(Allocation {
            week: week.to_owned(),
            surplus,
            is_provision,
            net,
            captain_bonus,
            cash: Amount(rest - 2 * third),
            rnd: Amount(third),
            assets: Amount(third),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_surplus_splits_into_the_documented_shares() {
        // 1000 ẐEN, premier euro de l'exercice : IS 15 % = 150, NET 850, prime 1 % = 8,50, reste 841,50 = 3 × 280,50.
        let a = SplitConfig::default().allocate("2026-W44", Amount(100_000), Amount(0)).unwrap();
        assert_eq!((a.is_provision, a.net, a.captain_bonus), (Amount(15_000), Amount(85_000), Amount(850)));
        assert_eq!((a.cash, a.rnd, a.assets), (Amount(28_050), Amount(28_050), Amount(28_050)));
        assert_eq!(a.total(), 100_000);
    }

    #[test]
    fn the_rounding_residue_goes_to_cash_and_nothing_is_ever_lost() {
        let cfg = SplitConfig::default();
        for surplus in [0u64, 1, 2, 3, 7, 99, 100, 101, 12_345, 999_999, 4_249_999, 4_250_001, 10_000_000] {
            for ytd in [0u64, 4_249_900, 4_250_000, 9_000_000] {
                let a = cfg.allocate("w", Amount(surplus), Amount(ytd)).unwrap();
                assert_eq!(a.total(), surplus, "surplus {surplus}, cumul {ytd}");
                assert!(a.cash.0 >= a.rnd.0 && a.cash.0 - a.rnd.0 <= 2, "résidu ≤ 2 centimes sur CASH");
                assert_eq!(a.rnd, a.assets);
                assert_eq!(a.net.0, surplus - a.is_provision.0);
            }
        }
    }

    #[test]
    fn corporate_tax_is_marginal_across_the_threshold() {
        let cfg = SplitConfig::default();
        // Cumul 42 000 ẐEN + surplus 1 000 : 500 au taux réduit (75) + 500 au taux normal (125) = 200.
        let a = cfg.allocate("w", Amount(100_000), Amount(4_200_000)).unwrap();
        assert_eq!(a.is_provision, Amount(20_000));
        // Entièrement au-delà du seuil : 25 %.
        assert_eq!(cfg.allocate("w", Amount(100_000), Amount(5_000_000)).unwrap().is_provision, Amount(25_000));
        assert_eq!(cfg.is_due(Amount(4_250_000)), Amount(637_500));
    }

    #[test]
    fn rates_are_configurable_and_validated() {
        let cfg = SplitConfig { captain_bonus_bp: 0, is_low_bp: 0, is_high_bp: 0, is_threshold: Amount(0) };
        let a = cfg.allocate("w", Amount(300), Amount(0)).unwrap();
        assert_eq!((a.captain_bonus, a.cash, a.rnd, a.assets), (Amount(0), Amount(100), Amount(100), Amount(100)));
        assert_eq!(SplitConfig { is_low_bp: 10_001, ..SplitConfig::default() }.allocate("w", Amount(1), Amount(0)), Err(SplitError::BadRate));
        assert_eq!(SplitConfig::default().allocate("w", Amount(u64::MAX), Amount(1)), Err(SplitError::Overflow));
    }
}
