//! Position GPS et date de prise de vue d'une photo, lues dans son EXIF **avant** qu'elle ne soit rangée chiffrée.
//!
//! Ces deux valeurs alimentent la carte des photos et la frise : sans elles, une photo n'a que sa date d'envoi. La lecture
//! ne rend jamais d'erreur (une photo sans EXIF, ou à l'EXIF abîmé, est une photo comme une autre) et n'invente rien :
//! une position hors du globe, ou `0, 0` (« île nulle », valeur des appareils sans signal GPS), est ignorée, et une date
//! hors de 1990 → demain aussi.

use std::io::Cursor;

use exif::{In, Reader, Tag, Value};

/// Ce que l'EXIF d'une photo apprend.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ExifMeta {
    /// Latitude en degrés décimaux (nord positif).
    pub lat: Option<f64>,
    /// Longitude en degrés décimaux (est positif).
    pub lon: Option<f64>,
    /// Prise de vue, secondes UNIX (heure locale lue comme UTC si l'appareil n'a pas noté le décalage).
    pub taken_at: Option<i64>,
}

impl ExifMeta {
    /// Rien d'utile.
    pub fn is_empty(&self) -> bool {
        self.lat.is_none() && self.taken_at.is_none()
    }
}

fn ascii(e: &exif::Exif, tag: Tag) -> Option<String> {
    match &e.get_field(tag, In::PRIMARY)?.value {
        Value::Ascii(v) => v.first().map(|b| String::from_utf8_lossy(b).trim_matches(char::from(0)).trim().to_owned()),
        _ => None,
    }
}

/// Degrés, minutes, secondes → degrés décimaux, signe selon la référence (`S`, `W` : négatif).
fn coordinate(e: &exif::Exif, value: Tag, reference: Tag) -> Option<f64> {
    let Value::Rational(r) = &e.get_field(value, In::PRIMARY)?.value else { return None };
    if r.len() < 3 || r.iter().any(|x| x.denom == 0) {
        return None;
    }
    let deg = r[0].to_f64() + r[1].to_f64() / 60.0 + r[2].to_f64() / 3600.0;
    let sign = match ascii(e, reference)?.to_ascii_uppercase().as_str() {
        "N" | "E" => 1.0,
        "S" | "W" => -1.0,
        _ => return None,
    };
    Some(sign * deg)
}

/// Jours depuis le 1ᵉʳ janvier 1970 d'une date du calendrier grégorien (algorithme de Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY:MM:DD HH:MM:SS` (+ décalage `±HH:MM` facultatif) → secondes UNIX.
fn timestamp(text: &str, offset: Option<&str>) -> Option<i64> {
    let (date, time) = text.split_once(' ')?;
    let num = |s: &str| s.trim().parse::<i64>().ok();
    let d: Vec<i64> = date.split(':').filter_map(num).collect();
    let t: Vec<i64> = time.split(':').filter_map(num).collect();
    if d.len() != 3 || t.len() != 3 || !(1..=12).contains(&d[1]) || !(1..=31).contains(&d[2]) || t[0] > 23 || t[1] > 59 || t[2] > 60 {
        return None;
    }
    let local = days_from_civil(d[0], d[1], d[2]) * 86_400 + t[0] * 3600 + t[1] * 60 + t[2];
    let shift = offset
        .and_then(|o| {
            let (sign, rest) = (if o.starts_with('-') { -1 } else { 1 }, o.trim_start_matches(['+', '-']));
            let (h, m) = rest.split_once(':')?;
            Some(sign * (num(h)? * 3600 + num(m)? * 60))
        })
        .unwrap_or(0);
    Some(local - shift)
}

/// Lit l'EXIF de `bytes` (JPEG, TIFF, HEIF, PNG, WebP). Rend `ExifMeta::default()` quand il n'y a rien d'exploitable.
pub fn read(bytes: &[u8], now_secs: i64) -> ExifMeta {
    let Ok(e) = Reader::new().read_from_container(&mut Cursor::new(bytes)) else { return ExifMeta::default() };
    let (lat, lon) = match (coordinate(&e, Tag::GPSLatitude, Tag::GPSLatitudeRef), coordinate(&e, Tag::GPSLongitude, Tag::GPSLongitudeRef)) {
        (Some(a), Some(o)) if (-90.0..=90.0).contains(&a) && (-180.0..=180.0).contains(&o) && !(a == 0.0 && o == 0.0) => (Some(a), Some(o)),
        _ => (None, None),
    };
    let taken_at = ascii(&e, Tag::DateTimeOriginal)
        .or_else(|| ascii(&e, Tag::DateTimeDigitized))
        .and_then(|t| timestamp(&t, ascii(&e, Tag::OffsetTimeOriginal).as_deref()))
        .filter(|t| (631_152_000..=now_secs + 86_400).contains(t)); // 1990-01-01 → demain
    ExifMeta { lat, lon, taken_at }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bloc TIFF petit-boutiste : IFD0 → (ExifIFD : DateTimeOriginal) et (GPS IFD : latitude, longitude).
    fn tiff(lat: (u32, u32, u32, &str), lon: (u32, u32, u32, &str), date: &str) -> Vec<u8> {
        fn entry(tag: u16, typ: u16, count: u32, value: u32) -> Vec<u8> {
            let mut e = tag.to_le_bytes().to_vec();
            e.extend(typ.to_le_bytes());
            e.extend(count.to_le_bytes());
            e.extend(value.to_le_bytes());
            e
        }
        let ref_value = |s: &str| u32::from(s.as_bytes()[0]);
        // Disposition : en-tête (8) · IFD0 (2 entrées) · IFD Exif (1) · IFD GPS (4) · données.
        let (ifd0, exif_ifd, gps_ifd) = (8u32, 8 + 2 + 2 * 12 + 4, 8 + 2 + 2 * 12 + 4 + 2 + 12 + 4);
        let data = gps_ifd + 2 + 4 * 12 + 4;
        let (lat_at, lon_at, date_at) = (data, data + 24, data + 48);
        let mut t = b"II*\0".to_vec();
        t.extend(ifd0.to_le_bytes());
        t.extend(2u16.to_le_bytes());
        t.extend(entry(0x8769, 4, 1, exif_ifd));
        t.extend(entry(0x8825, 4, 1, gps_ifd));
        t.extend(0u32.to_le_bytes());
        t.extend(1u16.to_le_bytes());
        t.extend(entry(0x9003, 2, 20, date_at));
        t.extend(0u32.to_le_bytes());
        t.extend(4u16.to_le_bytes());
        t.extend(entry(1, 2, 2, ref_value(lat.3)));
        t.extend(entry(2, 5, 3, lat_at));
        t.extend(entry(3, 2, 2, ref_value(lon.3)));
        t.extend(entry(4, 5, 3, lon_at));
        t.extend(0u32.to_le_bytes());
        for (d, m, s) in [(lat.0, lat.1, lat.2), (lon.0, lon.1, lon.2)] {
            for (n, den) in [(d, 1u32), (m, 1), (s, 100)] {
                t.extend(n.to_le_bytes());
                t.extend(den.to_le_bytes());
            }
        }
        t.extend(date.as_bytes());
        t.push(0);
        t
    }

    fn jpeg_with(tiff: &[u8]) -> Vec<u8> {
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(tiff);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend(u16::try_from(app1.len() + 2).unwrap().to_be_bytes());
        out.extend(app1);
        out.extend([0xFF, 0xD9]);
        out
    }

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn gps_and_the_shooting_date_are_read_with_their_signs() {
        // 43° 36′ 0,00″ N, 1° 26′ 24,00″ E (Toulouse) — le 14 juillet 2024 à 15 h 03 min 22 s.
        let m = read(&jpeg_with(&tiff((43, 36, 0, "N"), (1, 26, 2400, "E"), "2024:07:14 15:03:22")), NOW);
        let (lat, lon) = (m.lat.unwrap(), m.lon.unwrap());
        assert!((lat - 43.6).abs() < 1e-6 && (lon - 1.44).abs() < 1e-6, "{m:?}");
        assert_eq!(m.taken_at, Some(1_720_969_402), "2024-07-14T15:03:22Z");
        // Sud et ouest : négatifs.
        let m = read(&jpeg_with(&tiff((33, 51, 0, "S"), (151, 12, 0, "W"), "2024:07:14 15:03:22")), NOW);
        assert!(m.lat.unwrap() < -33.8 && m.lon.unwrap() < -151.1, "{m:?}");
    }

    #[test]
    fn implausible_values_and_missing_exif_are_ignored_never_invented() {
        // Île nulle (0, 0) : appareil sans signal GPS. Latitude hors du globe. Date avant 1990 ou dans le futur.
        let null_island = read(&jpeg_with(&tiff((0, 0, 0, "N"), (0, 0, 0, "E"), "2024:07:14 15:03:22")), NOW);
        assert_eq!((null_island.lat, null_island.lon), (None, None));
        let off_globe = read(&jpeg_with(&tiff((95, 0, 0, "N"), (1, 0, 0, "E"), "2024:07:14 15:03:22")), NOW);
        assert_eq!((off_globe.lat, off_globe.lon), (None, None));
        assert_eq!(read(&jpeg_with(&tiff((43, 0, 0, "N"), (1, 0, 0, "E"), "1971:01:01 00:00:00")), NOW).taken_at, None);
        assert_eq!(read(&jpeg_with(&tiff((43, 0, 0, "N"), (1, 0, 0, "E"), "2099:01:01 00:00:00")), NOW).taken_at, None);
        assert_eq!(read(&jpeg_with(&tiff((43, 0, 0, "N"), (1, 0, 0, "E"), "pas une date")), NOW).taken_at, None);
        assert!(read(b"pas une image", NOW).is_empty() && read(&[0xFF, 0xD8, 0xFF, 0xD9], NOW).is_empty());
    }

    #[test]
    fn civil_dates_convert_to_unix_time() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(timestamp("2024:07:14 15:03:22", Some("+02:00")), Some(1_720_969_402 - 7200));
        assert_eq!(timestamp("2024:07:14 15:03:22", Some("-05:30")), Some(1_720_969_402 + 19_800));
        assert_eq!(timestamp("2024:13:14 15:03:22", None), None);
    }
}
