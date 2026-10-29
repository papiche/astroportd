//! Moteur FaceID **local** : SCRFD (détection) puis ArcFace (empreinte 512D) par ONNX Runtime, dans le binaire.
//!
//! Remplace le service HTTP (port 8000) sur les stations autonomes. La bibliothèque `libonnxruntime` est chargée **à
//! l'exécution** (`ort/load-dynamic`) : rien n'est téléchargé à la compilation et la compilation croisée reste simple.
//! Elle se cherche dans `ORT_DYLIB_PATH`, sinon dans les chemins du système.
//!
//! Les deux modèles ne sont **pas** fournis : `det_10g.onnx` (SCRFD) et `w600k_r50.onnx` (ArcFace) du paquet InsightFace
//! `buffalo_l`. Leurs poids sont publiés pour la recherche non commerciale : vérifiez la licence avant tout usage commercial.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use image::RgbImage;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::engine::{BBox, DIM, DetectedFace, EngineError, FaceEngine, l2_normalize};
use crate::scrfd;

/// Nom du modèle de détection dans le dossier des modèles.
pub const DETECTOR_FILE: &str = "det_10g.onnx";
/// Nom du modèle d'empreinte dans le dossier des modèles.
pub const RECOGNIZER_FILE: &str = "w600k_r50.onnx";

/// Dossiers où chercher `libonnxruntime.so*` quand `ORT_DYLIB_PATH` n'est pas défini.
fn library_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".zen/astroportd/lib"));
        // Un onnxruntime déjà installé par pip (venv Astroport.ONE) : `~/.astro/lib/python3.*/site-packages/onnxruntime/capi`.
        if let Ok(rd) = std::fs::read_dir(h.join(".astro/lib")) {
            dirs.extend(rd.flatten().map(|e| e.path().join("site-packages/onnxruntime/capi")));
        }
    }
    dirs.extend(["/usr/local/lib", "/usr/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib/arm-linux-gnueabihf"].map(PathBuf::from));
    dirs
}

/// La bibliothèque ONNX Runtime à charger : `ORT_DYLIB_PATH` s'il est défini, sinon la plus récente trouvée aux
/// emplacements habituels (voir [`library_dirs`]).
pub fn find_library(env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = env.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let mut found: Vec<PathBuf> = library_dirs(home)
        .into_iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("libonnxruntime.so") && !n.contains("providers")))
        .collect();
    found.sort();
    found.pop()
}

/// Dossiers où chercher les modèles quand `--face-models` n'est pas donné.
pub fn default_model_dirs(data_dir: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = data_dir.map(|d| d.join("models")).into_iter().collect();
    if let Some(h) = home {
        dirs.push(h.join(".zen/astroportd/models"));
        dirs.push(h.join(".insightface/models/buffalo_l"));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// Le dossier contient-il les deux modèles ?
pub fn has_models(dir: &Path) -> bool {
    dir.join(DETECTOR_FILE).is_file() && dir.join(RECOGNIZER_FILE).is_file()
}

/// Premier dossier par défaut qui contient les deux modèles.
pub fn find_models(data_dir: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    default_model_dirs(data_dir, home).into_iter().find(|d| has_models(d))
}

fn backend(e: impl std::fmt::Display) -> EngineError {
    EngineError::Backend(e.to_string())
}

/// Moteur FaceID local.
#[derive(Clone)]
pub struct OrtFaceEngine {
    detector: Arc<Mutex<Session>>,
    recognizer: Arc<Mutex<Session>>,
}

impl OrtFaceEngine {
    /// Charge `det_10g.onnx` et `w600k_r50.onnx` depuis `dir`. `threads` : fils de calcul par modèle (0 = au choix d'ORT).
    pub fn load(dir: &Path, threads: usize) -> Result<Self, EngineError> {
        // Bibliothèque : `ORT_DYLIB_PATH`, sinon recherchée aux emplacements habituels ; sans elle, ort le dit clairement.
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(lib) = find_library(std::env::var("ORT_DYLIB_PATH").ok().as_deref(), home.as_deref()) {
            ort::init_from(lib.to_string_lossy()).commit().map_err(|e| EngineError::Backend(format!("ONNX Runtime ({}) : {e}", lib.display())))?;
        }
        let open = |file: &str| -> Result<Session, EngineError> {
            let path: PathBuf = dir.join(file);
            if !path.is_file() {
                return Err(EngineError::Backend(format!("modèle introuvable : {}", path.display())));
            }
            let mut b = Session::builder().map_err(backend)?.with_optimization_level(GraphOptimizationLevel::Level3).map_err(backend)?;
            if threads > 0 {
                b = b.with_intra_threads(threads).map_err(backend)?;
            }
            b.commit_from_file(&path).map_err(|e| EngineError::Backend(format!("{} : {e}", path.display())))
        };
        Ok(Self { detector: Arc::new(Mutex::new(open(DETECTOR_FILE)?)), recognizer: Arc::new(Mutex::new(open(RECOGNIZER_FILE)?)) })
    }

    fn run(session: &Mutex<Session>, input: Vec<f32>, shape: [usize; 4]) -> Result<Vec<Vec<f32>>, EngineError> {
        let mut s = session.lock().unwrap_or_else(|e| e.into_inner());
        let name = s.inputs.first().map(|i| i.name.clone()).ok_or_else(|| EngineError::Backend("modèle sans entrée".into()))?;
        let names: Vec<String> = s.outputs.iter().map(|o| o.name.clone()).collect();
        let tensor = Tensor::from_array((shape, input)).map_err(backend)?;
        let outputs = s.run(ort::inputs![name.as_str() => tensor]).map_err(backend)?;
        names
            .iter()
            .map(|n| {
                let (_, data) = outputs[n.as_str()].try_extract_tensor::<f32>().map_err(backend)?;
                Ok(data.to_vec())
            })
            .collect()
    }

    /// Détection et empreintes d'une image décodée. Bloquant : à appeler hors de la boucle asynchrone.
    pub fn infer(&self, img: &RgbImage) -> Result<Vec<DetectedFace>, EngineError> {
        let (input, scale) = scrfd::det_input(img);
        let side = scrfd::DET_SIZE as usize;
        let outs = Self::run(&self.detector, input, [1, 3, side, side])?;
        let dets = scrfd::detections(&outs, scale).map_err(EngineError::Backend)?;
        let mut faces = Vec::with_capacity(dets.len());
        for d in dets {
            let Some(aligned) = scrfd::align(img, &d.kps) else { continue };
            let side = scrfd::ALIGNED as usize;
            let mut outs = Self::run(&self.recognizer, scrfd::arcface_input(&aligned), [1, 3, side, side])?;
            let mut embedding = outs.pop().ok_or_else(|| EngineError::Backend("ArcFace : aucune sortie".into()))?;
            if embedding.len() != DIM {
                return Err(EngineError::Backend(format!("ArcFace : {} composantes au lieu de {DIM}", embedding.len())));
            }
            l2_normalize(&mut embedding)?;
            faces.push(DetectedFace { bbox: BBox { x1: d.bbox[0], y1: d.bbox[1], x2: d.bbox[2], y2: d.bbox[3] }, det_score: d.score, embedding });
        }
        Ok(faces)
    }
}

/// Décode une image encodée, orientation EXIF appliquée (voir [`crate::imageio`]) et avec des limites contre les images piégées.
pub fn decode_image(bytes: &[u8]) -> Result<RgbImage, EngineError> {
    crate::imageio::decode_oriented(bytes).map_err(|e| EngineError::Backend(format!("image illisible : {e}")))
}

impl FaceEngine for OrtFaceEngine {
    async fn detect(&self, image: &[u8]) -> Result<Vec<DetectedFace>, EngineError> {
        let (engine, bytes) = (self.clone(), image.to_vec());
        tokio::task::spawn_blocking(move || engine.infer(&decode_image(&bytes)?)).await.map_err(|e| EngineError::Backend(format!("inférence interrompue : {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("astro-onnx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_runtime_library_is_taken_from_the_environment_or_found_in_usual_places() {
        let home = tmp("lib");
        let capi = home.join(".astro/lib/python3.12/site-packages/onnxruntime/capi");
        std::fs::create_dir_all(&capi).unwrap();
        std::fs::write(capi.join("libonnxruntime.so.1.20.0"), b"").unwrap();
        std::fs::write(capi.join("libonnxruntime.so.1.27.0"), b"").unwrap();
        std::fs::write(capi.join("libonnxruntime_providers_shared.so"), b"").unwrap();
        assert_eq!(find_library(None, Some(&home)).unwrap().file_name().unwrap(), "libonnxruntime.so.1.27.0", "la plus récente, jamais un module de fournisseur");
        assert_eq!(find_library(Some("/choix/explicite.so"), Some(&home)), Some(PathBuf::from("/choix/explicite.so")), "l'environnement l'emporte");
        assert_eq!(find_library(Some(""), Some(&home)).unwrap().file_name().unwrap(), "libonnxruntime.so.1.27.0", "variable vide : ignorée");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn models_are_found_in_the_data_directory_or_the_insightface_default() {
        let (home, data) = (tmp("home"), tmp("data"));
        assert!(find_models(Some(&data), Some(&home)).is_none());
        let insight = home.join(".insightface/models/buffalo_l");
        std::fs::create_dir_all(&insight).unwrap();
        std::fs::write(insight.join(DETECTOR_FILE), b"").unwrap();
        assert!(find_models(Some(&data), Some(&home)).is_none(), "un seul modèle : incomplet");
        std::fs::write(insight.join(RECOGNIZER_FILE), b"").unwrap();
        assert_eq!(find_models(Some(&data), Some(&home)), Some(insight));
        let own = data.join("models");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join(DETECTOR_FILE), b"").unwrap();
        std::fs::write(own.join(RECOGNIZER_FILE), b"").unwrap();
        assert_eq!(find_models(Some(&data), Some(&home)), Some(own), "le dossier de la station prime");
        let _ = (std::fs::remove_dir_all(home), std::fs::remove_dir_all(data));
    }
}
