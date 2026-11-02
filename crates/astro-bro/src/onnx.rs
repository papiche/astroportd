//! Embeddings **locaux** de la mémoire de BRO : `bge-m3` quantifié int8 par ONNX Runtime, dans le binaire.
//!
//! Remplace l'appel HTTP à Ollama (`/api/embed`) pour les vecteurs : mêmes 1024 dimensions, donc les souvenirs déjà
//! indexés restent comparables. La bibliothèque `libonnxruntime` est chargée **à l'exécution** (voir `astro-facecloud::onnx`,
//! qui partage le même environnement ONNX : le second chargement est sans effet).
//!
//! Les deux fichiers ne sont **pas** fournis : `bge-m3-int8.onnx` (`onnx/model_int8.onnx` du dépôt Hugging Face
//! `Xenova/bge-m3`, licence MIT, ≈ 540 Mo) et `bge-m3-tokenizer.json` (son `tokenizer.json`, ≈ 16 Mo).
//!
//! `bge-m3` s'utilise en **pooling CLS** puis normalisation L2 : le modèle ne rend que `last_hidden_state`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use tokenizers::{Tokenizer, TruncationParams};

use crate::BroError;
use crate::ollama::{EMBED_DIM, Embedder, MAX_INPUT_CHARS};

/// Nom du modèle dans le dossier des modèles.
pub const MODEL_FILE: &str = "bge-m3-int8.onnx";
/// Nom du tokeniseur dans le dossier des modèles.
pub const TOKENIZER_FILE: &str = "bge-m3-tokenizer.json";
/// Jetons conservés au plus par texte (le reste est tronqué, comme Ollama le fait avec sa fenêtre de contexte).
pub const MAX_TOKENS: usize = 1024;

/// Dossiers où chercher les deux fichiers quand aucun dossier n'est donné.
pub fn default_model_dirs(data_dir: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = data_dir.map(|d| d.join("models")).into_iter().collect();
    dirs.extend(home.map(|h| h.join(".zen/astroportd/models")));
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// Le dossier contient-il le modèle et son tokeniseur ?
pub fn has_models(dir: &Path) -> bool {
    dir.join(MODEL_FILE).is_file() && dir.join(TOKENIZER_FILE).is_file()
}

/// Premier dossier par défaut qui contient le modèle et son tokeniseur.
pub fn find_models(data_dir: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    default_model_dirs(data_dir, home).into_iter().find(|d| has_models(d))
}

fn model(e: impl std::fmt::Display) -> BroError {
    BroError::Model(e.to_string())
}

struct Inner {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
}

/// Moteur d'embedding local (`bge-m3` int8). Se clone à peu de frais ; une inférence à la fois.
#[derive(Clone)]
pub struct OrtEmbedder {
    inner: Arc<Inner>,
}

impl OrtEmbedder {
    /// Charge le modèle et son tokeniseur depuis `dir`. `lib` : bibliothèque ONNX Runtime à charger (sinon `ORT_DYLIB_PATH`
    /// ou le chemin du système) ; `threads` : fils de calcul (0 = au choix d'ORT).
    ///
    /// `ort` **panique** quand `libonnxruntime` est introuvable : on rattrape la panique pour la rendre en erreur, jamais en arrêt de la station.
    pub fn load(dir: &Path, lib: Option<&Path>, threads: usize) -> Result<Self, BroError> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Self::load_inner(dir, lib, threads)))
            .unwrap_or_else(|_| Err(BroError::Model("ONNX Runtime : bibliothèque introuvable ou illisible (ORT_DYLIB_PATH, ou pip install onnxruntime)".into())))
    }

    fn load_inner(dir: &Path, lib: Option<&Path>, threads: usize) -> Result<Self, BroError> {
        if let Some(lib) = lib {
            ort::init_from(lib.to_string_lossy()).commit().map_err(|e| BroError::Model(format!("ONNX Runtime ({}) : {e}", lib.display())))?;
        }
        let (model_path, tok_path) = (dir.join(MODEL_FILE), dir.join(TOKENIZER_FILE));
        for p in [&model_path, &tok_path] {
            if !p.is_file() {
                return Err(BroError::Model(format!("fichier introuvable : {}", p.display())));
            }
        }
        let mut tokenizer = Tokenizer::from_file(&tok_path).map_err(|e| BroError::Model(format!("{} : {e}", tok_path.display())))?;
        tokenizer.with_truncation(Some(TruncationParams { max_length: MAX_TOKENS, ..TruncationParams::default() })).map_err(|e| BroError::Model(format!("troncature : {e}")))?;
        let mut b = Session::builder().map_err(model)?.with_optimization_level(GraphOptimizationLevel::Level3).map_err(model)?;
        if threads > 0 {
            b = b.with_intra_threads(threads).map_err(model)?;
        }
        let session = b.commit_from_file(&model_path).map_err(|e| BroError::Model(format!("{} : {e}", model_path.display())))?;
        Ok(Self { inner: Arc::new(Inner { session: Mutex::new(session), tokenizer }) })
    }

    /// Vecteur normalisé de `text`. Bloquant : à appeler hors de la boucle asynchrone.
    pub fn embed_blocking(&self, text: &str) -> Result<Vec<f32>, BroError> {
        let input: String = text.chars().take(MAX_INPUT_CHARS).collect();
        let enc = self.inner.tokenizer.encode(input, true).map_err(|e| BroError::Model(format!("tokenisation : {e}")))?;
        let n = enc.get_ids().len();
        if n == 0 {
            return Err(BroError::Model("aucun jeton à plonger".into()));
        }
        let ids: Vec<i64> = enc.get_ids().iter().map(|&i| i64::from(i)).collect();
        let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&m| i64::from(m)).collect();
        let mut session = self.inner.session.lock().unwrap_or_else(|e| e.into_inner());
        let outputs = session
            .run(ort::inputs!["input_ids" => Tensor::from_array(([1usize, n], ids)).map_err(model)?, "attention_mask" => Tensor::from_array(([1usize, n], mask)).map_err(model)?])
            .map_err(model)?;
        let (shape, data) = outputs["last_hidden_state"].try_extract_tensor::<f32>().map_err(model)?;
        if shape.len() != 3 || shape[2] as usize != EMBED_DIM || data.len() < EMBED_DIM {
            return Err(BroError::Model(format!("bge-m3 : forme {shape:?} inattendue, [1, n, {EMBED_DIM}] attendue")));
        }
        // Pooling CLS : le premier jeton résume la phrase.
        let mut v = data[..EMBED_DIM].to_vec();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if !norm.is_finite() || norm < 1e-6 {
            return Err(BroError::Model("bge-m3 : vecteur nul ou non fini".into()));
        }
        v.iter_mut().for_each(|x| *x /= norm);
        Ok(v)
    }
}

impl Embedder for OrtEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, BroError> {
        let (engine, text) = (self.clone(), text.to_owned());
        tokio::task::spawn_blocking(move || engine.embed_blocking(&text)).await.map_err(|e| BroError::Model(format!("inférence interrompue : {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_are_found_only_when_both_files_are_there() {
        let d = std::env::temp_dir().join(format!("astro-bge-{}", std::process::id()));
        let own = d.join("data/models");
        std::fs::create_dir_all(&own).unwrap();
        assert!(find_models(Some(&d.join("data")), None).is_none());
        std::fs::write(own.join(MODEL_FILE), b"").unwrap();
        assert!(find_models(Some(&d.join("data")), None).is_none(), "tokeniseur absent : incomplet");
        std::fs::write(own.join(TOKENIZER_FILE), b"").unwrap();
        assert_eq!(find_models(Some(&d.join("data")), None), Some(own));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_missing_runtime_library_is_an_error_not_a_panic() {
        let d = std::env::temp_dir().join(format!("astro-bge-lib-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MODEL_FILE), b"").unwrap();
        std::fs::write(d.join(TOKENIZER_FILE), b"").unwrap();
        let err = OrtEmbedder::load(&d, Some(Path::new("/inexistant/libonnxruntime.so")), 1).err().unwrap().to_string();
        assert!(err.contains("ONNX Runtime"), "{err}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_missing_file_is_named_not_swallowed() {
        let err = OrtEmbedder::load(Path::new("/inexistant"), None, 1).err().unwrap().to_string();
        assert!(err.contains(MODEL_FILE), "{err}");
    }
}
