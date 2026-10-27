//! Horloge solaire : prochain **20h12 heure solaire locale vraie** d'une station.
//!
//! L'heure solaire vraie suit le Soleil : midi solaire = passage au méridien.
//! `UTC(20h12 solaire) = minuit UTC + 20h12 − 4 min × longitude(°E) − équation du temps`.
//! L'équation du temps vient de la série de Spencer (1971), précise à ~30 s.
//! La latitude n'influe pas sur l'heure solaire vraie : elle est validée mais non utilisée.

use std::time::{SystemTime, UNIX_EPOCH};

/// Horloge système considérée comme non initialisée avant le 2026-01-01 00:00:00 UTC.
///
/// Un Raspberry Pi sans RTC démarre en 1970 : toute purge ou échéance calculée alors serait fausse.
pub const MIN_VALID_UNIX: u64 = 1_767_225_600;

/// 20h12 en secondes depuis minuit solaire.
const TARGET_SECS: f64 = (20 * 3600 + 12 * 60) as f64;
const SECS_PER_DAY: i64 = 86_400;

/// Erreurs d'horloge solaire.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SolarError {
    /// Horloge système antérieure à 2026 : RTC absente ou non synchronisée.
    #[error("horloge système invalide ({0} < {MIN_VALID_UNIX}) : RTC non initialisée, aucune échéance ne sera calculée")]
    ClockNotSane(u64),
    /// Coordonnées hors bornes.
    #[error("coordonnées GPS invalides (lat {lat}, lon {lon})")]
    BadCoordinates {
        /// Latitude fournie.
        lat: f64,
        /// Longitude fournie.
        lon: f64,
    },
    /// Horloge système illisible (antérieure à 1970).
    #[error("horloge système illisible")]
    ClockUnreadable,
}

/// Position GPS de la station, en degrés décimaux.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinates {
    lat: f64,
    lon: f64,
}

impl Coordinates {
    /// Valide latitude ∈ [-90, 90] et longitude ∈ [-180, 180].
    pub fn new(lat: f64, lon: f64) -> Result<Self, SolarError> {
        let ok = lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0;
        if ok { Ok(Self { lat, lon }) } else { Err(SolarError::BadCoordinates { lat, lon }) }
    }

    /// Latitude en degrés.
    pub fn lat(&self) -> f64 {
        self.lat
    }

    /// Longitude en degrés Est.
    pub fn lon(&self) -> f64 {
        self.lon
    }
}

/// Refuse une horloge antérieure à [`MIN_VALID_UNIX`].
pub fn ensure_clock_sane(now_unix: u64) -> Result<(), SolarError> {
    if now_unix < MIN_VALID_UNIX { Err(SolarError::ClockNotSane(now_unix)) } else { Ok(()) }
}

/// Heure système en secondes UNIX, après contrôle du garde-fou.
pub fn system_now_checked() -> Result<u64, SolarError> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| SolarError::ClockUnreadable)?.as_secs();
    ensure_clock_sane(now)?;
    Ok(now)
}

/// Équation du temps (Spencer) en minutes : positive quand le Soleil vrai est en avance sur le Soleil moyen.
///
/// `day_of_year` : 1 pour le 1er janvier ; `hour_utc` : heure UTC fractionnaire (corrige la dérive intrajournalière).
pub fn equation_of_time_minutes(day_of_year: u32, hour_utc: f64) -> f64 {
    let g = 2.0 * std::f64::consts::PI / 365.0 * (f64::from(day_of_year) - 1.0 + (hour_utc - 12.0) / 24.0);
    229.18
        * (0.000_075 + 0.001_868 * g.cos() - 0.032_077 * g.sin() - 0.014_615 * (2.0 * g).cos()
            - 0.040_849 * (2.0 * g).sin())
}

/// Jours depuis 1970-01-01 pour une date du calendrier grégorien (algorithme de H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Date civile `(année, mois, jour)` d'un nombre de jours depuis 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Instant UNIX (s) du 20h12 solaire vrai pour la **date solaire locale** `days_since_epoch`.
fn solar_instant(days_since_epoch: i64, coords: &Coordinates) -> i64 {
    let (y, _, _) = civil_from_days(days_since_epoch);
    let doy = (days_since_epoch - days_from_civil(y, 1, 1) + 1) as u32;
    let hour_utc = TARGET_SECS / 3600.0 - coords.lon / 15.0;
    let eot_secs = equation_of_time_minutes(doy, hour_utc) * 60.0;
    let secs = days_since_epoch as f64 * SECS_PER_DAY as f64 + TARGET_SECS - coords.lon * 240.0 - eot_secs;
    secs.round() as i64
}

/// Instant UNIX du 20h12 solaire vrai de la **date solaire locale** `solar_day` (jours depuis 1970-01-01 à la
/// longitude de la station). Sert au rattrapage : « la nuit de ce jour solaire est-elle échue ? ».
///
/// Échoue si l'horloge `now_unix` est antérieure à 2026 (garde-fou RTC).
pub fn solar_20h12_of(solar_day: u64, coords: &Coordinates, now_unix: u64) -> Result<u64, SolarError> {
    ensure_clock_sane(now_unix)?;
    let day = i64::try_from(solar_day).map_err(|_| SolarError::ClockUnreadable)?;
    Ok(solar_instant(day, coords).max(0) as u64)
}

/// Prochain 20h12 solaire vrai strictement postérieur à `now_unix`.
///
/// Échoue si l'horloge est antérieure à 2026 (garde-fou RTC).
pub fn next_20h12(now_unix: u64, coords: &Coordinates) -> Result<u64, SolarError> {
    ensure_clock_sane(now_unix)?;
    let now = now_unix as i64;
    let today = now.div_euclid(SECS_PER_DAY);
    // La longitude décale l'instant de ±12 h : trois dates solaires suffisent à encadrer `now`.
    for day in (today - 1)..=(today + 2) {
        let t = solar_instant(day, coords);
        if t > now {
            return Ok(t as u64);
        }
    }
    Err(SolarError::ClockUnreadable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unix(y: i64, m: i64, d: i64, hh: i64, mm: i64, ss: i64) -> u64 {
        (days_from_civil(y, m, d) * SECS_PER_DAY + hh * 3600 + mm * 60 + ss) as u64
    }

    fn doy(y: i64, m: i64, d: i64) -> u32 {
        (days_from_civil(y, m, d) - days_from_civil(y, 1, 1) + 1) as u32
    }

    #[test]
    fn calendar_conversions_are_exact() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 1, 1) as u64 * 86_400, MIN_VALID_UNIX);
        assert_eq!(civil_from_days(days_from_civil(2028, 2, 29)), (2028, 2, 29));
        assert_eq!(civil_from_days(days_from_civil(2026, 12, 31)), (2026, 12, 31));
        assert_eq!(doy(2026, 11, 3), 307);
        assert_eq!(doy(2028, 12, 31), 366);
    }

    #[test]
    fn equation_of_time_matches_known_extremes() {
        // Valeurs de référence NOAA/almanach : max ≈ +16,4 min vers le 3 novembre, min ≈ −14,2 min vers le 11 février.
        assert!((equation_of_time_minutes(doy(2026, 11, 3), 12.0) - 16.4).abs() < 0.5);
        assert!((equation_of_time_minutes(doy(2026, 2, 11), 12.0) + 14.2).abs() < 0.5);
        // Passages par zéro vers le 15 avril et le 25 décembre.
        assert!(equation_of_time_minutes(doy(2026, 4, 15), 12.0).abs() < 0.7);
        assert!(equation_of_time_minutes(doy(2026, 12, 25), 12.0).abs() < 0.7);
    }

    #[test]
    fn greenwich_20h12_is_shifted_by_the_equation_of_time() {
        let c = Coordinates::new(51.48, 0.0).unwrap();
        let now = unix(2026, 11, 3, 12, 0, 0);
        let got = next_20h12(now, &c).unwrap();
        // 20h12 − 16,4 min = 19h55m36 UTC
        let expected = unix(2026, 11, 3, 19, 55, 36);
        assert!(got.abs_diff(expected) < 45, "got {got} expected {expected}");
    }

    #[test]
    fn longitude_shifts_four_minutes_per_degree() {
        let now = unix(2026, 11, 3, 0, 0, 1);
        let east = next_20h12(now, &Coordinates::new(0.0, 90.0).unwrap()).unwrap();
        let expected = unix(2026, 11, 3, 13, 55, 36); // 20h12 − 6 h − 16,4 min
        assert!(east.abs_diff(expected) < 45);
        let west = next_20h12(now, &Coordinates::new(0.0, -90.0).unwrap()).unwrap();
        let expected = unix(2026, 11, 3, 1, 55, 36); // 20h12 + 6 h − 16,4 min, soit le soir du 2 novembre à 90° Ouest
        assert!(west.abs_diff(expected) < 45);
    }

    #[test]
    fn next_is_strictly_after_now_and_less_than_a_day_and_a_bit_away() {
        let c = Coordinates::new(43.6, 1.44).unwrap();
        let mut now = unix(2026, 3, 1, 0, 0, 0);
        for _ in 0..400 {
            let t = next_20h12(now, &c).unwrap();
            assert!(t > now);
            assert!(t - now <= 86_400 + 60, "écart {} à {now}", t - now);
            now += 21_617; // pas premier avec 86400 : balaie toutes les heures
        }
    }

    #[test]
    fn consecutive_runs_are_about_a_day_apart() {
        let c = Coordinates::new(-33.9, 151.2).unwrap(); // Sydney
        let first = next_20h12(unix(2026, 6, 1, 0, 0, 0), &c).unwrap();
        let second = next_20h12(first, &c).unwrap();
        assert!(second > first);
        assert!((second - first).abs_diff(86_400) < 60);
    }

    #[test]
    fn extreme_longitudes_work() {
        for lon in [-180.0, -179.9, 0.0, 179.9, 180.0] {
            let c = Coordinates::new(0.0, lon).unwrap();
            let now = unix(2026, 12, 31, 23, 59, 59);
            let t = next_20h12(now, &c).unwrap();
            assert!(t > now && t - now <= 86_400 + 60, "lon {lon}");
        }
    }

    #[test]
    fn rtc_guard_blocks_pre_2026_clocks() {
        let c = Coordinates::new(0.0, 0.0).unwrap();
        assert_eq!(ensure_clock_sane(0), Err(SolarError::ClockNotSane(0)));
        assert_eq!(next_20h12(MIN_VALID_UNIX - 1, &c), Err(SolarError::ClockNotSane(MIN_VALID_UNIX - 1)));
        assert!(ensure_clock_sane(MIN_VALID_UNIX).is_ok());
        assert!(next_20h12(MIN_VALID_UNIX, &c).is_ok());
    }

    #[test]
    fn coordinates_are_validated() {
        assert!(Coordinates::new(90.1, 0.0).is_err());
        assert!(Coordinates::new(0.0, 180.1).is_err());
        assert!(Coordinates::new(f64::NAN, 0.0).is_err());
        assert!(Coordinates::new(0.0, f64::INFINITY).is_err());
    }
}
