//! Mémoire sémantique de BRO : un souvenir = une ligne `memory` + un vecteur 1024D dans `memory_vectors`,
//! écrits **dans la même transaction** (jamais un souvenir sans vecteur, ni l'inverse).
//!
//! Identifiant stable : `sha256(propriétaire ‖ empreinte du contenu)` — écrire deux fois la même chose ne crée
//! qu'un souvenir. Une suppression laisse une pierre tombale (`status = deleted`, texte effacé, vecteur conservé)
//! qui bloque la réapparition du même contenu.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use astro_core::ActivityBus;
use astro_db::{Db, VectorTable};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter};
use sha2::{Digest, Sha256};

use crate::guard::unstorable_reason;
use crate::ollama::{Embedder, Summarizer};
use crate::reve::{DreamReport, REVE_THRESHOLD};
use crate::BroError;

/// Seuil de rappel (cosinus) mesuré pour `bge-m3` : bons souvenirs ≥ 0,56, hors-sujet ≤ 0,55.
pub const RECALL_THRESHOLD: f64 = 0.50;
/// Version du schéma des souvenirs.
const SCHEMA_V: i64 = 2;
/// Modèle d'embedding des souvenirs.
const EMBED_MODEL: &str = "bge-m3";

/// Les quatre emplacements qui subsistent du legacy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// 0 : identité et profil.
    Identity,
    /// 13 : préférences.
    Preferences,
    /// 14 : objectifs.
    Goals,
    /// 15 : journal.
    Journal,
}

impl Slot {
    /// Tous les emplacements.
    pub const ALL: [Slot; 4] = [Slot::Identity, Slot::Preferences, Slot::Goals, Slot::Journal];

    /// Numéro stocké.
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Identity => 0,
            Self::Preferences => 13,
            Self::Goals => 14,
            Self::Journal => 15,
        }
    }

    /// Emplacement d'un numéro stocké.
    pub fn from_i64(n: i64) -> Result<Self, BroError> {
        Self::ALL.into_iter().find(|s| s.as_i64() == n).ok_or(BroError::BadSlot(n))
    }
}

/// Nature d'un souvenir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    /// Fait stable.
    #[default]
    Fact,
    /// Épisode daté.
    Episode,
    /// Trait de caractère.
    Trait,
    /// Personne.
    Person,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Episode => "episode",
            Self::Trait => "trait",
            Self::Person => "person",
        }
    }
}

/// Qui peut relire le souvenir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Le titulaire seul.
    #[default]
    Private,
    /// Ses amis de cœur.
    Love,
    /// Public.
    Public,
}

impl Visibility {
    fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Love => "love",
            Self::Public => "public",
        }
    }
}

/// Métadonnées d'un souvenir à écrire.
#[derive(Debug, Clone)]
pub struct Meta {
    /// Nature.
    pub kind: Kind,
    /// Canal d'origine (`chat`, `bus`, `reve`, `manual`…).
    pub source: String,
    /// Visibilité.
    pub visibility: Visibility,
    /// Importance (le RÊVE y met le nombre de souvenirs condensés).
    pub importance: f64,
    /// Confiance de l'extraction, 0..1.
    pub confidence: f64,
    /// `Some("user")` : écrit ou corrigé par la personne, jamais condensé par le RÊVE.
    pub origin: Option<String>,
    /// Horodatage ; maintenant par défaut.
    pub ts: Option<u64>,
}

impl Default for Meta {
    fn default() -> Self {
        Self { kind: Kind::Fact, source: "chat".into(), visibility: Visibility::Private, importance: 1.0, confidence: 1.0, origin: None, ts: None }
    }
}

/// Un souvenir lu.
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    /// Clé interne.
    pub mem_id: i64,
    /// Texte.
    pub content: String,
    /// Horodatage.
    pub ts: u64,
    /// Emplacement, si connu.
    pub slot: Option<Slot>,
    /// Canal d'origine.
    pub source: Option<String>,
}

/// Un souvenir retrouvé par similarité.
#[derive(Debug, Clone, PartialEq)]
pub struct Recalled {
    /// Le souvenir.
    pub memory: Memory,
    /// Similarité cosinus (1 = identique).
    pub score: f64,
}

/// Résultat d'une écriture.
#[derive(Debug)]
pub struct Remembered {
    /// Clé du souvenir créé ; `None` si le contenu existait déjà.
    pub mem_id: Option<i64>,
    /// Compression RÊVE déclenchée par cette écriture.
    pub dream: Option<DreamReport>,
    /// Échec de la compression, visible (le souvenir est bien écrit).
    pub dream_error: Option<String>,
}

/// Mémoire de BRO.
pub struct MemoryStore<E, S> {
    pub(crate) db: Arc<Db>,
    pub(crate) embedder: Arc<E>,
    pub(crate) summarizer: Arc<S>,
    pub(crate) activity: Option<ActivityBus>,
    /// Compressions en cours `(propriétaire, emplacement)` : une seule à la fois par couple.
    pub(crate) dreaming: Mutex<HashSet<(String, i64)>>,
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Empreinte du contenu : espaces normalisés, minuscules, sha256 tronqué à 16 hex (comme le legacy).
pub fn content_hash(content: &str) -> String {
    let normalised = content.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    hex::encode(&Sha256::digest(normalised.as_bytes())[..8])
}

/// Identifiant stable d'un souvenir.
pub fn point_id(owner: &str, content_hash: &str) -> String {
    hex::encode(&Sha256::digest(format!("{owner}:{content_hash}").as_bytes())[..16])
}

/// Saison (hémisphère nord) d'un horodatage, pour filtrer une habitude hivernale d'une habitude estivale.
pub fn season(ts: u64) -> &'static str {
    let date = astro_core::week::date_string(ts);
    match date.get(5..7).and_then(|m| m.parse::<u32>().ok()) {
        Some(12 | 1 | 2) => "hiver",
        Some(3..=5) => "printemps",
        Some(6..=8) => "été",
        Some(9..=11) => "automne",
        _ => "",
    }
}

impl<E: Embedder, S: Summarizer> MemoryStore<E, S> {
    /// Mémoire sur `db`.
    pub fn new(db: Arc<Db>, embedder: Arc<E>, summarizer: Arc<S>, activity: Option<ActivityBus>) -> Self {
        Self { db, embedder, summarizer, activity, dreaming: Mutex::new(HashSet::new()) }
    }

    pub(crate) fn announce(&self, source: &str, message: String) {
        if let Some(bus) = &self.activity {
            bus.publish("bro", source, message, None);
        }
    }

    /// Mémorise `content` pour `owner`. Refuse les secrets. Déclenche le cycle RÊVE si l'emplacement atteint le seuil.
    pub async fn remember(&self, owner: &str, slot: Slot, content: &str, meta: Meta) -> Result<Remembered, BroError> {
        let content = content.trim();
        if content.is_empty() {
            return Err(BroError::Empty);
        }
        if let Some(why) = unstorable_reason(content) {
            self.announce("bro:memory", format!("souvenir refusé : {why}"));
            return Err(BroError::Unstorable(why));
        }
        let mem_id = self.insert(owner, Some(slot), content, &meta).await?;
        if mem_id.is_some() {
            self.announce("bro:memory", format!("souvenir ajouté (emplacement {})", slot.as_i64()));
        }
        let (mut dream, mut dream_error) = (None, None);
        if mem_id.is_some() && self.count_active(owner, slot).await? >= REVE_THRESHOLD {
            match self.dream(owner, slot).await {
                Ok(report) => dream = report,
                Err(e) => {
                    self.announce("bro:reve", format!("compression RÊVE impossible : {e}"));
                    dream_error = Some(e.to_string());
                }
            }
        }
        Ok(Remembered { mem_id, dream, dream_error })
    }

    /// Écrit la ligne et son vecteur dans une transaction. `None` = déjà présent (y compris en pierre tombale).
    pub(crate) async fn insert(&self, owner: &str, slot: Option<Slot>, content: &str, meta: &Meta) -> Result<Option<i64>, BroError> {
        let embedding = self.embedder.embed(content).await?;
        let blob = VectorTable::Memory.encode(&embedding)?;
        let hash = content_hash(content);
        let ts = meta.ts.unwrap_or_else(now_secs);
        let row = (owner.to_owned(), content.to_owned(), meta.clone(), slot.map(Slot::as_i64), point_id(owner, &hash), hash);
        let id = self
            .db
            .writer()
            .transaction(move |tx| {
                let (owner, content, meta, slot, pid, hash) = row;
                let n = tx.execute(
                    "INSERT INTO memory(point_id, owner_hex, content, content_hash, ts, season, importance, kind, source, visibility, confidence, status, embed_model, schema_v, origin, slot) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'active', ?12, ?13, ?14, ?15) ON CONFLICT(point_id) DO NOTHING",
                    params![pid, owner, content, hash, ts as i64, season(ts), meta.importance, meta.kind.as_str(), meta.source, meta.visibility.as_str(), meta.confidence, EMBED_MODEL, SCHEMA_V, meta.origin, slot],
                )?;
                if n == 0 {
                    return Ok(None);
                }
                let id = tx.last_insert_rowid();
                tx.execute("INSERT INTO memory_vectors(mem_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![id, owner, blob])?;
                Ok(Some(id))
            })
            .await?;
        Ok(id)
    }

    /// Nombre de souvenirs actifs de `owner` dans `slot`.
    pub async fn count_active(&self, owner: &str, slot: Slot) -> Result<usize, BroError> {
        let (owner, slot) = (owner.to_owned(), slot.as_i64());
        let n = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM memory WHERE owner_hex = ?1 AND slot = ?2 AND status = 'active'", params![owner, slot], |r| r.get::<_, i64>(0))?))
            .await?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Souvenirs actifs, du plus récent au plus ancien.
    pub async fn list(&self, owner: &str, slot: Option<Slot>, limit: usize) -> Result<Vec<Memory>, BroError> {
        let (owner, slot, limit) = (owner.to_owned(), slot.map(Slot::as_i64), i64::try_from(limit).unwrap_or(i64::MAX));
        Ok(self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare_cached(
                    "SELECT mem_id, content, ts, slot, source FROM memory WHERE owner_hex = ?1 AND status = 'active' AND (?2 IS NULL OR slot = ?2) ORDER BY ts DESC, mem_id DESC LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![owner, slot, limit], row_memory)?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?)
    }

    /// Souvenirs les plus proches de `query` (au moins [`RECALL_THRESHOLD`]), du plus pertinent au moins pertinent.
    pub async fn recall(&self, owner: &str, query: &str, k: usize, slot: Option<Slot>) -> Result<Vec<Recalled>, BroError> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let q = self.embedder.embed(query).await?;
        // Marge : les voisins d'un autre emplacement ou supprimés sont écartés après coup.
        let hits = self.db.readers().knn(VectorTable::Memory, owner, &q, k.saturating_mul(4).max(8)).await?;
        if hits.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = hits.iter().map(|h| h.0).collect();
        let slot = slot.map(Slot::as_i64);
        let rows = self
            .db
            .readers()
            .read(move |c| {
                let marks = vec!["?"; ids.len()].join(",");
                let sql = format!("SELECT mem_id, content, ts, slot, source FROM memory WHERE mem_id IN ({marks}) AND status = 'active' AND (? IS NULL OR slot = ?)");
                let mut args: Vec<Value> = ids.iter().map(|i| Value::Integer(*i)).collect();
                let s = slot.map_or(Value::Null, Value::Integer);
                args.extend([s.clone(), s]);
                let mut stmt = c.prepare(&sql)?;
                let found = stmt.query_map(params_from_iter(args.iter()), row_memory)?.collect::<Result<Vec<_>, _>>()?;
                Ok(found)
            })
            .await?;
        let mut out: Vec<Recalled> = hits
            .iter()
            .filter_map(|(id, dist)| rows.iter().find(|m| m.mem_id == *id).map(|m| Recalled { memory: m.clone(), score: 1.0 - dist }))
            .filter(|r| r.score >= RECALL_THRESHOLD)
            .collect();
        out.truncate(k);
        Ok(out)
    }

    /// Oublie un souvenir : le texte est effacé, une pierre tombale (vecteur conservé) bloque son retour.
    pub async fn forget(&self, owner: &str, mem_id: i64) -> Result<bool, BroError> {
        let owner = owner.to_owned();
        let n = self.db.writer().execute("UPDATE memory SET content = '', status = 'deleted' WHERE mem_id = ?1 AND owner_hex = ?2 AND status = 'active'", vec![Value::Integer(mem_id), Value::Text(owner)]).await?;
        Ok(n.changes > 0)
    }
}

fn row_memory(r: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        mem_id: r.get(0)?,
        content: r.get(1)?,
        ts: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
        slot: r.get::<_, Option<i64>>(3)?.and_then(|n| Slot::from_i64(n).ok()),
        source: r.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ollama::HashingEmbedder;
    use crate::testkit::{Echo, temp_db};

    const ALICE: &str = "aa";

    #[tokio::test]
    async fn remembering_twice_stores_once_and_recall_finds_the_right_memory() {
        let (db, dir) = temp_db("mem");
        let m = MemoryStore::new(db, Arc::new(HashingEmbedder), Arc::new(Echo), None);
        let first = m.remember(ALICE, Slot::Preferences, "J'aime le pain au levain du dimanche", Meta::default()).await.unwrap();
        assert!(first.mem_id.is_some());
        let again = m.remember(ALICE, Slot::Preferences, "  j'aime le PAIN au levain   du dimanche ", Meta::default()).await.unwrap();
        assert_eq!(again.mem_id, None, "même contenu, même souvenir");
        m.remember(ALICE, Slot::Goals, "Courir un semi-marathon en avril", Meta::default()).await.unwrap();
        m.remember("bb", Slot::Goals, "Le pain au levain de Bob", Meta::default()).await.unwrap();

        let hits = m.recall(ALICE, "quel pain au levain", 3, None).await.unwrap();
        assert_eq!(hits.first().map(|h| h.memory.content.as_str()), Some("J'aime le pain au levain du dimanche"));
        assert!(hits.iter().all(|h| !h.memory.content.contains("Bob")), "jamais les souvenirs d'un autre compte");
        let goals = m.recall(ALICE, "pain au levain", 3, Some(Slot::Goals)).await.unwrap();
        assert!(goals.iter().all(|h| h.memory.slot == Some(Slot::Goals)));
        assert_eq!(m.count_active(ALICE, Slot::Preferences).await.unwrap(), 1);
        drop(m);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn secrets_are_refused_and_forgetting_leaves_a_tombstone() {
        let (db, dir) = temp_db("forget");
        let m = MemoryStore::new(db, Arc::new(HashingEmbedder), Arc::new(Echo), None);
        let err = m.remember(ALICE, Slot::Journal, "mon mot de passe est hunter2024", Meta::default()).await.unwrap_err();
        assert!(matches!(err, BroError::Unstorable(_)));
        assert!(matches!(m.remember(ALICE, Slot::Journal, "   ", Meta::default()).await, Err(BroError::Empty)));
        let id = m.remember(ALICE, Slot::Journal, "Rendez-vous chez Léa mardi", Meta::default()).await.unwrap().mem_id.unwrap();
        assert!(m.forget(ALICE, id).await.unwrap());
        assert!(!m.forget(ALICE, id).await.unwrap());
        assert!(m.list(ALICE, None, 10).await.unwrap().is_empty());
        let back = m.remember(ALICE, Slot::Journal, "Rendez-vous chez Léa mardi", Meta::default()).await.unwrap();
        assert_eq!(back.mem_id, None, "la pierre tombale bloque le retour du même texte");
        drop(m);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn slots_and_seasons() {
        assert_eq!(Slot::ALL.map(Slot::as_i64), [0, 13, 14, 15]);
        assert!(matches!(Slot::from_i64(7), Err(BroError::BadSlot(7))));
        assert_eq!(season(1_790_000_000), "automne"); // 2026-09-21
    }
}
