//! Clôture hebdomadaire et « Love Ledger » : la station ne s'arrête jamais faute de trésorerie.
//!
//! Chaque semaine, [`close_week`] compare les recettes aux charges :
//!
//! - recettes ≥ charges → [`WeekOutcome::Split`] : le surplus est ventilé par la règle des 3×1/3 ;
//! - recettes < charges mais la trésorerie couvre l'écart → [`WeekOutcome::Drawdown`] : pas de ventilation, la trésorerie baisse ;
//! - sinon → [`WeekOutcome::Volunteer`] : *Bénévolat Actif*. Le Capitaine comble le déficit sur ses deniers ; l'apport est
//!   inscrit dans `love_ledger` et reconnu par une attestation TrocZen (kind 30305).

use astro_core::Amount;
use astro_core::week::date_string;
use astro_nostr_proto::UnsignedEvent;

use crate::split::{Allocation, SplitConfig, SplitError};

/// Kind de l'attestation de reconnaissance TrocZen.
pub const KIND_TROCZEN_ATTESTATION: u32 = 30305;

/// Données d'une clôture.
#[derive(Debug, Clone)]
pub struct WeekInput {
    /// Semaine ISO (`AAAA-Www`).
    pub week: String,
    /// Recettes de la semaine (loyers, contributions).
    pub revenue: Amount,
    /// Charges de la semaine (PAF, factures).
    pub costs: Amount,
    /// Trésorerie disponible avant la clôture (`CASH`).
    pub cash_balance: Amount,
    /// Résultat cumulé de l'exercice avant cette semaine.
    pub ytd_profit: Amount,
}

/// Semaine financée par le Capitaine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolunteerWeek {
    /// Semaine ISO.
    pub week: String,
    /// Déficit couvert par le Capitaine.
    pub deficit: Amount,
}

/// Issue d'une clôture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeekOutcome {
    /// Surplus ventilé (peut être nul).
    Split(Allocation),
    /// Écart absorbé par la trésorerie.
    Drawdown {
        /// Semaine ISO.
        week: String,
        /// Montant prélevé sur la trésorerie.
        gap: Amount,
    },
    /// Bascule en Bénévolat Actif.
    Volunteer(VolunteerWeek),
}

/// Clôture une semaine.
pub fn close_week(cfg: &SplitConfig, input: &WeekInput) -> Result<WeekOutcome, SplitError> {
    match input.revenue.checked_sub(input.costs) {
        Some(surplus) => Ok(WeekOutcome::Split(cfg.allocate(&input.week, surplus, input.ytd_profit)?)),
        None => {
            let gap = Amount(input.costs.0 - input.revenue.0);
            Ok(match input.cash_balance.checked_sub(gap) {
                Some(_) => WeekOutcome::Drawdown { week: input.week.clone(), gap },
                None => WeekOutcome::Volunteer(VolunteerWeek { week: input.week.clone(), deficit: Amount(gap.0 - input.cash_balance.0) }),
            })
        }
    }
}

/// Attestation de reconnaissance émise à la station pour un apport bénévole du Capitaine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attestation {
    /// `d` du kind 30305 : `du-AAAA-MM-JJ`.
    pub d: String,
    /// Bénéficiaire (clé du Capitaine, 64 hex).
    pub captain_hex: String,
    /// Montant reconnu.
    pub amount: Amount,
    /// Semaine concernée.
    pub week: String,
}

impl Attestation {
    /// Attestation pour `week` à la date `at`.
    pub fn new(captain_hex: &str, week: &VolunteerWeek, at: u64) -> Self {
        Self { d: format!("du-{}", date_string(at)), captain_hex: captain_hex.to_owned(), amount: week.deficit, week: week.week.clone() }
    }

    /// Modèle d'événement 30305 à signer **par la station** (clé NODE) : c'est la coopérative qui reconnaît l'apport,
    /// pas le Capitaine qui s'auto-attesterait. Remplaçable par `d` : une seule attestation par jour.
    pub fn unsigned(&self, at: u64) -> UnsignedEvent {
        UnsignedEvent {
            created_at: at,
            kind: KIND_TROCZEN_ATTESTATION,
            tags: vec![
                vec!["d".into(), self.d.clone()],
                vec!["p".into(), self.captain_hex.clone()],
                vec!["amount".into(), self.amount.to_string()],
                vec!["week".into(), self.week.clone()],
                vec!["t".into(), "benevolat-actif".into()],
            ],
            content: format!("Reconnaissance d'un apport bénévole de {} ẐEN ({})", self.amount, self.week),
        }
    }
}

#[cfg(test)]
mod tests {
    use astro_nostr_proto::pubkey_of;

    use super::*;

    fn input(revenue: u64, costs: u64, cash: u64) -> WeekInput {
        WeekInput { week: "2026-W44".into(), revenue: Amount(revenue), costs: Amount(costs), cash_balance: Amount(cash), ytd_profit: Amount(0) }
    }

    #[test]
    fn a_surplus_is_split_and_a_break_even_week_is_an_empty_split() {
        let cfg = SplitConfig::default();
        assert!(matches!(close_week(&cfg, &input(50_000, 20_000, 0)).unwrap(), WeekOutcome::Split(a) if a.surplus == Amount(30_000) && a.total() == 30_000));
        assert!(matches!(close_week(&cfg, &input(20_000, 20_000, 0)).unwrap(), WeekOutcome::Split(a) if a.total() == 0));
    }

    #[test]
    fn a_shortfall_covered_by_cash_is_a_drawdown_not_volunteering() {
        let out = close_week(&SplitConfig::default(), &input(10_000, 15_000, 5_000)).unwrap();
        assert_eq!(out, WeekOutcome::Drawdown { week: "2026-W44".into(), gap: Amount(5_000) });
    }

    #[test]
    fn when_cash_cannot_cover_the_gap_the_captain_funds_exactly_the_remainder() {
        let out = close_week(&SplitConfig::default(), &input(10_000, 15_000, 1_200)).unwrap();
        assert_eq!(out, WeekOutcome::Volunteer(VolunteerWeek { week: "2026-W44".into(), deficit: Amount(3_800) }));
        let none = close_week(&SplitConfig::default(), &input(0, 15_000, 0)).unwrap();
        assert_eq!(none, WeekOutcome::Volunteer(VolunteerWeek { week: "2026-W44".into(), deficit: Amount(15_000) }));
    }

    #[test]
    fn the_attestation_is_a_30305_replaceable_per_day_signed_by_the_station() {
        let captain = pubkey_of(&[2; 32]).unwrap();
        let week = VolunteerWeek { week: "2026-W44".into(), deficit: Amount(3_800) };
        let at = 1_792_000_000; // 2026-10-14
        let att = Attestation::new(&captain, &week, at);
        assert_eq!(att.d, "du-2026-10-14");
        let ev = att.unsigned(at).sign(&[9; 32]).unwrap();
        ev.verify().unwrap();
        assert_eq!(ev.kind, 30305);
        assert_eq!((ev.tag_value("d"), ev.tag_value("p"), ev.tag_value("amount")), (Some("du-2026-10-14"), Some(captain.as_str()), Some("38.00")));
        assert_ne!(ev.pubkey, captain, "émise par la station, pas par le Capitaine");
    }
}
