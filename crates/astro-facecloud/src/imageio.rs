//! Lecture d'image **dans le sens où on la regarde** : l'orientation EXIF est appliquée au décodage.
//!
//! Un smartphone enregistre souvent les pixels « couchés » et demande, par un champ EXIF, de les tourner à l'affichage.
//! Les navigateurs le font ; un décodeur brut non. Le cadre d'un visage (`bbox`), la vignette qu'on en tire et la photo
//! que montre la visionneuse doivent donc partager le même repère : **celui de l'image tournée**. Le moteur local
//! ([`crate::onnx`]) et les vignettes ([`crate::thumbs`]) passent tous par ici ; le service HTTP (ComfyUI) tourne déjà
//! l'image avant de détecter.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageReader, Limits, RgbImage};

/// Côté maximal accepté : au-delà, l'image est refusée (décompression piégée).
pub const MAX_SIDE: u32 = 16_384;

/// Décode `bytes` (JPEG, PNG, WebP, GIF), orientation EXIF appliquée.
pub fn decode_oriented(bytes: &[u8]) -> Result<RgbImage, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(512 << 20);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    Ok(img.to_rgb8())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JPEG `w × h` (moitié gauche rouge, moitié droite bleue) portant l'orientation EXIF `orientation`.
    fn jpeg_with_orientation(w: u32, h: u32, orientation: u16) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, _| if x < w / 2 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).encode_image(&img).unwrap();
        // Segment APP1 « Exif » minimal : en-tête TIFF petit-boutiste, une entrée (0x0112 Orientation, SHORT, 1).
        let mut tiff = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0];
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let len = u16::try_from(app1.len() + 2).unwrap();
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn exif_orientation_is_applied_so_boxes_thumbnails_and_the_viewer_share_one_frame() {
        let upright = decode_oriented(&jpeg_with_orientation(40, 20, 1)).unwrap();
        assert_eq!(upright.dimensions(), (40, 20));
        assert!(upright.get_pixel(2, 10)[0] > 200 && upright.get_pixel(37, 10)[2] > 200, "orientation 1 : rouge à gauche, bleu à droite");
        // 6 = « tourner de 90° dans le sens horaire pour afficher » : l'image devient 20 × 40, le rouge passe en HAUT.
        let rotated = decode_oriented(&jpeg_with_orientation(40, 20, 6)).unwrap();
        assert_eq!(rotated.dimensions(), (20, 40));
        assert!(rotated.get_pixel(10, 2)[0] > 200 && rotated.get_pixel(10, 37)[2] > 200, "orientation 6 : rouge en haut, bleu en bas");
        // 8 = 90° anti-horaire : le rouge passe en BAS.
        let other = decode_oriented(&jpeg_with_orientation(40, 20, 8)).unwrap();
        assert_eq!(other.dimensions(), (20, 40));
        assert!(other.get_pixel(10, 37)[0] > 200 && other.get_pixel(10, 2)[2] > 200);
        // 3 = 180° : le rouge passe à droite.
        assert!(decode_oriented(&jpeg_with_orientation(40, 20, 3)).unwrap().get_pixel(37, 10)[0] > 200);
        assert!(decode_oriented(b"pas une image").is_err());
    }
}
