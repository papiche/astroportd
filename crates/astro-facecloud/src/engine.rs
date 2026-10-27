//! Moteur d'inférence faciale : détection + embeddings 512D normalisés L2.
//!
//! [`FaceEngine`] abstrait l'inférence : un modèle ArcFace local (ONNX Runtime) ou, comme aujourd'hui sur les stations
//! sans GPU, un Brain distant joint par HTTP ([`HttpFaceEngine`]).

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, header};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Dimension des embeddings (InsightFace `buffalo_l`).
pub const DIM: usize = 512;

/// Erreurs d'inférence.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Embedding de mauvaise dimension, nul ou non fini.
    #[error("embedding invalide : {0}")]
    BadEmbedding(&'static str),
    /// Échec de l'inférence ou du transport.
    #[error("inférence impossible : {0}")]
    Backend(String),
}

/// Boîte englobante, dans le repère de l'image d'origine.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct BBox {
    /// Coin supérieur gauche, abscisse.
    pub x1: f32,
    /// Coin supérieur gauche, ordonnée.
    pub y1: f32,
    /// Coin inférieur droit, abscisse.
    pub x2: f32,
    /// Coin inférieur droit, ordonnée.
    pub y2: f32,
}

/// Visage détecté.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DetectedFace {
    /// Boîte englobante.
    pub bbox: BBox,
    /// Confiance de la détection, 0..1.
    pub det_score: f32,
    /// Embedding (normalisé L2 par le pipeline, quelle que soit la sortie du moteur).
    pub embedding: Vec<f32>,
}

/// Inférence faciale.
pub trait FaceEngine: Send + Sync + 'static {
    /// Détecte les visages d'une image encodée (JPEG, PNG…).
    fn detect(&self, image: &[u8]) -> impl Future<Output = Result<Vec<DetectedFace>, EngineError>> + Send;
}

/// Normalise `v` à une norme de 1. Refuse une dimension ≠ 512, un vecteur nul ou non fini.
pub fn l2_normalize(v: &mut [f32]) -> Result<(), EngineError> {
    if v.len() != DIM {
        return Err(EngineError::BadEmbedding("dimension ≠ 512"));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(EngineError::BadEmbedding("valeur non finie"));
    }
    let norm = v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    if norm < 1e-12 {
        return Err(EngineError::BadEmbedding("vecteur nul"));
    }
    for x in v.iter_mut() {
        *x = (f64::from(*x) / norm) as f32;
    }
    Ok(())
}

/// Identifiant déterministe d'un embedding : sha256 de ses composantes arrondies à 6 décimales.
pub fn embedding_hash(v: &[f32]) -> String {
    let text = v.iter().map(|x| format!("{x:.6}")).collect::<Vec<_>>().join(",");
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Nom d'un visage inconnu : `Inconnu_<8 premiers hexadécimaux du hash>`.
pub fn unknown_name(embed_hash: &str) -> String {
    format!("Inconnu_{}", &embed_hash[..8.min(embed_hash.len())])
}

#[derive(Deserialize)]
struct Reply {
    faces: Vec<DetectedFace>,
}

/// Moteur distant : `POST <endpoint>` avec l'image brute, réponse JSON
/// `{"faces":[{"bbox":{"x1","y1","x2","y2"},"det_score":0.99,"embedding":[512 flottants]}]}`.
#[derive(Clone)]
pub struct HttpFaceEngine {
    endpoint: String,
    timeout: Duration,
    client: Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
}

impl HttpFaceEngine {
    /// `endpoint` : URL complète (`http://brain.local:8190/faces`).
    pub fn new(endpoint: &str, timeout: Duration) -> Self {
        Self { endpoint: endpoint.to_owned(), timeout, client: Client::builder(TokioExecutor::new()).build_http() }
    }
}

impl FaceEngine for HttpFaceEngine {
    async fn detect(&self, image: &[u8]) -> Result<Vec<DetectedFace>, EngineError> {
        let req = Request::post(&self.endpoint)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Full::new(Bytes::copy_from_slice(image)))
            .map_err(|e| EngineError::Backend(e.to_string()))?;
        let call = async {
            let resp = self.client.request(req).await.map_err(|e| EngineError::Backend(e.to_string()))?;
            let status = resp.status();
            let body = resp.into_body().collect().await.map_err(|e| EngineError::Backend(e.to_string()))?.to_bytes();
            if !status.is_success() {
                return Err(EngineError::Backend(format!("{status} : {}", String::from_utf8_lossy(&body[..body.len().min(200)]))));
            }
            serde_json::from_slice::<Reply>(&body).map_err(|e| EngineError::Backend(format!("réponse illisible : {e}")))
        };
        let reply = tokio::time::timeout(self.timeout, call).await.map_err(|_| EngineError::Backend("délai dépassé".into()))??;
        Ok(reply.faces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_and_hash() {
        let mut v = vec![0.0f32; DIM];
        v[0] = 3.0;
        v[1] = 4.0;
        l2_normalize(&mut v).unwrap();
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        let h = embedding_hash(&v);
        assert_eq!(h.len(), 64);
        assert_eq!(h, embedding_hash(&v), "déterministe");
        let mut w = v.clone();
        w[0] += 1e-9; // sous la précision de 6 décimales
        assert_eq!(embedding_hash(&w), h);
        w[0] += 1e-3;
        assert_ne!(embedding_hash(&w), h);
        assert_eq!(unknown_name(&h), format!("Inconnu_{}", &h[..8]));
        assert!(l2_normalize(&mut [1.0f32; 10]).is_err());
        assert!(l2_normalize(&mut vec![0.0f32; DIM]).is_err());
        let mut bad = vec![1.0f32; DIM];
        bad[3] = f32::NAN;
        assert!(l2_normalize(&mut bad).is_err());
    }
}
