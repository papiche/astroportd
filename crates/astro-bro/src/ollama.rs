//! Modèles de BRO : embeddings (`bge-m3`, 1024 dimensions) et résumés RÊVE, par l'API HTTP d'Ollama.
//!
//! Ollama est souvent sur une autre machine (le Brain GPU, joint par tunnel P2P) : toute requête a un délai, et
//! tout échec remonte (`BroError::Model`) — jamais un vecteur vide qui passerait pour un résultat normal.

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, header};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::BroError;

/// Dimension des embeddings de la mémoire (`bge-m3`).
pub const EMBED_DIM: usize = 1024;
/// Nombre maximal de caractères envoyés à un embedding.
const MAX_INPUT_CHARS: usize = 12_000;

/// Transforme un texte en vecteur de [`EMBED_DIM`] dimensions.
pub trait Embedder: Send + Sync + 'static {
    /// Vecteur de `text`.
    fn embed(&self, text: &str) -> impl Future<Output = Result<Vec<f32>, BroError>> + Send;
}

/// Résume un bloc de souvenirs (cycle RÊVE).
pub trait Summarizer: Send + Sync + 'static {
    /// Résumé dense de `block`.
    fn summarize(&self, block: &str) -> impl Future<Output = Result<String, BroError>> + Send;
}

/// Client Ollama.
pub struct OllamaClient {
    base: String,
    embed_model: String,
    chat_model: String,
    timeout: Duration,
    http: Client<HttpConnector, Full<Bytes>>,
}

impl OllamaClient {
    /// `base` : `http://127.0.0.1:11434`. Modèles du legacy : `bge-m3` et `llama3.2`.
    pub fn new(base: &str, embed_model: &str, chat_model: &str, timeout: Duration) -> Self {
        Self { base: base.trim_end_matches('/').to_owned(), embed_model: embed_model.to_owned(), chat_model: chat_model.to_owned(), timeout, http: Client::builder(TokioExecutor::new()).build_http() }
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, BroError> {
        let model = |e: &dyn std::fmt::Display| BroError::Model(e.to_string());
        let req = Request::post(format!("{}{path}", self.base)).header(header::CONTENT_TYPE, "application/json").body(Full::new(Bytes::from(body.to_string()))).map_err(|e| model(&e))?;
        let call = async {
            let resp = self.http.request(req).await.map_err(|e| model(&e))?;
            let status = resp.status();
            let bytes = resp.into_body().collect().await.map_err(|e| model(&e))?.to_bytes();
            if !status.is_success() {
                return Err(BroError::Model(format!("Ollama {status} : {}", String::from_utf8_lossy(&bytes[..bytes.len().min(200)]))));
            }
            serde_json::from_slice::<Value>(&bytes).map_err(|e| BroError::Model(format!("réponse illisible : {e}")))
        };
        tokio::time::timeout(self.timeout, call).await.map_err(|_| BroError::Model(format!("Ollama ne répond pas ({path}, délai {:?})", self.timeout)))?
    }
}

impl OllamaClient {
    /// Ollama répond-il (liste des modèles, 2 s au plus) ?
    pub async fn ping(&self) -> Result<(), BroError> {
        let model = |e: &dyn std::fmt::Display| BroError::Model(e.to_string());
        let req = Request::get(format!("{}/api/tags", self.base)).body(Full::new(Bytes::new())).map_err(|e| model(&e))?;
        let call = async {
            let resp = self.http.request(req).await.map_err(|e| model(&e))?;
            if resp.status().is_success() { Ok(()) } else { Err(BroError::Model(format!("Ollama {}", resp.status()))) }
        };
        tokio::time::timeout(Duration::from_secs(2), call).await.map_err(|_| BroError::Model("Ollama ne répond pas (2 s)".into()))?
    }
}

impl Embedder for OllamaClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, BroError> {
        let input: String = text.chars().take(MAX_INPUT_CHARS).collect();
        let reply = self.post("/api/embed", &json!({ "model": self.embed_model, "input": input })).await?;
        let vec: Vec<f32> = reply
            .get("embeddings")
            .and_then(|e| e.get(0))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default();
        if vec.is_empty() {
            return Err(BroError::Model(format!("embedding vide renvoyé par {}", self.embed_model)));
        }
        if vec.len() != EMBED_DIM {
            return Err(BroError::Model(format!("{} renvoie {} dimensions, {EMBED_DIM} attendues : ré-indexer, ne rien mélanger", self.embed_model, vec.len())));
        }
        Ok(vec)
    }
}

impl Summarizer for OllamaClient {
    async fn summarize(&self, block: &str) -> Result<String, BroError> {
        let prompt = format!(
            "Résume les échanges suivants en 2-3 phrases denses (en français). Garde les faits, décisions et compétences clés. \
             Ignore les redondances.\n\n{block}\n\n--- Résumé :"
        );
        let body = json!({ "model": self.chat_model, "prompt": prompt, "stream": false, "options": { "num_predict": 200, "temperature": 0.3 } });
        let reply = self.post("/api/generate", &body).await?;
        Ok(reply.get("response").and_then(Value::as_str).unwrap_or("").trim().to_owned())
    }
}

/// Embedding déterministe par hachage de mots (sac de mots signé, normalisé L2).
///
/// **Pas un modèle sémantique** : deux textes qui partagent des mots sont proches, rien de plus. Sert aux tests et à
/// un fonctionnement dégradé *explicitement choisi* (jamais un repli silencieux quand Ollama tombe).
#[derive(Debug, Clone, Copy, Default)]
pub struct HashingEmbedder;

impl Embedder for HashingEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, BroError> {
        let mut v = vec![0.0f32; EMBED_DIM];
        let mut any = false;
        for word in text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() > 1) {
            let h = Sha256::digest(word.as_bytes());
            let idx = usize::from(u16::from_le_bytes([h[0], h[1]])) % EMBED_DIM;
            v[idx] += if h[2] & 1 == 0 { 1.0 } else { -1.0 };
            any = true;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if !any || norm < 1e-6 {
            return Err(BroError::Model("aucun mot exploitable à plonger".into()));
        }
        v.iter_mut().for_each(|x| *x /= norm);
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[tokio::test]
    async fn hashing_embedder_is_deterministic_normalised_and_word_sensitive() {
        let e = HashingEmbedder;
        let a = e.embed("pain au levain du dimanche").await.unwrap();
        assert_eq!(a.len(), EMBED_DIM);
        assert_eq!(a, e.embed("pain au levain du dimanche").await.unwrap());
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-5);
        let near = e.embed("le pain au levain").await.unwrap();
        let far = e.embed("courir un marathon en avril").await.unwrap();
        assert!(cosine(&a, &near) > cosine(&a, &far) + 0.3);
        assert!(e.embed("  ! ").await.is_err());
    }
}
