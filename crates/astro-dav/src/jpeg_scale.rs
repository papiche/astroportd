//! Décodage JPEG **à échelle réduite** : l'IDCT de `jpeg-decoder` sait produire directement l'image au 1/2, 1/4 ou
//! 1/8, sans jamais matérialiser les pixels pleine résolution. Une miniature de 320 px tirée d'une photo de 12 Mpx
//! n'a besoin que d'une image de 500 à 1 000 px : décodage et réduction coûtent alors bien moins cher, en temps comme
//! en mémoire (≈ 1,5 Mo au lieu de 36 Mo pour la photo décodée).
//!
//! Le décodeur général (`image`, zune-jpeg) reste la voie normale : ce module ne sert que si une réduction est
//! possible, et toute erreur ici (format non JPEG, JPEG CMYK ou 16 bits, flux que `jpeg-decoder` refuse) laisse
//! l'appelant retomber sur lui — c'est lui qui dira alors la vraie erreur, s'il y en a une.
//!
//! L'échelle s'adapte à la machine : sur une machine modeste (deux cœurs au plus, ou moins de 2 Go de RAM), la
//! plus agressive qui garde la taille demandée ; ailleurs, un cran de marge (deux fois la taille demandée), pour une
//! réduction finale plus fine.

use image::{DynamicImage, GrayImage, RgbImage};
use image::metadata::Orientation;

/// Image décodée réduite, orientation EXIF appliquée.
#[derive(Debug)]
pub struct Scaled {
    /// Pixels réduits, dans le sens où l'on regarde la photo.
    pub image: DynamicImage,
    /// Dimensions de l'original (pleine résolution), dans le même sens.
    pub full: (u32, u32),
}

/// Machine modeste : deux cœurs au plus ou moins de 2 Go de RAM (RAM inconnue : on ne présume pas qu'elle l'est).
pub fn modest_machine() -> bool {
    static MODEST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MODEST.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        cores <= 2 || astro_db::total_ram_bytes().is_some_and(|r| r < 2 << 30)
    })
}

/// Marge appliquée à la taille demandée : ×1 sur une machine modeste (échelle la plus agressive), ×2 ailleurs.
pub fn margin() -> u32 {
    if modest_machine() { 1 } else { 2 }
}

/// Plus grand diviseur parmi 8, 4, 2 tel que `len / k` (arrondi au-dessus, comme l'IDCT réduite) reste ≥ `need` ;
/// `1` si aucune réduction n'est possible.
pub fn divisor_for(len: u32, need: u32) -> u32 {
    [8, 4, 2].into_iter().find(|k| len.div_ceil(*k) >= need.max(1)).unwrap_or(1)
}

/// Décode `bytes` (JPEG) réduit du diviseur que `pick(largeur, hauteur)` choisit d'après les dimensions **brutes**
/// (avant rotation EXIF : `pick` ne doit dépendre que de grandeurs qui ne changent pas en tournant, plus grand ou
/// plus petit côté). `max_side` refuse les images piégées. `Err` : pas de réduction utile, ou JPEG que ce décodeur ne
/// traite pas — l'appelant passe alors par le décodeur général.
pub fn decode_scaled(bytes: &[u8], max_side: u32, pick: impl FnOnce(u32, u32) -> u32) -> Result<Scaled, String> {
    if !bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Err("pas un JPEG".into());
    }
    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    decoder.set_max_decoding_buffer_size(512 << 20);
    decoder.read_info().map_err(|e| e.to_string())?;
    let info = decoder.info().ok_or("en-tête JPEG absent")?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    if w == 0 || h == 0 || w > max_side || h > max_side {
        return Err(format!("dimensions refusées : {w} × {h}"));
    }
    let k = match pick(w, h) {
        k @ (2 | 4 | 8) => k,
        _ => return Err("pas de réduction utile".into()),
    };
    let request = |len: u32| u16::try_from(len.div_ceil(k)).map_err(|e| e.to_string());
    let (sw, sh) = decoder.scale(request(w)?, request(h)?).map_err(|e| e.to_string())?;
    let pixels = decoder.decode().map_err(|e| e.to_string())?;
    let (sw, sh) = (u32::from(sw), u32::from(sh));
    let info = decoder.info().ok_or("en-tête JPEG absent")?;
    let mut image = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => DynamicImage::ImageRgb8(RgbImage::from_raw(sw, sh, pixels).ok_or("taille de tampon inattendue")?),
        jpeg_decoder::PixelFormat::L8 => DynamicImage::ImageLuma8(GrayImage::from_raw(sw, sh, pixels).ok_or("taille de tampon inattendue")?),
        other => return Err(format!("format de pixels laissé au décodeur général : {other:?}")),
    };
    let orientation = decoder.exif_data().and_then(Orientation::from_exif_chunk).unwrap_or(Orientation::NoTransforms);
    image.apply_orientation(orientation);
    let turned = matches!(orientation, Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH);
    let full = if turned { (h, w) } else { (w, h) };
    Ok(Scaled { image, full })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_divisor_keeps_the_requested_size() {
        assert_eq!(divisor_for(4000, 320), 8);
        assert_eq!(divisor_for(4000, 640), 4);
        assert_eq!(divisor_for(2559, 320), 8, "2559 / 8 arrondi au-dessus = 320");
        assert_eq!(divisor_for(2550, 320), 4);
        assert_eq!(divisor_for(600, 320), 1);
        assert_eq!(divisor_for(320, 0), 8);
    }

    #[test]
    fn non_jpeg_and_useless_reductions_are_left_to_the_general_decoder() {
        assert!(decode_scaled(b"\x89PNG....", 20_000, |_, _| 8).is_err());
        let mut jpeg = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::new(64, 48)).write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        let jpeg = jpeg.into_inner();
        assert!(decode_scaled(&jpeg, 20_000, |_, _| 1).is_err());
        assert!(decode_scaled(&jpeg, 32, |_, _| 2).is_err(), "côté maximal");
        let s = decode_scaled(&jpeg, 20_000, |_, _| 2).unwrap();
        assert_eq!((s.image.width(), s.image.height(), s.full), (32, 24, (64, 48)));
    }
}
