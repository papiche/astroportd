//! Vignettes de visages recadrées **par la station** : la table de tri en affiche des milliers, et déchiffrer puis
//! décoder chaque photo entière côté navigateur pour en recadrer un visage serait ruineux.
//!
//! Chaque photo n'est déchiffrée et décodée qu'une fois pour tous ses visages du lot. Les vignettes sont gardées en
//! mémoire (jamais de clair sur disque) ; le décodage est borné (deux photos à la fois, limites de taille d'image).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use astro_dav::BlobStore;
use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;

use crate::pipeline::{FaceCloud, FaceError, Outbox};
use crate::FaceEngine;

/// Taille mini et maxi d'une vignette, en pixels.
pub const SIZE_RANGE: (u32, u32) = (32, 320);
/// Vignettes gardées en mémoire (≈ 4 Ko chacune).
const CACHE_ENTRIES: usize = 6_000;
/// Visages au plus par appel.
pub const BATCH_MAX: usize = 80;
/// Marge autour du cadre du visage : le côté du carré recadré vaut 1,6 fois le plus grand côté du cadre.
const MARGIN: f64 = 1.6;

/// Titulaire, visage, taille.
type Key = (String, i64, u32);

/// Mémoire des vignettes déjà calculées, évincées dans l'ordre d'arrivée.
#[derive(Default)]
pub struct ThumbCache {
    inner: Mutex<(HashMap<Key, String>, VecDeque<Key>)>,
}

impl ThumbCache {
    /// Cache vide.
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, owner: &str, id: i64, size: u32) -> Option<String> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).0.get(&(owner.to_owned(), id, size)).cloned()
    }

    fn put(&self, owner: &str, id: i64, size: u32, url: String) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let key = (owner.to_owned(), id, size);
        if g.0.insert(key.clone(), url).is_none() {
            g.1.push_back(key);
        }
        while g.1.len() > CACHE_ENTRIES {
            if let Some(old) = g.1.pop_front() {
                g.0.remove(&old);
            }
        }
    }

    /// Oublie les vignettes de ces visages (supprimés, ou rangés : la vignette ne change pas, mais l'identifiant n'existe plus).
    pub fn forget(&self, owner: &str, ids: &[i64]) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.0.retain(|(o, i, _), _| !(o == owner && ids.contains(i)));
        g.1.retain(|(o, i, _)| !(o == owner && ids.contains(i)));
    }
}

/// Carré recadré autour de `bbox` dans une image `w × h` : `(x, y, côté)`, ramené dans l'image.
pub fn crop_square(bbox: [f64; 4], w: u32, h: u32) -> (u32, u32, u32) {
    let [x1, y1, x2, y2] = bbox;
    let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
    let side = ((x2 - x1).max(y2 - y1) * MARGIN).max(8.0);
    let side = side.min(f64::from(w.min(h))).max(1.0);
    let x = (cx - side / 2.0).clamp(0.0, f64::from(w) - side);
    let y = (cy - side / 2.0).clamp(0.0, f64::from(h) - side);
    (x.round() as u32, y.round() as u32, side.round().max(1.0) as u32)
}

/// Décode `plain` et en tire une vignette JPEG carrée par cadre.
///
/// Une photo JPEG assez grande est décodée réduite (voir [`astro_dav::jpeg_scale`]) tant que le plus petit carré à
/// recadrer garde au moins `size` pixels de côté (le double hors machine modeste) : la vignette n'y perd rien, la
/// photo entière n'est jamais décodée en pleine résolution. Les cadres (`bbox`, repère de l'original tourné) sont
/// ramenés à l'échelle de l'image réduite.
fn crops(plain: &[u8], boxes: &[(i64, [f64; 4])], size: u32) -> Result<Vec<(i64, String)>, String> {
    let need = size * astro_dav::jpeg_scale::margin();
    // Le côté d'un carré ne dépend que de la taille du cadre et du plus petit côté de l'image : invariant par rotation.
    let pick = |w: u32, h: u32| boxes.iter().map(|(_, b)| crop_square(*b, w, h).2).min().map_or(1, |side| astro_dav::jpeg_scale::divisor_for(side, need));
    let (rgb, (fx, fy)) = match astro_dav::jpeg_scale::decode_scaled(plain, crate::imageio::MAX_SIDE, pick) {
        Ok(s) => {
            let rgb = s.image.to_rgb8();
            let f = (f64::from(rgb.width()) / f64::from(s.full.0), f64::from(rgb.height()) / f64::from(s.full.1));
            (rgb, f)
        }
        Err(_) => (crate::imageio::decode_oriented(plain)?, (1.0, 1.0)),
    };
    let (w, h) = rgb.dimensions();
    let mut out = Vec::with_capacity(boxes.len());
    for (id, bbox) in boxes {
        let [x1, y1, x2, y2] = *bbox;
        let (x, y, side) = crop_square([x1 * fx, y1 * fy, x2 * fx, y2 * fy], w, h);
        let part = image::imageops::crop_imm(&rgb, x, y, side.min(w - x), side.min(h - y)).to_image();
        let small = image::imageops::resize(&part, size, size, FilterType::Triangle);
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 82).encode_image(&small).map_err(|e| e.to_string())?;
        out.push((*id, format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg))));
    }
    Ok(out)
}

/// Vignettes obtenues et visages dont la photo n'a pas pu être lue (raison).
#[derive(Debug, Default)]
pub struct ThumbBatch {
    /// `face_id` → data URL JPEG.
    pub thumbs: HashMap<i64, String>,
    /// `(face_id, raison)`.
    pub failed: Vec<(i64, String)>,
}

impl<B: BlobStore, E: FaceEngine, O: Outbox> FaceCloud<B, E, O> {
    /// Vignettes carrées de `size` pixels pour des visages de `owner`.
    pub async fn face_thumbs(&self, owner: &str, ids: &[i64], size: u32) -> Result<ThumbBatch, FaceError> {
        if ids.is_empty() || ids.len() > BATCH_MAX {
            return Err(FaceError::Invalid("1 à 80 visages par appel"));
        }
        if !(SIZE_RANGE.0..=SIZE_RANGE.1).contains(&size) {
            return Err(FaceError::Invalid("taille de vignette hors limites (32 à 320)"));
        }
        let mut batch = ThumbBatch::default();
        let mut missing: Vec<i64> = Vec::new();
        for id in ids {
            match self.thumbs.get(owner, *id, size) {
                Some(url) => {
                    batch.thumbs.insert(*id, url);
                }
                None => missing.push(*id),
            }
        }
        if missing.is_empty() {
            return Ok(batch);
        }
        let (o, want) = (owner.to_owned(), missing.clone());
        let rows: Vec<(i64, String, [f64; 4])> = self
            .db
            .readers()
            .read(move |c| {
                let marks = vec!["?"; want.len()].join(",");
                let mut stmt = c.prepare(&format!("SELECT face_id, ucloud_path, bbox_x1, bbox_y1, bbox_x2, bbox_y2 FROM faces WHERE owner_hex = ?1 AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL AND face_id IN ({marks})"))?;
                let mut args: Vec<rusqlite::types::Value> = vec![o.into()];
                args.extend(want.iter().map(|i| rusqlite::types::Value::Integer(*i)));
                let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| Ok((r.get(0)?, r.get(1)?, [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?])))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;
        let found: Vec<i64> = rows.iter().map(|r| r.0).collect();
        for id in &missing {
            if !found.contains(id) {
                batch.failed.push((*id, "visage inconnu".into()));
            }
        }
        let mut by_path: HashMap<String, Vec<(i64, [f64; 4])>> = HashMap::new();
        for (id, path, bbox) in rows {
            by_path.entry(path).or_default().push((id, bbox));
        }
        for (path, boxes) in by_path {
            let _slot = self.decode.acquire().await.map_err(|_| FaceError::Invalid("décodage fermé"))?;
            let plain = match self.photo(owner, &path).await {
                Ok(photo) => self.read_plain(&photo).await,
                Err(e) => Err(e),
            };
            let plain = match plain {
                Ok(p) => p,
                Err(e) => {
                    batch.failed.extend(boxes.iter().map(|(id, _)| (*id, e.to_string())));
                    continue;
                }
            };
            let work = boxes.clone();
            match tokio::task::spawn_blocking(move || crops(&plain, &work, size)).await {
                Ok(Ok(done)) => {
                    for (id, url) in done {
                        self.thumbs.put(owner, id, size, url.clone());
                        batch.thumbs.insert(id, url);
                    }
                }
                Ok(Err(why)) => batch.failed.extend(boxes.iter().map(|(id, _)| (*id, format!("image illisible : {why}")))),
                Err(_) => batch.failed.extend(boxes.iter().map(|(id, _)| (*id, "décodage interrompu".to_owned()))),
            }
        }
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::DynamicImage;

    use super::*;

    #[test]
    fn the_crop_is_a_square_around_the_face_and_stays_inside_the_image() {
        // Visage 100 × 120 centré en (60, 80) : côté = 120 × 1,6 = 192, centré.
        assert_eq!(crop_square([10.0, 20.0, 110.0, 140.0], 1000, 1000), (0, 0, 192), "collé au coin haut-gauche : ramené dans l'image");
        assert_eq!(crop_square([400.0, 400.0, 500.0, 520.0], 1000, 1000), (354, 364, 192));
        // Visage énorme dans une petite image : le côté est borné par la plus petite dimension.
        assert_eq!(crop_square([0.0, 0.0, 300.0, 300.0], 200, 100), (100, 0, 100), "centré sur le cadre (150), ramené dans l'image");
        // Cadre dégénéré : jamais de côté nul.
        assert!(crop_square([5.0, 5.0, 5.0, 5.0], 50, 50).2 >= 8);
    }

    #[test]
    fn crops_decode_once_and_return_square_jpeg_data_urls() {
        let img = image::RgbImage::from_fn(200, 100, |x, _| image::Rgb([(x % 256) as u8, 80, 160]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let out = crops(&png, &[(1, [20.0, 20.0, 60.0, 60.0]), (2, [140.0, 30.0, 180.0, 70.0])], 48).unwrap();
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), [1, 2]);
        for (_, url) in &out {
            let b64 = url.strip_prefix("data:image/jpeg;base64,").unwrap();
            let jpeg = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
            assert_eq!(image::load_from_memory(&jpeg).unwrap().dimensions_pair(), (48, 48));
        }
        assert!(crops(b"pas une image", &[(1, [0.0; 4])], 48).is_err());
    }

    /// Vignette de référence : décodage pleine résolution, recadrage, réduction (le chemin d'avant).
    fn reference(plain: &[u8], bbox: [f64; 4], size: u32) -> image::RgbImage {
        let rgb = crate::imageio::decode_oriented(plain).unwrap();
        let (x, y, side) = crop_square(bbox, rgb.width(), rgb.height());
        let part = image::imageops::crop_imm(&rgb, x, y, side, side).to_image();
        image::imageops::resize(&part, size, size, FilterType::Triangle)
    }

    #[test]
    fn a_large_jpeg_is_cropped_from_a_reduced_decode_without_visible_difference() {
        // 4000 × 3000 couché (EXIF 6) : vu debout 3000 × 4000. Deux visages : un grand (réduction possible), un petit.
        let raw = image::RgbImage::from_fn(4000, 3000, |x, y| {
            let t = ((x / 7 + y / 5) % 64) as u8;
            if x < 2000 { image::Rgb([200 - t, 40 + t, 30]) } else { image::Rgb([30, 60 + t, 210 - t]) }
        });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&raw).unwrap();
        let mut tiff = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0];
        let mut app1 = b"Exif\0\0".to_vec();
        app1.append(&mut tiff);
        let mut src = vec![0xFF, 0xD8, 0xFF, 0xE1];
        src.extend_from_slice(&u16::try_from(app1.len() + 2).unwrap().to_be_bytes());
        src.extend_from_slice(&app1);
        src.extend_from_slice(&jpeg[2..]);
        let boxes = [(1, [900.0, 1500.0, 1700.0, 2300.0]), (2, [2000.0, 3000.0, 2100.0, 3100.0])];
        for size in [96, 320] {
            let out = crops(&src, &boxes, size).unwrap();
            for ((_, url), (_, bbox)) in out.iter().zip(boxes) {
                let b64 = url.strip_prefix("data:image/jpeg;base64,").unwrap();
                let got = image::load_from_memory(&base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap().to_rgb8();
                let want = reference(&src, bbox, size);
                assert_eq!(got.dimensions(), (size, size));
                let diff: f64 = got.pixels().zip(want.pixels()).map(|(a, b)| (0..3).map(|i| f64::from(a[i].abs_diff(b[i]))).sum::<f64>()).sum::<f64>() / f64::from(size * size * 3);
                assert!(diff < 8.0, "écart moyen {diff:.1} (taille {size}, cadre {bbox:?})");
            }
        }
        if std::env::var_os("ASTRO_MEASURE").is_some() {
            let t = std::time::Instant::now();
            crops(&src, &boxes[..1], 96).unwrap();
            let reduced = t.elapsed();
            let t = std::time::Instant::now();
            reference(&src, boxes[0].1, 96);
            println!("vignette de visage, JPEG 12 Mpx : réduit {reduced:?}, pleine résolution {:?}", t.elapsed());
        }
    }

    trait Dim {
        fn dimensions_pair(&self) -> (u32, u32);
    }
    impl Dim for DynamicImage {
        fn dimensions_pair(&self) -> (u32, u32) {
            image::GenericImageView::dimensions(self)
        }
    }

    #[test]
    fn the_cache_evicts_the_oldest_and_forgets_on_demand() {
        let c = ThumbCache::new();
        for i in 0..(CACHE_ENTRIES as i64 + 10) {
            c.put("a", i, 96, format!("u{i}"));
        }
        assert!(c.get("a", 0, 96).is_none() && c.get("a", CACHE_ENTRIES as i64 + 9, 96).is_some());
        c.put("a", 5_000_000, 96, "x".into());
        c.forget("a", &[5_000_000]);
        assert!(c.get("a", 5_000_000, 96).is_none());
    }
}
