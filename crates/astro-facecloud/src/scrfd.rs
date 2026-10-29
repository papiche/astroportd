//! Détection SCRFD et empreinte ArcFace d'InsightFace : pré- et post-traitement **purs** (sans ONNX Runtime).
//!
//! Le moteur local ([`crate::onnx`], fonction `onnx`) n'exécute que les deux réseaux ; tout ce qui les entoure est ici,
//! testé sans modèle :
//! - `det_input` : image → tenseur de détection (mise à l'échelle conservant les proportions sur un carré de 640 px,
//!   `(pixel − 127,5) / 128`, RGB, NCHW) ;
//! - `decode` / `nms` : sorties des trois pas (8, 16, 32) → boîtes et 5 repères, suppression des doublons ;
//! - `align` : similitude des 5 repères vers le gabarit ArcFace, rendu 112 × 112 ;
//! - `arcface_input` : visage aligné → tenseur d'empreinte `(pixel − 127,5) / 127,5`.
//!
//! Les conventions (ordre des sorties, deux ancres par position, gabarit des repères) sont celles de la
//! bibliothèque `insightface` (modèles `det_10g.onnx` et `w600k_r50.onnx` du paquet `buffalo_l`).

use image::RgbImage;

/// Côté du carré de détection.
pub const DET_SIZE: u32 = 640;
/// Confiance minimale d'une détection.
pub const DET_THRESH: f32 = 0.5;
/// Recouvrement (IoU) au-delà duquel deux boîtes sont le même visage.
pub const NMS_IOU: f32 = 0.4;
/// Pas des trois têtes de détection.
pub const STRIDES: [u32; 3] = [8, 16, 32];
/// Ancres par position.
pub const ANCHORS: usize = 2;
/// Côté de l'image alignée donnée au réseau d'empreinte.
pub const ALIGNED: u32 = 112;

/// Gabarit des 5 repères (œil gauche, œil droit, nez, coin gauche et droit de la bouche) pour une image de 112 px.
pub const ARCFACE_DST: [[f32; 2]; 5] = [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

/// Visage détecté, dans le repère de l'image d'origine.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// `[x1, y1, x2, y2]`.
    pub bbox: [f32; 4],
    /// Confiance.
    pub score: f32,
    /// Cinq repères `[x, y]`.
    pub kps: [[f32; 2]; 5],
}

/// Tenseur de détection `1 × 3 × 640 × 640` et facteur d'échelle de l'image d'origine vers ce carré.
///
/// L'image est réduite (ou agrandie) pour tenir dans le carré, collée en haut à gauche ; le reste est noir.
pub fn det_input(img: &RgbImage) -> (Vec<f32>, f32) {
    let (w, h) = img.dimensions();
    let size = DET_SIZE;
    let ratio = f64::from(h) / f64::from(w);
    let (nw, nh) = if ratio > 1.0 { (((f64::from(size) / ratio) as u32).max(1), size) } else { (size, ((f64::from(size) * ratio) as u32).max(1)) };
    let scale = nh as f32 / h as f32;
    let resized = image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);
    let plane = (size * size) as usize;
    // Fond noir : (0 − 127,5) / 128.
    let mut out = vec![-127.5 / 128.0; 3 * plane];
    for (x, y, p) in resized.enumerate_pixels() {
        let at = (y * size + x) as usize;
        for c in 0..3 {
            out[c * plane + at] = (f32::from(p[c]) - 127.5) / 128.0;
        }
    }
    (out, scale)
}

/// Décode une tête de détection : `scores` (`n`), `boxes` (`n × 4`, distances en unités de pas), `kps` (`n × 10`,
/// décalages en unités de pas), pour une grille `side × side` de pas `stride` et [`ANCHORS`] ancres par position.
/// Les boîtes rendues sont dans le repère du carré de détection.
pub fn decode(scores: &[f32], boxes: &[f32], kps: &[f32], stride: u32, side: u32) -> Vec<Detection> {
    let n = (side * side) as usize * ANCHORS;
    let stride_f = stride as f32;
    let mut out = Vec::new();
    for i in 0..n.min(scores.len()) {
        if scores[i] < DET_THRESH || boxes.len() < (i + 1) * 4 || kps.len() < (i + 1) * 10 {
            continue;
        }
        // Les deux ancres d'une position sont consécutives ; positions parcourues ligne par ligne.
        let pos = i / ANCHORS;
        let (cx, cy) = (((pos as u32 % side) * stride) as f32, ((pos as u32 / side) * stride) as f32);
        let b = &boxes[i * 4..i * 4 + 4];
        let k = &kps[i * 10..i * 10 + 10];
        let mut pts = [[0f32; 2]; 5];
        for (j, p) in pts.iter_mut().enumerate() {
            *p = [cx + k[2 * j] * stride_f, cy + k[2 * j + 1] * stride_f];
        }
        out.push(Detection { bbox: [cx - b[0] * stride_f, cy - b[1] * stride_f, cx + b[2] * stride_f, cy + b[3] * stride_f], score: scores[i], kps: pts });
    }
    out
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let (x1, y1, x2, y2) = (a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3]));
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Supprime les détections qui recouvrent (IoU ≥ `thresh`) une détection plus sûre.
pub fn nms(mut dets: Vec<Detection>, thresh: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for d in dets {
        if kept.iter().all(|k| iou(&k.bbox, &d.bbox) < thresh) {
            kept.push(d);
        }
    }
    kept
}

/// Ramène des détections du carré de détection vers l'image d'origine.
pub fn rescale(dets: &mut [Detection], scale: f32) {
    for d in dets {
        d.bbox.iter_mut().for_each(|v| *v /= scale);
        d.kps.iter_mut().flatten().for_each(|v| *v /= scale);
    }
}

/// Similitude (rotation, échelle uniforme, translation) qui envoie `src` au plus près de `dst` (moindres carrés).
/// Rend `[a, b, tx, ty]` : `x' = a·x − b·y + tx`, `y' = b·x + a·y + ty`.
pub fn similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> Option<[f32; 4]> {
    let n = 5.0f64;
    let mean = |p: &[[f32; 2]; 5]| (p.iter().map(|q| f64::from(q[0])).sum::<f64>() / n, p.iter().map(|q| f64::from(q[1])).sum::<f64>() / n);
    let ((sx, sy), (dx, dy)) = (mean(src), mean(dst));
    // Nombres complexes : w = Σ conj(s)·d / Σ |s|², sur les points centrés.
    let (mut num_re, mut num_im, mut den) = (0.0f64, 0.0f64, 0.0f64);
    for (s, d) in src.iter().zip(dst) {
        let (a, b) = (f64::from(s[0]) - sx, f64::from(s[1]) - sy);
        let (c, e) = (f64::from(d[0]) - dx, f64::from(d[1]) - dy);
        num_re += a * c + b * e;
        num_im += a * e - b * c;
        den += a * a + b * b;
    }
    if den < 1e-9 {
        return None;
    }
    let (a, b) = (num_re / den, num_im / den);
    let (tx, ty) = (dx - (a * sx - b * sy), dy - (b * sx + a * sy));
    Some([a as f32, b as f32, tx as f32, ty as f32])
}

/// Visage aligné `112 × 112` : les 5 repères de `kps` sont envoyés sur [`ARCFACE_DST`] (rendu bilinéaire, noir hors image).
pub fn align(img: &RgbImage, kps: &[[f32; 2]; 5]) -> Option<RgbImage> {
    let [a, b, tx, ty] = similarity(kps, &ARCFACE_DST)?;
    let norm = a * a + b * b;
    if norm < 1e-12 {
        return None;
    }
    let (w, h) = img.dimensions();
    let mut out = RgbImage::new(ALIGNED, ALIGNED);
    for (u, v, px) in out.enumerate_pixels_mut() {
        // Transformation inverse : (x, y) = M⁻¹ (u, v).
        let (du, dv) = (u as f32 - tx, v as f32 - ty);
        let (x, y) = ((a * du + b * dv) / norm, (-b * du + a * dv) / norm);
        if x < 0.0 || y < 0.0 || x > (w - 1) as f32 || y > (h - 1) as f32 {
            continue;
        }
        let (x0, y0) = (x.floor() as u32, y.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        for c in 0..3 {
            let p = |xx: u32, yy: u32| f32::from(img.get_pixel(xx, yy)[c]);
            let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
            let bottom = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
            px[c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
        }
    }
    Some(out)
}

/// Tenseur d'empreinte `1 × 3 × 112 × 112` d'un visage aligné.
pub fn arcface_input(face: &RgbImage) -> Vec<f32> {
    let plane = (ALIGNED * ALIGNED) as usize;
    let mut out = vec![0f32; 3 * plane];
    for (x, y, p) in face.enumerate_pixels() {
        let at = (y * ALIGNED + x) as usize;
        for c in 0..3 {
            out[c * plane + at] = (f32::from(p[c]) - 127.5) / 127.5;
        }
    }
    out
}

/// Visages d'une image à partir des neuf sorties de SCRFD, dans l'ordre d'InsightFace : scores des pas 8, 16, 32,
/// puis boîtes, puis repères. `scale` : facteur rendu par [`det_input`].
pub fn detections(outputs: &[Vec<f32>], scale: f32) -> Result<Vec<Detection>, String> {
    if outputs.len() != 9 {
        return Err(format!("SCRFD : 9 sorties attendues, {} reçues", outputs.len()));
    }
    let mut all = Vec::new();
    for (i, stride) in STRIDES.iter().enumerate() {
        let side = DET_SIZE / stride;
        let n = (side * side) as usize * ANCHORS;
        let (s, b, k) = (&outputs[i], &outputs[i + 3], &outputs[i + 6]);
        if s.len() != n || b.len() != n * 4 || k.len() != n * 10 {
            return Err(format!("SCRFD : sortie du pas {stride} de taille inattendue ({} / {} / {}, attendu {n} / {} / {})", s.len(), b.len(), k.len(), n * 4, n * 10));
        }
        all.extend(decode(s, b, k, *stride, side));
    }
    let mut kept = nms(all, NMS_IOU);
    rescale(&mut kept, scale);
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_detection_input_keeps_proportions_pads_with_black_and_normalises() {
        let img = RgbImage::from_pixel(320, 160, image::Rgb([255, 127, 0])); // 2 : 1 → 640 × 320
        let (t, scale) = det_input(&img);
        assert_eq!(t.len(), 3 * 640 * 640);
        assert!((scale - 2.0).abs() < 1e-6, "{scale}");
        let at = |c: usize, x: usize, y: usize| t[c * 640 * 640 + y * 640 + x];
        assert!((at(0, 10, 10) - (255.0 - 127.5) / 128.0).abs() < 1e-4 && (at(2, 10, 10) - (0.0 - 127.5) / 128.0).abs() < 1e-4, "RGB, canal par canal");
        assert!((at(1, 10, 10) - (127.0 - 127.5) / 128.0).abs() < 1e-4);
        assert!((at(0, 10, 500) + 127.5 / 128.0).abs() < 1e-6, "sous l'image : fond noir");
        // Portrait : c'est la largeur qui est réduite.
        let (_, s2) = det_input(&RgbImage::new(100, 400));
        assert!((s2 - 1.6).abs() < 1e-6, "{s2}");
    }

    #[test]
    fn a_head_is_decoded_to_boxes_and_landmarks_in_pixels() {
        // Grille 80 × 80 au pas 8 ; une ancre active à la position (x = 3, y = 2), deuxième ancre.
        let side = 80u32;
        let n = (side * side) as usize * ANCHORS;
        let (mut s, mut b, mut k) = (vec![0.0; n], vec![0.0; n * 4], vec![0.0; n * 10]);
        let i = (2 * side as usize + 3) * ANCHORS + 1;
        s[i] = 0.9;
        b[i * 4..i * 4 + 4].copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        for j in 0..5 {
            k[i * 10 + 2 * j] = j as f32;
            k[i * 10 + 2 * j + 1] = -(j as f32);
        }
        let d = decode(&s, &b, &k, 8, side);
        assert_eq!(d.len(), 1);
        // Centre (24, 16) ; distances × 8.
        assert_eq!(d[0].bbox, [24.0 - 8.0, 16.0 - 16.0, 24.0 + 24.0, 16.0 + 32.0]);
        assert_eq!(d[0].kps[0], [24.0, 16.0]);
        assert_eq!(d[0].kps[4], [24.0 + 32.0, 16.0 - 32.0]);
        assert!(decode(&vec![0.4; n], &b, &k, 8, side).is_empty(), "sous le seuil de confiance");
        assert!(decode(&s, &[], &k, 8, side).is_empty(), "sorties tronquées : rien d'inventé");
    }

    #[test]
    fn overlapping_boxes_are_merged_keeping_the_surest() {
        let d = |x: f32, score: f32| Detection { bbox: [x, 0.0, x + 100.0, 100.0], score, kps: [[0.0; 2]; 5] };
        let kept = nms(vec![d(0.0, 0.7), d(10.0, 0.95), d(300.0, 0.6), d(15.0, 0.8)], NMS_IOU);
        assert_eq!(kept.iter().map(|k| k.score).collect::<Vec<_>>(), [0.95, 0.6]);
        let mut one = vec![d(0.0, 0.9)];
        rescale(&mut one, 2.0);
        assert_eq!(one[0].bbox, [0.0, 0.0, 50.0, 50.0]);
    }

    #[test]
    fn the_similarity_recovers_a_known_rotation_scale_and_translation() {
        // Applique x' = a·x − b·y + tx à des repères quelconques, puis retrouve (a, b, tx, ty).
        let (a, b, tx, ty) = (0.8f32, 0.3f32, 12.0f32, -7.0f32);
        let src = [[10.0, 20.0], [50.0, 22.0], [30.0, 40.0], [15.0, 60.0], [48.0, 61.0]];
        let dst = src.map(|p| [a * p[0] - b * p[1] + tx, b * p[0] + a * p[1] + ty]);
        let m = similarity(&src, &dst).unwrap();
        for (got, want) in m.iter().zip([a, b, tx, ty]) {
            assert!((got - want).abs() < 1e-3, "{m:?}");
        }
        assert!(similarity(&[[1.0, 1.0]; 5], &dst).is_none(), "repères confondus : pas de transformation");
    }

    #[test]
    fn alignment_puts_the_landmarks_on_the_arcface_template() {
        // Image 200 × 200 dont on marque en blanc les 5 repères d'un visage tourné de 15° et deux fois trop grand.
        let (ang, scale) = (15f32.to_radians(), 2.0f32);
        let (c, s) = (ang.cos() * scale, ang.sin() * scale);
        let kps = ARCFACE_DST.map(|p| [c * (p[0] - 56.0) - s * (p[1] - 56.0) + 100.0, s * (p[0] - 56.0) + c * (p[1] - 56.0) + 100.0]);
        let mut img = RgbImage::from_pixel(200, 200, image::Rgb([20, 20, 20]));
        for k in &kps {
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    img.put_pixel((k[0] as i32 + dx) as u32, (k[1] as i32 + dy) as u32, image::Rgb([255, 255, 255]));
                }
            }
        }
        let face = align(&img, &kps).unwrap();
        assert_eq!(face.dimensions(), (112, 112));
        for d in ARCFACE_DST {
            let (x, y) = (d[0].round() as u32, d[1].round() as u32);
            assert!(face.get_pixel(x, y)[0] > 150, "le repère {d:?} doit tomber sur le gabarit : {}", face.get_pixel(x, y)[0]);
        }
        assert!(face.get_pixel(2, 2)[0] < 60, "loin des repères : fond");
        // Hors de l'image source : noir, jamais de lecture hors limites.
        let far = ARCFACE_DST.map(|p| [p[0] + 5000.0, p[1]]);
        assert!(align(&img, &far).unwrap().pixels().all(|p| p[0] == 0));
    }

    #[test]
    fn the_embedding_input_is_centered_rgb_planar() {
        let t = arcface_input(&RgbImage::from_pixel(112, 112, image::Rgb([255, 0, 127])));
        assert_eq!(t.len(), 3 * 112 * 112);
        assert!((t[0] - 1.0).abs() < 1e-6 && (t[112 * 112] + 1.0).abs() < 1e-6 && t[2 * 112 * 112].abs() < 0.01);
    }

    #[test]
    fn the_nine_outputs_are_checked_before_use() {
        assert!(detections(&[], 1.0).is_err());
        let sizes = [80 * 80 * 2, 40 * 40 * 2, 20 * 20 * 2];
        let mut outs: Vec<Vec<f32>> = Vec::new();
        for m in [1usize, 4, 10] {
            for n in sizes {
                outs.push(vec![0.0; n * m]);
            }
        }
        let got = detections(&outs, 1.0).unwrap();
        assert!(got.is_empty());
        outs[0].pop();
        assert!(detections(&outs, 1.0).unwrap_err().contains("taille inattendue"));
    }
}
