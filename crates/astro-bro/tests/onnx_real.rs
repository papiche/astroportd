//! Embeddings locaux avec le VRAI `bge-m3` int8. Ignoré tant que les fichiers et la bibliothèque ne sont pas fournis (ils
//! ne sont pas dans le dépôt) :
//!
//! ```text
//! ASTRO_BGE_DIR=/chemin          # contient bge-m3-int8.onnx et bge-m3-tokenizer.json
//! ASTRO_BGE_REF=/chemin/ref.json # facultatif : vecteurs de référence [{"text","vec"}] (onnxruntime Python, CLS + L2)
//! ORT_DYLIB_PATH=/chemin/libonnxruntime.so
//! cargo test -p astro-bro --features onnx --test onnx_real -- --nocapture
//! ```
#![cfg(feature = "onnx")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use astro_bro::Embedder;
use astro_bro::ollama::EMBED_DIM;
use astro_bro::onnx::OrtEmbedder;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn engine() -> Option<OrtEmbedder> {
    let dir = PathBuf::from(std::env::var_os("ASTRO_BGE_DIR")?);
    let lib = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from);
    Some(OrtEmbedder::load(&dir, lib.as_deref(), 0).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_local_engine_ranks_french_sentences_by_meaning() {
    let Some(e) = engine() else {
        eprintln!("ASTRO_BGE_DIR absent : test ignoré");
        return;
    };
    let q = e.embed("pain au levain du dimanche").await.unwrap();
    assert_eq!(q.len(), EMBED_DIM);
    assert!((cosine(&q, &q) - 1.0).abs() < 1e-4, "normalisé L2");
    let near = e.embed("Le boulanger fait cuire une miche de pain au levain.").await.unwrap();
    let far = e.embed("courir un marathon en avril").await.unwrap();
    println!("proche {:.3} · lointain {:.3}", cosine(&q, &near), cosine(&q, &far));
    assert!(cosine(&q, &near) > cosine(&q, &far) + 0.1, "le sens prime sur les mots");
    // Texte très long : tronqué, pas d'échec ni d'explosion mémoire.
    let long = "souvenir de vacances ".repeat(4000);
    assert_eq!(e.embed(&long).await.unwrap().len(), EMBED_DIM);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_local_engine_matches_the_python_reference() {
    let (Some(e), Some(reference)) = (engine(), std::env::var_os("ASTRO_BGE_REF")) else {
        eprintln!("ASTRO_BGE_DIR / ASTRO_BGE_REF absents : test ignoré");
        return;
    };
    let items: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(Path::new(&reference)).unwrap()).unwrap();
    for it in items {
        let text = it["text"].as_str().unwrap();
        let want: Vec<f32> = it["vec"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        let got = e.embed(text).await.unwrap();
        let c = cosine(&got, &want);
        println!("{c:.6}  {text}");
        assert!(c > 0.999, "« {text} » : cosinus {c} avec la référence");
    }
}
