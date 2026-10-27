//! File d'attente persistante : une analyse à la fois, avec reprise sur incident.

use std::sync::Arc;
use std::time::Duration;

use astro_dav::{BlobStore, UploadHook};
use rusqlite::{OptionalExtension, params};
use tokio::sync::watch;

use crate::engine::FaceEngine;
use crate::pipeline::{FaceCloud, FaceError, Outbox, now};

const KIND: &str = "facecloud";
/// Tentatives avant abandon.
const MAX_ATTEMPTS: i64 = 5;

/// Bilan d'un passage de la file.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct QueueStats {
    /// Tâches terminées.
    pub done: usize,
    /// Tâches replanifiées après échec.
    pub retried: usize,
    /// Tâches abandonnées.
    pub failed: usize,
}

impl<B: BlobStore, E: FaceEngine, O: Outbox> FaceCloud<B, E, O> {
    /// Met une photo en file d'attente (sans doublon tant qu'elle n'est pas traitée).
    pub async fn enqueue(&self, owner: &str, path: &str) -> Result<(), FaceError> {
        let payload = serde_json::json!({ "owner": owner, "path": path }).to_string();
        self.db
            .writer()
            .execute(
                "INSERT INTO tasks(kind, payload, status, created_at) SELECT ?1, ?2, 'pending', ?3 \
                 WHERE NOT EXISTS (SELECT 1 FROM tasks WHERE kind = ?1 AND payload = ?2 AND status IN ('pending', 'running'))",
                vec![KIND.to_owned().into(), payload.into(), now().into()],
            )
            .await?;
        Ok(())
    }

    /// Rappel à brancher sur `DavState::on_upload` : met les images déposées en file d'attente.
    pub fn upload_hook(self: &Arc<Self>) -> UploadHook {
        let me = self.clone();
        Arc::new(move |owner: &str, path: &str, mime: &str| {
            if mime.starts_with("image/") {
                let (me, owner, path) = (me.clone(), owner.to_owned(), path.to_owned());
                tokio::spawn(async move {
                    let _ = me.enqueue(&owner, &path).await;
                });
            }
        })
    }

    /// Remet en attente les tâches interrompues par un arrêt brutal.
    pub async fn recover(&self) -> Result<usize, FaceError> {
        Ok(self.db.writer().execute("UPDATE tasks SET status = 'pending' WHERE kind = ?1 AND status = 'running'", vec![KIND.to_owned().into()]).await?.changes)
    }

    async fn claim(&self) -> Result<Option<(i64, String, i64)>, FaceError> {
        let t = now();
        Ok(self
            .db
            .writer()
            .transaction(move |tx| {
                let row: Option<(i64, String, i64)> = tx
                    .query_row(
                        "SELECT id, payload, attempts FROM tasks WHERE kind = ?1 AND status = 'pending' AND (not_before IS NULL OR not_before <= ?2) ORDER BY id LIMIT 1",
                        params![KIND, t],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                if let Some((id, _, _)) = &row {
                    tx.execute("UPDATE tasks SET status = 'running' WHERE id = ?1", params![id])?;
                }
                Ok(row)
            })
            .await?)
    }

    /// Traite au plus `max` tâches échues, une à la fois.
    pub async fn run_pending(&self, max: usize) -> Result<QueueStats, FaceError> {
        let mut stats = QueueStats::default();
        for _ in 0..max {
            let Some((id, payload, attempts)) = self.claim().await? else { break };
            let job: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
            let (owner, path) = (job["owner"].as_str().unwrap_or(""), job["path"].as_str().unwrap_or(""));
            match self.process_photo(owner, path).await {
                Ok(_) => {
                    self.db.writer().execute("UPDATE tasks SET status = 'done' WHERE id = ?1", vec![id.into()]).await?;
                    stats.done += 1;
                }
                Err(FaceError::Invalid(_)) => {
                    // Photo supprimée entre-temps ou non analysable : inutile de réessayer.
                    self.db.writer().execute("UPDATE tasks SET status = 'failed', attempts = attempts + 1 WHERE id = ?1", vec![id.into()]).await?;
                    stats.failed += 1;
                }
                Err(_) if attempts + 1 >= MAX_ATTEMPTS => {
                    self.db.writer().execute("UPDATE tasks SET status = 'failed', attempts = attempts + 1 WHERE id = ?1", vec![id.into()]).await?;
                    stats.failed += 1;
                }
                Err(_) => {
                    // Reprise exponentielle : 1 min, 2, 4, 8.
                    let delay = 60 * (1i64 << attempts.min(6));
                    self.db
                        .writer()
                        .execute("UPDATE tasks SET status = 'pending', attempts = attempts + 1, not_before = ?2 WHERE id = ?1", vec![id.into(), (now() + delay).into()])
                        .await?;
                    stats.retried += 1;
                }
            }
        }
        Ok(stats)
    }

    /// Travailleur de fond : un passage toutes les `poll`, jusqu'au signal d'arrêt.
    pub async fn run_worker(self: Arc<Self>, poll: Duration, mut shutdown: watch::Receiver<bool>) {
        let _ = self.recover().await;
        loop {
            while matches!(self.run_pending(10).await, Ok(s) if s.done + s.retried + s.failed > 0) {}
            tokio::select! {
                () = tokio::time::sleep(poll) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }
}
