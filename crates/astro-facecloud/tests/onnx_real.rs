//! Inférence locale avec de VRAIS modèles InsightFace. Ignoré tant que les modèles et la bibliothèque ne sont pas
//! fournis (ils ne sont pas dans le dépôt) :
//!
//! ```text
//! ASTRO_FACE_MODELS=/chemin/buffalo_l          # contient det_10g.onnx et w600k_r50.onnx
//! ASTRO_FACE_IMAGES=a.jpg,b.jpg                # deux photos de deux personnes différentes, un visage chacune
//! ORT_DYLIB_PATH=/chemin/libonnxruntime.so
//! cargo test -p astro-facecloud --features onnx --test onnx_real -- --nocapture
//! ```
#![cfg(feature = "onnx")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use astro_facecloud::onnx::{OrtFaceEngine, decode_image};
use astro_facecloud::{DetectedFace, FaceEngine};
use image::{DynamicImage, RgbImage};

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn png(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(img.clone()).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

fn best(faces: &[DetectedFace]) -> &DetectedFace {
    faces.iter().max_by(|a, b| a.det_score.total_cmp(&b.det_score)).expect("au moins un visage")
}

#[tokio::test(flavor = "multi_thread")]
async fn real_models_detect_align_and_embed_consistently() {
    let (Some(models), Some(images)) = (std::env::var_os("ASTRO_FACE_MODELS"), std::env::var("ASTRO_FACE_IMAGES").ok()) else {
        eprintln!("ASTRO_FACE_MODELS / ASTRO_FACE_IMAGES absents : test ignoré");
        return;
    };
    let paths: Vec<PathBuf> = images.split(',').map(PathBuf::from).collect();
    assert_eq!(paths.len(), 2, "deux images : deux personnes différentes");
    let engine = OrtFaceEngine::load(&PathBuf::from(models), 2).expect("modèles chargés");
    let (a, b) = (decode_image(&std::fs::read(&paths[0]).unwrap()).unwrap(), decode_image(&std::fs::read(&paths[1]).unwrap()).unwrap());

    let t = std::time::Instant::now();
    let fa = engine.detect(&png(&a)).await.unwrap();
    eprintln!("détection + empreinte d'une image {}×{} : {:?}, {} visage(s)", a.width(), a.height(), t.elapsed(), fa.len());
    let fb = engine.detect(&png(&b)).await.unwrap();
    let (pa, pb) = (best(&fa), best(&fb));
    for (f, img) in [(pa, &a), (pb, &b)] {
        assert!(f.det_score > 0.7, "confiance {}", f.det_score);
        assert_eq!(f.embedding.len(), 512);
        assert!((cosine(&f.embedding, &f.embedding) - 1.0).abs() < 1e-3, "embedding normalisé");
        let (w, h) = (f.bbox.x2 - f.bbox.x1, f.bbox.y2 - f.bbox.y1);
        assert!(f.bbox.x1 >= -2.0 && f.bbox.y1 >= -2.0 && f.bbox.x2 <= img.width() as f32 + 2.0 && f.bbox.y2 <= img.height() as f32 + 2.0, "boîte dans l'image : {:?}", f.bbox);
        assert!(w > 30.0 && h > 30.0 && (w / h) > 0.5 && (w / h) < 1.6, "boîte plausible : {w} × {h}");
    }

    // Même personne, autre prise de vue : réduite, déplacée dans un grand fond gris, retournée.
    let small = image::imageops::resize(&a, a.width() * 3 / 5, a.height() * 3 / 5, image::imageops::FilterType::Triangle);
    let mut canvas = RgbImage::from_pixel(a.width() + 300, a.height() + 200, image::Rgb([128, 128, 128]));
    image::imageops::overlay(&mut canvas, &small, 220, 120);
    let moved = best(&engine.detect(&png(&canvas)).await.unwrap()).embedding.clone();
    let flipped = best(&engine.detect(&png(&image::imageops::flip_horizontal(&a))).await.unwrap()).embedding.clone();
    let (same_scale, same_flip, other) = (cosine(&pa.embedding, &moved), cosine(&pa.embedding, &flipped), cosine(&pa.embedding, &pb.embedding));
    eprintln!("cosinus : réduite/déplacée {same_scale:.3} · retournée {same_flip:.3} · autre personne {other:.3}");
    assert!(same_scale > 0.8 && same_flip > 0.8, "la même personne se reconnaît : {same_scale} {same_flip}");
    assert!(other < 0.4, "deux personnes différentes restent éloignées : {other}");

    // Photo de groupe : les deux visages côte à côte sont trouvés, chacun reconnu.
    let h = a.height().max(b.height());
    let mut group = RgbImage::from_pixel(a.width() + b.width(), h, image::Rgb([0, 0, 0]));
    image::imageops::overlay(&mut group, &a, 0, 0);
    image::imageops::overlay(&mut group, &b, i64::from(a.width()), 0);
    let faces = engine.detect(&png(&group)).await.unwrap();
    let mut strong: Vec<&DetectedFace> = faces.iter().filter(|f| f.det_score > 0.7).collect();
    strong.sort_by(|x, y| x.bbox.x1.total_cmp(&y.bbox.x1));
    assert_eq!(strong.len(), 2, "deux visages : {:?}", faces.iter().map(|f| (f.det_score, f.bbox)).collect::<Vec<_>>());
    assert!(cosine(&strong[0].embedding, &pa.embedding) > 0.8 && cosine(&strong[1].embedding, &pb.embedding) > 0.8, "chacun est reconnu dans la photo de groupe");
    assert!(strong[1].bbox.x1 > a.width() as f32 * 0.9, "le second visage est bien dans la partie droite (repère de l'image d'origine)");

    // Une image sans visage ne produit rien (et n'échoue pas) ; une image illisible est une erreur claire.
    assert!(engine.detect(&png(&RgbImage::from_pixel(300, 300, image::Rgb([30, 90, 60])))).await.unwrap().is_empty());
    assert!(engine.detect(b"pas une image").await.is_err());
}

#[test]
fn missing_models_are_reported_with_their_path() {
    let e = match OrtFaceEngine::load(&PathBuf::from("/nulle/part"), 1) {
        Ok(_) => panic!("le chargement aurait dû échouer"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("modèle introuvable") && e.contains("det_10g.onnx"), "{e}");
}
