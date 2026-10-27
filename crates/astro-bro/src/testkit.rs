//! Outils de test partagés.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use astro_db::Db;

use crate::ollama::Summarizer;
use crate::BroError;

/// Résumeur qui compte les lignes qu'on lui donne.
pub struct Echo;

impl Summarizer for Echo {
    async fn summarize(&self, block: &str) -> Result<String, BroError> {
        Ok(format!("{} souvenirs résumés", block.lines().count()))
    }
}

/// Résumeur en panne.
pub struct Broken;

impl Summarizer for Broken {
    async fn summarize(&self, _: &str) -> Result<String, BroError> {
        Err(BroError::Model("Ollama ne répond pas".into()))
    }
}

/// Base temporaire (à supprimer par l'appelant).
pub fn temp_db(tag: &str) -> (Arc<Db>, PathBuf) {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("astro-bro-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    (Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap()), dir)
}
