//! Quart de tour d'une photo. Un JPEG se redresse **sans perte** en réécrivant son orientation EXIF (le reste de l'EXIF,
//! date et GPS compris, reste intact) ; les autres formats, ou un JPEG dont l'EXIF n'a pas d'orientation à modifier,
//! sont tournés au pixel près puis ré-encodés (l'orientation EXIF éventuelle est alors appliquée aux pixels).

use std::io::Cursor;

use std::sync::Arc;

use astro_dav::{BlobStore, DavState, carddav, ingest, vfs};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, metadata::Orientation};

/// Un quart de tour : `1` vers la droite (sens horaire), `-1` vers la gauche.
pub type Turn = i8;

/// Résultat : nouveaux octets et type MIME.
pub struct Rotated {
    /// Fichier tourné.
    pub bytes: Vec<u8>,
    /// Type MIME (identique à l'entrée).
    pub mime: &'static str,
    /// `true` si seul l'EXIF a changé (aucune perte de qualité).
    pub lossless: bool,
}

/// Orientation EXIF obtenue en tournant l'image affichée d'un quart de tour horaire.
const CLOCKWISE: [u8; 8] = [6, 7, 8, 5, 2, 3, 4, 1];

fn turned(orientation: u8, turn: Turn) -> u8 {
    let mut o = if (1..=8).contains(&orientation) { orientation } else { 1 };
    for _ in 0..(if turn > 0 { 1 } else { 3 }) {
        o = CLOCKWISE[usize::from(o) - 1];
    }
    o
}

/// Position de l'étiquette d'orientation dans un JPEG : `Ok(Some((offset, petit-boutiste)))`, `Ok(None)` s'il n'y a aucun
/// segment EXIF, `Err(())` si l'EXIF existe sans étiquette d'orientation utilisable.
fn find_orientation(jpeg: &[u8]) -> Result<Option<(usize, bool)>, ()> {
    if jpeg.len() < 4 || jpeg[..2] != [0xFF, 0xD8] {
        return Err(());
    }
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF {
        let marker = jpeg[i + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        let end = i + 2 + len;
        if len < 2 || end > jpeg.len() {
            return Err(());
        }
        let seg = &jpeg[i + 4..end];
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            let tiff = i + 4 + 6;
            let t = &jpeg[tiff..end];
            let le = match t.get(..2) {
                Some(b"II") => true,
                Some(b"MM") => false,
                _ => return Err(()),
            };
            let u16at = |o: usize| -> Option<u16> { t.get(o..o + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) }) };
            let u32at = |o: usize| -> Option<u32> { t.get(o..o + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) }) };
            let ifd = usize::try_from(u32at(4).ok_or(())?).map_err(|_| ())?;
            let n = usize::from(u16at(ifd).ok_or(())?);
            for k in 0..n {
                let e = ifd + 2 + k * 12;
                if u16at(e).ok_or(())? == 0x0112 {
                    // Type SHORT, un seul élément : la valeur tient dans les 2 premiers octets du champ.
                    if u16at(e + 2).ok_or(())? != 3 || u32at(e + 4).ok_or(())? != 1 {
                        return Err(());
                    }
                    return Ok(Some((tiff + e + 8, le)));
                }
            }
            return Err(());
        }
        i = end;
    }
    Ok(None)
}

/// JPEG sans EXIF : insère un segment EXIF minimal (orientation seule) après l'éventuel `APP0` JFIF.
fn with_new_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
    let mut at = 2;
    if jpeg.len() >= 6 && jpeg[2] == 0xFF && jpeg[3] == 0xE0 {
        at = 2 + 2 + usize::from(u16::from_be_bytes([jpeg[4], jpeg[5]]));
    }
    let at = at.min(jpeg.len());
    let mut seg = vec![0xFF, 0xE1, 0x00, 0x22];
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(b"MM\0\x2A\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01");
    seg.extend_from_slice(&[0, orientation, 0, 0]);
    seg.extend_from_slice(&[0, 0, 0, 0]);
    let mut out = Vec::with_capacity(jpeg.len() + seg.len());
    out.extend_from_slice(&jpeg[..at]);
    out.extend_from_slice(&seg);
    out.extend_from_slice(&jpeg[at..]);
    out
}

fn pixels(src: &[u8], turn: Turn) -> Result<Rotated, String> {
    let reader = ImageReader::new(Cursor::new(src)).with_guessed_format().map_err(|e| e.to_string())?;
    let format = reader.format().ok_or("format d'image inconnu")?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let img = if turn > 0 { img.rotate90() } else { img.rotate270() };
    let mut out = Vec::new();
    let mime = match format {
        ImageFormat::Jpeg => {
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
            img.to_rgb8().write_with_encoder(enc).map_err(|e| e.to_string())?;
            "image/jpeg"
        }
        ImageFormat::Png => {
            img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(|e| e.to_string())?;
            "image/png"
        }
        ImageFormat::WebP => {
            img.write_to(&mut Cursor::new(&mut out), ImageFormat::WebP).map_err(|e| e.to_string())?;
            "image/webp"
        }
        _ => return Err("ce format (GIF animé ?) ne se tourne pas".into()),
    };
    Ok(Rotated { bytes: out, mime, lossless: false })
}

/// Tourne `src` d'un quart de tour.
pub fn rotate(src: &[u8], turn: Turn) -> Result<Rotated, String> {
    if src.starts_with(&[0xFF, 0xD8]) {
        match find_orientation(src) {
            Ok(Some((at, le))) => {
                let current = if le { src[at] } else { src[at + 1] };
                let next = turned(current, turn);
                let mut out = src.to_vec();
                (out[at], out[at + 1]) = if le { (next, 0) } else { (0, next) };
                return Ok(Rotated { bytes: out, mime: "image/jpeg", lossless: true });
            }
            Ok(None) => return Ok(Rotated { bytes: with_new_orientation(src, turned(1, turn)), mime: "image/jpeg", lossless: true }),
            Err(()) => {}
        }
    }
    pixels(src, turn)
}

/// Type MIME, date de modification, date de prise de vue, latitude, longitude.
type EntryMeta = (Option<String>, i64, Option<i64>, Option<f64>, Option<f64>);

/// Tourne la photo `path` de `owner` et la range à la même place (nouveau blob chiffré, ancien libéré). La date de prise
/// de vue, la position et la date de modification sont conservées même quand l'EXIF n'a pas survécu au ré-encodage.
pub async fn rotate_entry<B: BlobStore>(st: &Arc<DavState<B>>, owner: &str, path: &str, turn: Turn) -> Result<(), String> {
    let (o, p) = (owner.to_owned(), path.to_owned());
    let meta: Option<EntryMeta> = st
        .db
        .readers()
        .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT mime, mtime, taken_at, lat, lon FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file' AND cid IS NOT NULL", [&o, &p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))))?))
        .await
        .map_err(|e| e.to_string())?;
    let (mime, mtime, taken, lat, lon) = meta.ok_or("photo introuvable")?;
    if !mime.as_deref().is_some_and(|m| matches!(m, "image/jpeg" | "image/png" | "image/webp")) {
        return Err("seuls les JPEG, PNG et WebP se tournent".into());
    }
    let bytes = carddav::read_file(st, owner, path).await.ok_or("photo illisible")?;
    let r = tokio::task::spawn_blocking(move || rotate(&bytes, turn)).await.map_err(|e| e.to_string())??;
    let (dir, name) = (vfs::parent_of(path), vfs::name_of(path).to_owned());
    ingest::replace(st, owner, &dir, &name, Some(r.mime), Cursor::new(r.bytes)).await.map_err(|e| format!("{e:?}"))?;
    let (o, p) = (owner.to_owned(), path.to_owned());
    st.db
        .writer()
        .execute("UPDATE ucloud_entries SET mtime = ?3, taken_at = COALESCE(taken_at, ?4), lat = COALESCE(lat, ?5), lon = COALESCE(lon, ?6) WHERE owner_hex = ?1 AND path = ?2", vec![o.into(), p.into(), mtime.into(), taken.into(), lat.into(), lon.into()])
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < w / 2 { Rgb([250, 0, 0]) } else { Rgb([0, 0, 250]) };
        }
        let mut out = Vec::new();
        img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)).unwrap();
        out
    }

    fn shown(bytes: &[u8]) -> RgbImage {
        let mut d = ImageReader::new(Cursor::new(bytes)).with_guessed_format().unwrap().into_decoder().unwrap();
        let o = d.orientation().unwrap_or(Orientation::NoTransforms);
        let mut img = DynamicImage::from_decoder(d).unwrap();
        img.apply_orientation(o);
        img.to_rgb8()
    }

    #[test]
    fn the_exif_table_matches_what_a_viewer_draws_for_all_eight_orientations() {
        let base = jpeg(40, 20);
        for start in 1..=8u8 {
            let tagged = with_new_orientation(&base, start);
            for turn in [1, -1] {
                let r = rotate(&tagged, turn).unwrap();
                assert!(r.lossless);
                let (a, b) = (shown(&r.bytes), {
                    let img = DynamicImage::ImageRgb8(shown(&tagged));
                    (if turn > 0 { img.rotate90() } else { img.rotate270() }).to_rgb8()
                });
                assert_eq!(a.dimensions(), b.dimensions(), "orientation {start}, tour {turn}");
                for (x, y) in [(2, 2), (a.width() - 3, 2), (2, a.height() - 3), (a.width() - 3, a.height() - 3)] {
                    let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
                    assert!(p[0].abs_diff(q[0]) < 40 && p[2].abs_diff(q[2]) < 40, "orientation {start}, tour {turn}, pixel {x},{y}");
                }
            }
        }
    }

    #[test]
    fn four_quarter_turns_bring_the_photo_back_and_the_pixels_are_never_touched_on_a_jpeg() {
        let base = jpeg(40, 20);
        let mut cur = base.clone();
        for _ in 0..4 {
            cur = rotate(&cur, 1).unwrap().bytes;
        }
        assert_eq!(shown(&cur).dimensions(), (40, 20));
        let once = rotate(&base, 1).unwrap().bytes;
        assert_eq!(shown(&once).dimensions(), (20, 40), "tournée vers la droite : portrait");
        assert_eq!(&once[once.len() - 20..], &base[base.len() - 20..], "les données d'image sont intactes");
    }

    #[test]
    fn a_png_is_turned_by_its_pixels_and_garbage_is_refused() {
        let mut img = RgbImage::new(30, 10);
        img.put_pixel(0, 0, Rgb([255, 0, 0]));
        let mut png = Vec::new();
        DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let r = rotate(&png, 1).unwrap();
        assert_eq!((r.mime, r.lossless), ("image/png", false));
        let back = image::load_from_memory(&r.bytes).unwrap().to_rgb8();
        assert_eq!(back.dimensions(), (10, 30));
        assert_eq!(back.get_pixel(9, 0)[0], 255, "le coin haut-gauche passe en haut à droite");
        assert!(rotate(b"pas une image", 1).is_err());
    }
}
