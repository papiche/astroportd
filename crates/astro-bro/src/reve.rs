//! Cycle RÊVE : quand un emplacement atteint [`REVE_THRESHOLD`] souvenirs, les plus anciens sont condensés en un
//! seul souvenir `[RÊVE]`, les [`REVE_KEEP`] plus récents restent intacts.
//!
//! Le résumé et son embedding sont calculés **avant** toute écriture ; la purge des anciens souvenirs (lignes et
//! vecteurs) et l'insertion du `[RÊVE]` forment ensuite **une seule transaction**. Si le résumé échoue ou si les
//! souvenirs ont changé entre-temps, rien n'est modifié. Les souvenirs écrits par la personne (`origin = user`,
//! sources `manual`/`rec`) ne sont jamais condensés : sa parole l'emporte sur un résumé automatique.

use std::collections::HashSet;
use std::sync::Mutex;

use astro_db::DbError;
use rusqlite::params;

use crate::memory::{MemoryStore, Slot, content_hash, now_secs, point_id, season};
use crate::ollama::{Embedder, Summarizer};
use crate::BroError;

/// Nombre de souvenirs d'un emplacement qui déclenche la compression.
pub const REVE_THRESHOLD: usize = 150;
/// Nombre de souvenirs récents conservés tels quels.
pub const REVE_KEEP: usize = 80;

/// Bilan d'une compression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamReport {
    /// Souvenirs condensés (et purgés).
    pub compressed: usize,
    /// Souvenirs récents conservés.
    pub kept: usize,
    /// Clé du souvenir `[RÊVE]` (absente s'il existait déjà à l'identique).
    pub summary_id: Option<i64>,
}

/// Libère le couple `(propriétaire, emplacement)` à la fin de la compression, même en cas d'erreur.
struct Busy<'a>(&'a Mutex<HashSet<(String, i64)>>, (String, i64));

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
    }
}

impl<E: Embedder, S: Summarizer> MemoryStore<E, S> {
    /// Balayage périodique : compresse tous les `(propriétaire, emplacement)` ayant atteint le seuil sans avoir été
    /// compressés à l'écriture (démon arrêté, résumé indisponible à l'époque…). Un échec n'arrête pas le balayage.
    pub async fn sweep(&self) -> Result<Vec<(String, Slot, Result<Option<DreamReport>, BroError>)>, BroError> {
        let due: Vec<(String, i64)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT owner_hex, slot FROM memory WHERE status = 'active' AND slot IS NOT NULL AND COALESCE(origin, '') <> 'user' \
                       AND COALESCE(source, '') NOT IN ('manual', 'rec') GROUP BY owner_hex, slot HAVING COUNT(*) >= ?1",
                )?;
                let rows = stmt.query_map([REVE_THRESHOLD as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;
        let mut out = Vec::new();
        for (owner, slot) in due {
            let Ok(slot) = Slot::from_i64(slot) else { continue };
            let result = self.dream(&owner, slot).await;
            if let Err(e) = &result {
                self.announce("bro:reve", format!("compression RÊVE impossible : {e}"));
            }
            out.push((owner, slot, result));
        }
        Ok(out)
    }

    /// Compresse l'emplacement `slot` de `owner` s'il a atteint le seuil. `Ok(None)` : rien à faire (sous le seuil
    /// ou compression déjà en cours).
    pub async fn dream(&self, owner: &str, slot: Slot) -> Result<Option<DreamReport>, BroError> {
        let key = (owner.to_owned(), slot.as_i64());
        if !self.dreaming.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
            return Ok(None);
        }
        let _busy = Busy(&self.dreaming, key);

        let (o, s) = (owner.to_owned(), slot.as_i64());
        let rows: Vec<(i64, u64, String)> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare_cached(
                    "SELECT mem_id, ts, content FROM memory WHERE owner_hex = ?1 AND slot = ?2 AND status = 'active' \
                       AND COALESCE(origin, '') <> 'user' AND COALESCE(source, '') NOT IN ('manual', 'rec') ORDER BY ts, mem_id",
                )?;
                let found = stmt
                    .query_map(params![o, s], |r| Ok((r.get::<_, i64>(0)?, u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0), r.get::<_, String>(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(found)
            })
            .await?;
        if rows.len() < REVE_THRESHOLD {
            return Ok(None);
        }
        let old = &rows[..rows.len() - REVE_KEEP];
        let block = old.iter().map(|(_, ts, content)| format!("[{}] {content}", astro_core::week::date_string(*ts))).collect::<Vec<_>>().join("\n");
        let summary = self.summarizer.summarize(&block).await?;
        let summary = summary.trim();
        if summary.is_empty() {
            return Err(BroError::EmptySummary);
        }
        let content = format!("[RÊVE] {summary}");
        let embedding = self.embedder.embed(&content).await?;
        let blob = astro_db::VectorTable::Memory.encode(&embedding)?;

        // Le résumé porte la date du DERNIER souvenir qu'il remplace, pas « maintenant » : sinon il passerait pour
        // le souvenir le plus récent (tri, prochaine compression).
        let ts = old.last().map_or_else(now_secs, |r| r.1);
        let ids: Vec<i64> = old.iter().map(|r| r.0).collect();
        let (compressed, kept) = (ids.len(), rows.len() - ids.len());
        let hash = content_hash(&content);
        let (owner_s, pid, content_s) = (owner.to_owned(), point_id(owner, &hash), content);
        let outcome = self
            .db
            .writer()
            .transaction(move |tx| {
                let mut live = 0usize;
                {
                    let mut check = tx.prepare("SELECT COUNT(*) FROM memory WHERE mem_id = ?1 AND owner_hex = ?2 AND status = 'active'")?;
                    for id in &ids {
                        live += usize::try_from(check.query_row(params![id, owner_s], |r| r.get::<_, i64>(0))?).unwrap_or(0);
                    }
                }
                if live != ids.len() {
                    return Err(DbError::Aborted); // une autre écriture a touché ces souvenirs : on n'achève rien
                }
                for id in &ids {
                    tx.execute("DELETE FROM memory_vectors WHERE mem_id = ?1", params![id])?;
                    tx.execute("DELETE FROM memory WHERE mem_id = ?1", params![id])?;
                }
                let n = tx.execute(
                    "INSERT INTO memory(point_id, owner_hex, content, content_hash, ts, season, importance, kind, source, visibility, confidence, status, embed_model, schema_v, slot) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'episode', 'reve', 'private', 1.0, 'active', 'bge-m3', 2, ?8) ON CONFLICT(point_id) DO NOTHING",
                    params![pid, owner_s, content_s, hash, ts as i64, season(ts), compressed as f64, s],
                )?;
                let summary_id = if n == 0 {
                    None
                } else {
                    let id = tx.last_insert_rowid();
                    tx.execute("INSERT INTO memory_vectors(mem_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![id, owner_s, blob])?;
                    Some(id)
                };
                Ok(summary_id)
            })
            .await;
        match outcome {
            Ok(summary_id) => {
                self.announce("bro:reve", format!("RÊVE : {compressed} souvenirs condensés, {kept} conservés (emplacement {})", slot.as_i64()));
                Ok(Some(DreamReport { compressed, kept, summary_id }))
            }
            Err(DbError::Aborted) => Err(BroError::Raced),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::memory::Meta;
    use crate::ollama::HashingEmbedder;
    use crate::testkit::{Broken, Echo, temp_db};

    const OWNER: &str = "aa";
    const T0: u64 = 1_780_000_000;

    async fn fill<S: Summarizer>(m: &MemoryStore<HashingEmbedder, S>, slot: Slot, n: usize, meta: impl Fn(usize) -> Meta) {
        for i in 0..n {
            let id = m.insert(OWNER, Some(slot), &format!("souvenir numéro {i} du jeu {}", slot.as_i64()), &Meta { ts: Some(T0 + i as u64 * 60), ..meta(i) }).await.unwrap();
            assert!(id.is_some());
        }
    }

    async fn counts(m: &MemoryStore<HashingEmbedder, impl Summarizer>) -> (i64, i64) {
        m.db.readers().read(|c| Ok((c.query_row("SELECT COUNT(*) FROM memory", [], |r| r.get(0))?, c.query_row("SELECT COUNT(*) FROM memory_vectors", [], |r| r.get(0))?))).await.unwrap()
    }

    #[tokio::test]
    async fn the_150th_memory_triggers_a_dream_that_keeps_the_80_newest_and_purges_atomically() {
        let (db, dir) = temp_db("reve");
        let m = MemoryStore::new(db, Arc::new(HashingEmbedder), Arc::new(Echo), None);
        fill(&m, Slot::Journal, REVE_THRESHOLD - 1, |_| Meta::default()).await;
        fill(&m, Slot::Goals, 5, |_| Meta::default()).await; // un autre emplacement n'est pas touché
        assert!(m.dream(OWNER, Slot::Journal).await.unwrap().is_none(), "sous le seuil : rien");

        let last = m.remember(OWNER, Slot::Journal, "le cent-cinquantième souvenir arrive", Meta { ts: Some(T0 + 10_000), ..Meta::default() }).await.unwrap();
        let Some(report) = last.dream else { unreachable!("la 150e écriture déclenche le RÊVE") };
        assert_eq!((report.compressed, report.kept), (REVE_THRESHOLD - REVE_KEEP, REVE_KEEP));
        assert!(last.dream_error.is_none());

        // 80 conservés + 1 [RÊVE] + 5 d'un autre emplacement ; autant de vecteurs que de lignes (purge atomique).
        assert_eq!(counts(&m).await, (86, 86));
        assert_eq!(m.count_active(OWNER, Slot::Journal).await.unwrap(), REVE_KEEP + 1);
        let all = m.list(OWNER, Some(Slot::Journal), 500).await.unwrap();
        let dream = all.iter().find(|x| x.content.starts_with("[RÊVE]")).unwrap();
        assert_eq!(dream.content, "[RÊVE] 70 souvenirs résumés");
        assert_eq!(dream.ts, T0 + 69 * 60, "daté du dernier souvenir condensé");
        // Les 80 plus récents sont intacts : le 70e (indice 70) est le plus ancien conservé.
        assert!(all.iter().any(|x| x.content == "souvenir numéro 70 du jeu 15"));
        assert!(!all.iter().any(|x| x.content == "souvenir numéro 69 du jeu 15"));
        assert_eq!(m.count_active(OWNER, Slot::Goals).await.unwrap(), 5);
        drop(m);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_sweep_dreams_every_slot_that_reached_the_threshold() {
        let (db, dir) = temp_db("reve-sweep");
        let m = MemoryStore::new(db, Arc::new(HashingEmbedder), Arc::new(Echo), None);
        fill(&m, Slot::Journal, REVE_THRESHOLD, |_| Meta::default()).await;
        fill(&m, Slot::Goals, 10, |_| Meta::default()).await;
        let swept = m.sweep().await.unwrap();
        assert_eq!(swept.len(), 1, "seul le journal a atteint le seuil");
        assert!(matches!(&swept[0].2, Ok(Some(r)) if r.compressed == REVE_THRESHOLD - REVE_KEEP));
        assert!(m.sweep().await.unwrap().is_empty(), "plus rien à compresser");
        drop(m);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failing_summary_changes_nothing_and_the_failure_is_reported() {
        let (db, dir) = temp_db("reve-fail");
        let m = MemoryStore::new(db, Arc::new(HashingEmbedder), Arc::new(Broken), None);
        fill(&m, Slot::Journal, REVE_THRESHOLD - 1, |_| Meta::default()).await;
        let r = m.remember(OWNER, Slot::Journal, "le dernier qui déclenche", Meta::default()).await.unwrap();
        assert!(r.mem_id.is_some(), "le souvenir est bien écrit");
        assert!(r.dream.is_none());
        assert!(r.dream_error.as_deref().is_some_and(|e| e.contains("Ollama")), "{:?}", r.dream_error);
        assert_eq!(counts(&m).await, (150, 150));
        drop(m);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn memories_written_by_the_person_are_never_condensed() {
        let (db, dir) = temp_db("reve-pinned");
        let m = MemoryStore::new(db, Arc::new(HashingEmbedder), Arc::new(Echo), None);
        fill(&m, Slot::Journal, REVE_THRESHOLD + 10, |i| if i < 10 { Meta { origin: Some("user".into()), ..Meta::default() } } else { Meta::default() }).await;
        let report = m.dream(OWNER, Slot::Journal).await.unwrap().unwrap();
        // 150 éligibles (le seuil se compte hors souvenirs de la personne) : 70 condensés, 80 conservés ;
        // les 10 souvenirs de la personne restent en plus.
        assert_eq!((report.compressed, report.kept), (70, 80));
        let all = m.list(OWNER, Some(Slot::Journal), 500).await.unwrap();
        assert!((0..10).all(|i| all.iter().any(|x| x.content == format!("souvenir numéro {i} du jeu 15"))));
        assert_eq!(all.len(), 10 + 80 + 1);
        drop(m);
        let _ = std::fs::remove_dir_all(dir);
    }
}
