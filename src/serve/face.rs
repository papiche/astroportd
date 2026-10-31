//! Choix du moteur de reconnaissance des visages au démarrage.

use super::*;
use astroportd::station_admin::{FaceControl, FaceMode, load_local_engine, local_models_found};

/// Moteur FaceID : une option de ligne de commande l'impose (réglage figé dans l'interface) ; sinon le choix du Capitaine
/// (enregistré) l'emporte ; sinon, si les deux modèles sont aux emplacements habituels (binaire compilé avec `onnx`),
/// l'inférence locale s'active toute seule ; `astroportd face-check` dit ce qui manque. Le Capitaine peut ensuite
/// basculer à chaud (ONNX local, Brain distant, arrêt) sans redémarrer.
/// Rend le mode initial, l'option qui le fige (le cas échéant) et le moteur chargé avec son libellé.
pub(super) async fn initial(a: &ServeArgs, db: &Arc<Db>) -> Result<(FaceMode, Option<String>, Option<(astro_facecloud::AnyFaceEngine, String)>), Failure> {
    let auto_models = if a.face_engine.is_none() && a.face_models.is_none() { local_models_found(&a.db) } else { None };
    let (initial_mode, locked) = match (&a.face_engine, &a.face_models) {
        (Some(url), _) => (FaceMode::Remote(url.clone()), Some("--face-engine".to_owned())),
        (None, Some(_)) => (FaceMode::Local, Some("--face-models".to_owned())),
        (None, None) => match FaceControl::stored(db).await {
            Some(mode) => (mode, None),
            None if auto_models.is_some() => {
                println!("FaceCloud : modèles trouvés : inférence locale activée automatiquement");
                (FaceMode::Local, None)
            }
            None => (FaceMode::Off, None),
        },
    };
    let initial_engine: Option<(astro_facecloud::AnyFaceEngine, String)> = match &initial_mode {
        FaceMode::Off => None,
        FaceMode::Remote(url) => Some((astro_facecloud::AnyFaceEngine::Http(Box::new(HttpFaceEngine::new(url, Duration::from_secs(120)))), format!("moteur {url}"))),
        FaceMode::Local => match a.face_models.clone().or(auto_models) {
            Some(dir) => match load_local_engine(&dir, a.face_threads).await {
                Ok(engine) => Some((engine, format!("inférence locale ONNX ({})", dir.display()))),
                Err(e) if locked.is_some() => return Err(e.into()),
                Err(e) => {
                    eprintln!("FaceCloud : {e} : moteur arrêté (réglable dans l'interface du Capitaine)");
                    None
                }
            },
            None => {
                eprintln!("FaceCloud : inférence locale choisie mais modèles introuvables : moteur arrêté");
                None
            }
        },
    };
    Ok((initial_mode, locked, initial_engine))
}
