//! Réglages du Capitaine, changeables à chaud et conservés dans la base (`kv`, portée `options`).
//!
//! - **`face`** : moteur d'inférence faciale — ONNX local, Brain GPU distant ou arrêt — sans redémarrer le démon. Une
//!   option de ligne de commande (`--face-engine`, `--face-models`) verrouille ce réglage : l'interface le dit.
//! - **`kinds`** : matrice du relai par kind (lecture, écriture, projection en base). Couches, de la plus faible à la plus
//!   forte : défauts du relai, lignes `kind.<n> = read=… write=… project=…` de `station.conf`, réglages du Capitaine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_db::Db;
use astro_facecloud::{AnyFaceEngine, HttpFaceEngine, SwitchableEngine};
use astro_relay::{KindRule, Policy, Relay};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::ui_backend::AdminFn;

const SCOPE: &str = "options";

async fn kv_get(db: &Db, key: &'static str) -> Option<Value> {
    let raw: Option<String> = db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, key], |r| r.get(0)).optional()?)).await.ok().flatten();
    raw.and_then(|t| serde_json::from_str(&t).ok())
}

async fn kv_put(db: &Db, key: &'static str, value: &Value) -> Result<(), String> {
    let (value, at) = (value.to_string(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64));
    db.writer()
        .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![SCOPE.to_owned().into(), key.to_owned().into(), value.into(), at.into()])
        .await
        .map(|_| ())
        .map_err(|e| format!("réglage non enregistré : {e}"))
}

// ───────────────────────────── Moteur FaceID ─────────────────────────────

/// Mode du moteur d'inférence faciale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaceMode {
    /// Arrêté : les photos attendent.
    Off,
    /// ONNX Runtime dans le processus.
    Local,
    /// Brain GPU (service HTTP) à cette adresse.
    Remote(String),
}

impl FaceMode {
    fn to_json(&self) -> Value {
        match self {
            Self::Off => json!({ "mode": "off" }),
            Self::Local => json!({ "mode": "local" }),
            Self::Remote(url) => json!({ "mode": "remote", "url": url }),
        }
    }

    fn from_json(v: &Value) -> Result<Self, String> {
        match v["mode"].as_str() {
            Some("off") => Ok(Self::Off),
            Some("local") => Ok(Self::Local),
            Some("remote") => {
                let url = v["url"].as_str().map(str::trim).unwrap_or_default();
                if !url.starts_with("http://") || url.len() < 12 || url.len() > 256 || url.chars().any(char::is_whitespace) {
                    return Err("adresse du Brain : http://hôte:port/chemin attendu (HTTPS n'est pas pris en charge par le client d'inférence)".into());
                }
                Ok(Self::Remote(url.to_owned()))
            }
            _ => Err("mode attendu : off, local ou remote".into()),
        }
    }
}

/// Pilote le moteur FaceID : état, bascule à chaud, persistance.
pub struct FaceControl {
    db: Arc<Db>,
    engine: Arc<SwitchableEngine>,
    threads: usize,
    data_dir: PathBuf,
    locked: Option<String>,
    mode: Mutex<FaceMode>,
}

#[cfg(feature = "onnx")]
fn find_models(data: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    astro_facecloud::onnx::find_models(Some(data), home.as_deref())
}

#[cfg(not(feature = "onnx"))]
fn find_models(_: &Path) -> Option<PathBuf> {
    None
}

/// Modèles d'inférence locale aux emplacements habituels (à côté de la base de la station, ou `~/.insightface`).
pub fn local_models_found(db_path: &Path) -> Option<PathBuf> {
    db_path.parent().and_then(find_models)
}

#[cfg(feature = "onnx")]
fn library_found() -> Option<PathBuf> {
    astro_facecloud::onnx::find_library(std::env::var("ORT_DYLIB_PATH").ok().as_deref(), std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

#[cfg(not(feature = "onnx"))]
fn library_found() -> Option<PathBuf> {
    None
}

/// Charge SCRFD + ArcFace (hors de la boucle asynchrone : quelques secondes de lecture et d'optimisation).
#[cfg(feature = "onnx")]
pub async fn load_local_engine(dir: &Path, threads: usize) -> Result<AnyFaceEngine, String> {
    let dir = dir.to_owned();
    let engine = tokio::task::spawn_blocking(move || astro_facecloud::onnx::OrtFaceEngine::load(&dir, threads))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("moteur FaceID local : {e} (modèles attendus : det_10g.onnx et w600k_r50.onnx ; ORT_DYLIB_PATH = libonnxruntime.so)"))?;
    Ok(AnyFaceEngine::Local(engine))
}

/// Sans la fonction `onnx`, l'inférence locale n'existe pas.
#[cfg(not(feature = "onnx"))]
pub async fn load_local_engine(_: &Path, _: usize) -> Result<AnyFaceEngine, String> {
    Err("ce binaire est compilé sans la fonction `onnx` (cargo build --release --features onnx)".into())
}

impl FaceControl {
    /// `locked` : option de ligne de commande qui impose le moteur (le réglage de l'interface est alors figé).
    pub fn new(db: Arc<Db>, engine: Arc<SwitchableEngine>, threads: usize, data_dir: PathBuf, locked: Option<String>, initial: FaceMode) -> Self {
        Self { db, engine, threads, data_dir, locked, mode: Mutex::new(initial) }
    }

    /// Choix enregistré par le Capitaine (sans option de ligne de commande, il l'emporte sur la détection automatique).
    pub async fn stored(db: &Db) -> Option<FaceMode> {
        FaceMode::from_json(&kv_get(db, "face_engine").await?).ok()
    }

    /// Construit le moteur d'un mode ; une erreur laisse l'état courant intact.
    pub async fn build(&self, mode: &FaceMode) -> Result<Option<(AnyFaceEngine, String)>, String> {
        match mode {
            FaceMode::Off => Ok(None),
            FaceMode::Remote(url) => {
                let authority = url.trim_start_matches("http://").split('/').next().unwrap_or_default();
                let target = if authority.contains(':') { authority.to_owned() } else { format!("{authority}:80") };
                match tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect(&target)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => return Err(format!("Brain injoignable ({target}) : {e}")),
                    Err(_) => return Err(format!("Brain injoignable ({target}) : délai de 4 s dépassé")),
                }
                Ok(Some((AnyFaceEngine::Http(Box::new(HttpFaceEngine::new(url, Duration::from_secs(120)))), format!("moteur {url}"))))
            }
            FaceMode::Local => {
                let dir = find_models(&self.data_dir).ok_or_else(|| "modèles introuvables (det_10g.onnx et w600k_r50.onnx dans ~/.zen/astroportd/models ou ~/.insightface/models/buffalo_l)".to_owned())?;
                let engine = load_local_engine(&dir, self.threads).await?;
                Ok(Some((engine, format!("inférence locale ONNX ({})", dir.display()))))
            }
        }
    }

    /// État pour l'interface.
    pub async fn status(&self) -> Value {
        let mode = self.mode.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let (pending, failed): (i64, i64) = self
            .db
            .readers()
            .read(|c| Ok(c.query_row("SELECT COALESCE(SUM(status = 'pending'), 0), COALESCE(SUM(status = 'failed'), 0) FROM tasks WHERE kind = 'facecloud'", [], |r| Ok((r.get(0)?, r.get(1)?)))?))
            .await
            .unwrap_or((0, 0));
        let mut v = mode.to_json();
        v["label"] = json!(self.engine.label());
        v["locked"] = json!(self.locked);
        v["onnx_compiled"] = json!(cfg!(feature = "onnx"));
        v["models"] = json!(find_models(&self.data_dir).map(|p| p.display().to_string()));
        v["library"] = json!(library_found().map(|p| p.display().to_string()));
        v["pending"] = json!(pending);
        v["failed"] = json!(failed);
        v
    }

    /// Bascule à chaud : `{ "mode": "off" | "local" | "remote", "url": … }`.
    pub async fn set(&self, body: &Value) -> Result<Value, String> {
        if let Some(flag) = &self.locked {
            return Err(format!("moteur imposé par {flag} sur la ligne de commande : retirez l'option pour le régler ici"));
        }
        let mode = FaceMode::from_json(body)?;
        let built = self.build(&mode).await?;
        let (engine, label) = built.map_or((None, String::new()), |(e, l)| (Some(e), l));
        self.engine.set(engine, label);
        kv_put(&self.db, "face_engine", &mode.to_json()).await?;
        *self.mode.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
        Ok(self.status().await)
    }
}

// ───────────────────────────── Matrice des kinds ─────────────────────────────

/// Kinds que l'interface propose toujours, avec leur libellé.
pub const KIND_LABELS: &[(u32, &str)] = &[
    (0, "Profil"),
    (1, "Note publique"),
    (3, "Contacts (Proches)"),
    (4, "Message privé (ancien)"),
    (5, "Suppression"),
    (7, "Réaction (+N = Ẑen)"),
    (21, "Vidéo"),
    (22, "Vidéo courte"),
    (1059, "Message privé (gift wrap)"),
    (10_002, "Liste de relais (NIP-65)"),
    (24_315, "Visio : présence"),
    (24_316, "Visio : sonnerie"),
    (30_023, "Article"),
    (30_078, "Données d'application"),
    (30_800, "DID / configuration coopérative"),
    (30_850, "Santé économique"),
    (30_851, "Émission Ẑen"),
    (30_852, "Ledger Ğ1-N²"),
    (31_903, "Paramètres d'application"),
];

fn label_of(kind: u32) -> &'static str {
    KIND_LABELS.iter().find(|(k, _)| *k == kind).map_or("", |(_, l)| l)
}

/// Règles `kind.<n> = …` d'un `station.conf`.
pub fn conf_rules(path: &Path) -> BTreeMap<u32, String> {
    crate::init::read_conf(path).into_iter().filter_map(|(k, v)| Some((k.strip_prefix("kind.")?.parse().ok()?, v))).collect()
}

/// Politique = défauts, puis `station.conf`, puis réglages du Capitaine.
pub fn build_policy(conf: &BTreeMap<u32, String>, captain: &BTreeMap<u32, String>) -> Result<Policy, String> {
    let mut p = Policy::default();
    for (layer, rules) in [("station.conf", conf), ("réglage du Capitaine", captain)] {
        for (kind, spec) in rules {
            let rule = KindRule::parse(spec, p.rule(*kind)).map_err(|e| format!("kind {kind} ({layer}) : {e}"))?;
            p = p.with_rules([(*kind, rule)]).map_err(|e| format!("kind {kind} ({layer}) : {e}"))?;
        }
    }
    Ok(p)
}

/// Pilote la matrice du relai.
pub struct KindsControl {
    db: Arc<Db>,
    conf: BTreeMap<u32, String>,
    relay: Arc<Relay>,
}

async fn captain_rules(db: &Db) -> BTreeMap<u32, String> {
    kv_get(db, "kind_rules").await.and_then(|v| v.as_object().cloned()).map(|m| m.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_owned()))).collect()).unwrap_or_default()
}

impl KindsControl {
    /// Politique de départ (à donner à `RelayConfig`). Une règle invalide est signalée et la politique par défaut s'applique.
    pub async fn initial_policy(db: &Db, conf_path: &Path) -> Policy {
        match build_policy(&conf_rules(conf_path), &captain_rules(db).await) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("relai : matrice des kinds invalide ({e}) : politique par défaut appliquée");
                Policy::default()
            }
        }
    }

    /// Branche le pilote sur le relai en marche.
    pub fn new(db: Arc<Db>, conf_path: &Path, relay: Arc<Relay>) -> Self {
        Self { db, conf: conf_rules(conf_path), relay }
    }

    /// Vue pour l'interface.
    pub async fn view(&self) -> Value {
        let captain = captain_rules(&self.db).await;
        let policy = self.relay.policy();
        let defaults = build_policy(&self.conf, &BTreeMap::new()).unwrap_or_default();
        let mut kinds: Vec<u32> = KIND_LABELS.iter().map(|(k, _)| *k).chain(self.conf.keys().copied()).chain(captain.keys().copied()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        let rows: Vec<Value> = kinds
            .into_iter()
            .map(|k| {
                let r = policy.rule(k);
                let source = if captain.contains_key(&k) { "capitaine" } else if self.conf.contains_key(&k) { "station.conf" } else { "défaut" };
                json!({ "kind": k, "label": label_of(k), "read": r.read.name(), "write": r.write.name(), "project": r.project, "default": defaults.rule(k).spec(), "source": source })
            })
            .collect();
        json!({
            "kinds": rows,
            "choices": { "read": ["public", "addressed", "friends", "circle", "deny"], "write": ["circle", "open", "deny"] },
            "fixed": { "1059": "écriture : gift wrap vers un compte hébergé ici (amorçage) ; lecture : destinataire seul ou fermé", "22242": "jamais stocké (authentification NIP-42)" },
        })
    }

    /// `{ "rules": { "<kind>": "read=… write=… project=…" }, "reset": [<kind>…] }` ; applique à chaud.
    pub async fn set(&self, body: &Value) -> Result<Value, String> {
        let mut captain = captain_rules(&self.db).await;
        let lower = build_policy(&self.conf, &BTreeMap::new())?;
        for (k, spec) in body["rules"].as_object().into_iter().flatten() {
            let kind: u32 = k.parse().map_err(|_| format!("« {k} » n'est pas un numéro de kind"))?;
            let spec = spec.as_str().ok_or("règle attendue sous forme de texte (read=… write=… project=…)")?;
            let rule = KindRule::parse(spec, lower.rule(kind)).map_err(|e| format!("kind {kind} : {e}"))?;
            if rule == lower.rule(kind) {
                captain.remove(&kind); // identique à la couche inférieure : pas de réglage à conserver
            } else {
                captain.insert(kind, rule.spec());
            }
        }
        for k in body["reset"].as_array().into_iter().flatten().filter_map(Value::as_u64) {
            captain.remove(&(k as u32));
        }
        if captain.len() > 200 {
            return Err("trop de règles (200 au plus)".into());
        }
        let policy = build_policy(&self.conf, &captain)?; // valide AVANT d'enregistrer ou d'appliquer
        let stored: serde_json::Map<String, Value> = captain.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        kv_put(&self.db, "kind_rules", &Value::Object(stored)).await?;
        self.relay.set_policy(policy);
        Ok(self.view().await)
    }
}

// ───────────────────────────── Branchement ─────────────────────────────

/// Les réglages d'administration de la station.
pub struct Admin {
    /// Moteur FaceID.
    pub face: Arc<FaceControl>,
    /// Matrice des kinds.
    pub kinds: Arc<KindsControl>,
}

impl Admin {
    /// Fonction à donner à `StationUi::with_admin`.
    pub fn handler(self: Arc<Self>) -> AdminFn {
        Arc::new(move |section, body| {
            let me = self.clone();
            Box::pin(async move {
                match (section.as_str(), body) {
                    ("face", None) => Ok(me.face.status().await),
                    ("face", Some(b)) => me.face.set(&b).await,
                    ("kinds", None) => Ok(me.kinds.view().await),
                    ("kinds", Some(b)) => me.kinds.set(&b).await,
                    _ => Err(format!("section inconnue : {section}")),
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(items: &[(u32, &str)]) -> BTreeMap<u32, String> {
        items.iter().map(|(k, v)| (*k, (*v).to_owned())).collect()
    }

    #[test]
    fn layers_apply_in_order_and_the_captain_wins() {
        let p = build_policy(&map(&[(7, "write=deny"), (1, "read=deny")]), &map(&[(1, "read=public"), (30_078, "write=open")])).unwrap();
        assert_eq!(p.rule(7).write.name(), "deny", "station.conf");
        assert_eq!(p.rule(1).read.name(), "public", "le Capitaine l'emporte sur station.conf");
        assert_eq!(p.rule(30_078).write.name(), "open");
        assert_eq!(p.rule(30_078).read.name(), "circle", "les clés omises gardent la valeur inférieure");
    }

    #[test]
    fn an_invalid_layer_names_its_origin() {
        let e = build_policy(&map(&[(7, "write=tous")]), &BTreeMap::new()).unwrap_err();
        assert!(e.contains("kind 7") && e.contains("station.conf"), "{e}");
        assert!(build_policy(&BTreeMap::new(), &map(&[(22_242, "read=public")])).is_err());
    }

    #[test]
    fn face_modes_validate_their_input() {
        assert_eq!(FaceMode::from_json(&json!({ "mode": "off" })), Ok(FaceMode::Off));
        assert_eq!(FaceMode::from_json(&json!({ "mode": "remote", "url": "http://brain.local:8190/faces" })), Ok(FaceMode::Remote("http://brain.local:8190/faces".into())));
        for bad in [json!({}), json!({ "mode": "gpu" }), json!({ "mode": "remote" }), json!({ "mode": "remote", "url": "https://x.example/faces" }), json!({ "mode": "remote", "url": "http://a b" })] {
            assert!(FaceMode::from_json(&bad).is_err(), "{bad}");
        }
    }
}
