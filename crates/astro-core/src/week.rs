//! Calendrier civil en UTC : semaine ISO et date `AAAA-MM-JJ`, sans dépendance externe.

/// Jours depuis 1970-01-01 → (année, mois 1-12, jour 1-31) (algorithme de H. Hinnant).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// 1970-01-01 → jours, réciproque de [`civil_from_days`].
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `AAAA-MM-JJ` (UTC) d'un horodatage UNIX.
pub fn date_string(ts: u64) -> String {
    let (y, m, d) = civil_from_days((ts / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Semaine ISO 8601 (`AAAA-Www`, UTC) : le jeudi de la semaine donne l'année et le numéro.
pub fn iso_week(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let weekday = (days + 3).rem_euclid(7); // lundi = 0 (le 1970-01-01 était un jeudi)
    let thursday = days - weekday + 3;
    let (year, _, _) = civil_from_days(thursday);
    let ordinal = thursday - days_from_civil(year, 1, 1);
    format!("{year:04}-W{:02}", ordinal / 7 + 1)
}

/// Lundi 00:00 UTC de la semaine ISO qui contient `ts`.
pub fn week_start(ts: u64) -> u64 {
    let days = (ts / 86_400) as i64;
    let weekday = (days + 3).rem_euclid(7); // lundi = 0
    ((days - weekday) as u64) * 86_400
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn week_start_is_the_monday_midnight() {
        let monday = days_from_civil(2026, 10, 26) as u64 * 86_400;
        assert_eq!(iso_week(monday), "2026-W44");
        for off in [0, 1, 3 * 86_400 + 77, 7 * 86_400 - 1] {
            assert_eq!(week_start(monday + off), monday);
        }
        assert_eq!(week_start(monday + 7 * 86_400), monday + 7 * 86_400);
    }

    #[test]
    fn dates_and_iso_weeks_match_the_calendar() {
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(date_string(1_792_000_000), "2026-10-14");
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        // Cas limites connus : 2020-12-31 est en S53, 2021-01-03 aussi, 2021-01-04 ouvre la S01 ; 2024-12-30 est en 2025-W01.
        let ts = |y, m, d| days_from_civil(y, m, d) as u64 * 86_400 + 12 * 3600;
        assert_eq!(iso_week(ts(2020, 12, 31)), "2020-W53");
        assert_eq!(iso_week(ts(2021, 1, 3)), "2020-W53");
        assert_eq!(iso_week(ts(2021, 1, 4)), "2021-W01");
        assert_eq!(iso_week(ts(2024, 12, 30)), "2025-W01");
        assert_eq!(iso_week(ts(2026, 10, 27)), "2026-W44");
    }
}
