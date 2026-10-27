//! Montants en centimes de Ğ1 (2 décimales). Aucun flottant : `"12.5"` → 1250.

use std::fmt;
use std::str::FromStr;

/// Erreurs de montant.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AmountError {
    /// Format invalide (signe, exposant, plus de 2 décimales…).
    #[error("montant invalide : {0}")]
    Invalid(String),
    /// Dépassement de capacité.
    #[error("montant trop grand")]
    Overflow,
}

/// Montant en centimes de Ğ1 (1 Ğ1 = 100).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Amount(pub u64);

impl Amount {
    /// Nul.
    pub const ZERO: Amount = Amount(0);

    /// Somme sans dépassement.
    pub fn checked_add(self, o: Amount) -> Result<Amount, AmountError> {
        self.0.checked_add(o.0).map(Amount).ok_or(AmountError::Overflow)
    }

    /// Différence ; `None` si négative (jamais de découvert).
    pub fn checked_sub(self, o: Amount) -> Option<Amount> {
        self.0.checked_sub(o.0).map(Amount)
    }
}

impl FromStr for Amount {
    type Err = AmountError;

    /// `^[0-9]+(\.[0-9]{1,2})?$`.
    fn from_str(s: &str) -> Result<Self, AmountError> {
        let bad = || AmountError::Invalid(s.to_owned());
        let (int, frac) = s.split_once('.').map_or((s, ""), |(i, f)| (i, f));
        let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
        if int.is_empty() || !digits(int) || !digits(frac) || frac.len() > 2 || (s.contains('.') && frac.is_empty()) {
            return Err(bad());
        }
        let whole: u64 = int.parse().map_err(|_| AmountError::Overflow)?;
        let cents: u64 = format!("{frac:0<2}").parse().map_err(|_| bad())?;
        whole.checked_mul(100).and_then(|w| w.checked_add(cents)).map(Amount).ok_or(AmountError::Overflow)
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}", self.0 / 100, self.0 % 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exactly_and_round_trips() {
        for (s, c) in [("0", 0), ("1", 100), ("12.5", 1250), ("12.50", 1250), ("0.07", 7), ("100.01", 10_001)] {
            let a: Amount = s.parse().unwrap();
            assert_eq!(a.0, c, "{s}");
        }
        assert_eq!(Amount(1250).to_string(), "12.50");
        assert_eq!(Amount(7).to_string(), "0.07");
        assert_eq!("12.5".parse::<Amount>().unwrap().to_string().parse::<Amount>().unwrap(), Amount(1250));
    }

    #[test]
    fn rejects_everything_that_is_not_a_plain_decimal() {
        for bad in ["", ".5", "5.", "1.234", "-1", "+1", "1e3", "1,5", "abc", "1.2.3", " 1"] {
            assert!(bad.parse::<Amount>().is_err(), "{bad:?}");
        }
        assert_eq!("99999999999999999999".parse::<Amount>(), Err(AmountError::Overflow));
        assert_eq!(Amount(u64::MAX).checked_add(Amount(1)), Err(AmountError::Overflow));
        assert_eq!(Amount(5).checked_sub(Amount(6)), None);
    }
}
