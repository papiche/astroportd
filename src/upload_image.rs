//! Envoi d'images par un membre (NIP-96) : **relues et réencodées**, jamais servies telles quelles.
//!
//! Le fichier arrive sur une route publique authentifiée et finit dans `/Public/uploads` du membre, donc en clair sur le web.
//! Ce qui entre est décodé (une image piégée, une bombe de décompression ou un faux type sont refusés), réduit à 2 048 px de
//! côté au plus, puis réencodé : JPEG sans aucune métadonnée (la position GPS d'une photo n'est jamais republiée), ou PNG si
//! l'image a de la transparence. Le nom est le hachage du résultat : renvoyer la même image ne crée pas de doublon.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};

/// Côté maximal, en pixels, d'une image publiée.
pub const MAX_SIDE: u32 = 2048;
/// Dimension maximale acceptée à l'entrée (au-delà : refus avant tout décodage complet).
const MAX_INPUT_SIDE: u32 = 16_384;
/// Mémoire maximale du décodeur.
const MAX_ALLOC: u64 = 256 << 20;

/// Image prête à être rangée.
#[derive(Debug)]
pub struct Prepared {
    /// Octets réencodés.
    pub data: Vec<u8>,
    /// Extension du fichier (`jpg` ou `png`).
    pub ext: &'static str,
    /// Type MIME correspondant.
    pub mime: &'static str,
    /// SHA-256 (hexadécimal) des octets réencodés.
    pub sha256: String,
}

/// Décode, réduit et réencode. Les formats acceptés sont JPEG, PNG et WebP.
pub fn normalize(bytes: &[u8]) -> Result<Prepared, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    match reader.format() {
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP) => {}
        Some(ImageFormat::Gif) => return Err("les GIF ne sont pas acceptés ici (JPEG, PNG ou WebP)".into()),
        _ => return Err("ce fichier n'est pas une image JPEG, PNG ou WebP".into()),
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_INPUT_SIDE);
    limits.max_image_height = Some(MAX_INPUT_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let mut img = reader.decode().map_err(|_| "ce fichier n'est pas une image lisible (ou elle est démesurée)".to_owned())?;
    if img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        img = img.resize(MAX_SIDE, MAX_SIDE, FilterType::Lanczos3);
    }
    let mut out = Vec::new();
    let (ext, mime) = if img.color().has_alpha() {
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(|e| e.to_string())?;
        ("png", "image/png")
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img.to_rgb8()).map_err(|e| e.to_string())?;
        ("jpg", "image/jpeg")
    };
    let sha256 = hex::encode(Sha256::digest(&out));
    Ok(Prepared { data: out, ext, mime, sha256 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, alpha: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if alpha {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(w, h, image::Rgba([10, 200, 30, 128]))).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        } else {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([10, 200, 30]))).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        }
        out
    }

    #[test]
    fn a_photo_becomes_a_metadata_free_jpeg_named_by_its_hash() {
        // Un JPEG avec un segment EXIF (APP1) : le réencodage ne le garde pas.
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&image::RgbImage::from_pixel(64, 48, image::Rgb([1, 2, 3]))).unwrap();
        let exif = b"Exif\0\0GPS-LAT-45.76";
        let mut with_exif = jpeg[..2].to_vec();
        with_exif.extend_from_slice(&[0xFF, 0xE1]);
        with_exif.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
        with_exif.extend_from_slice(exif);
        with_exif.extend_from_slice(&jpeg[2..]);
        assert!(with_exif.windows(7).any(|w| w == b"GPS-LAT"));
        let p = normalize(&with_exif).unwrap();
        assert_eq!((p.ext, p.mime), ("jpg", "image/jpeg"));
        assert!(!p.data.windows(7).any(|w| w == b"GPS-LAT"), "aucune métadonnée republiée");
        assert_eq!(p.sha256.len(), 64);
        assert_eq!(normalize(&with_exif).unwrap().sha256, p.sha256, "même image, même nom");
    }

    #[test]
    fn transparency_keeps_png_and_big_images_are_reduced() {
        assert_eq!(normalize(&png(40, 30, true)).unwrap().ext, "png");
        assert_eq!(normalize(&png(40, 30, false)).unwrap().ext, "jpg");
        let big = normalize(&png(3000, 1000, false)).unwrap();
        let decoded = image::load_from_memory(&big.data).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (MAX_SIDE, 683), "2 048 px de côté au plus, proportions gardées");
    }

    #[test]
    fn anything_that_is_not_a_plain_image_is_refused() {
        for (what, bytes) in [
            ("html", b"<html><script>alert(1)</script></html>".to_vec()),
            ("svg", b"<svg xmlns='http://www.w3.org/2000/svg'><script>1</script></svg>".to_vec()),
            ("gif", b"GIF89a\x01\x00\x01\x00\x00\x00\x00;".to_vec()),
            ("exe", b"MZ\x90\x00\x03\x00\x00\x00".to_vec()),
            ("vide", Vec::new()),
            ("png tronqué", png(40, 30, false)[..30].to_vec()),
            ("png avec des octets en trop mais illisible", [&b"\x89PNG\r\n\x1a\n"[..], &[0u8; 64]].concat()),
        ] {
            assert!(normalize(&bytes).is_err(), "{what}");
        }
    }

    #[test]
    fn a_decompression_bomb_is_refused_before_it_is_decoded() {
        // En-tête PNG valide annonçant 40 000 × 40 000 pixels : refusé par les limites, sans allouer 6 Go.
        let mut b = png(8, 8, false);
        b[16..20].copy_from_slice(&40_000u32.to_be_bytes());
        b[20..24].copy_from_slice(&40_000u32.to_be_bytes());
        assert!(normalize(&b).is_err());
    }
}
