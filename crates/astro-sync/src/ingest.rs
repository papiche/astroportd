//! Ingestion atomique d'un événement distant : déduplication (`claimed`), stockage et projection en une transaction.

use astro_db::events::{InsertOutcome, insert_event};
use astro_db::{Writer, apply_statements};
use astro_nostr_proto::Event;
use rusqlite::params;

use crate::projector::Projector;
use crate::SyncError;

/// Sort d'un événement ingéré.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingested {
    /// Nouvel événement stocké et projeté.
    Inserted,
    /// Déjà traité (réclamé ou connu).
    Duplicate,
    /// Une version plus récente d'un événement remplaçable est déjà stockée.
    Superseded,
    /// Refusé avant stockage (signature, auteur, kind).
    Rejected(&'static str),
}

/// Ingère `event` : tout ou rien.
///
/// La réclamation dans `claimed` (`owner`, `event_id`), l'insertion et la projection partagent la même
/// transaction : un échec ne laisse ni événement réclamé sans être stocké, ni stocké sans projection.
/// Un événement dont le plan de projection échoue reste stocké (et se reprojette au prochain replay).
pub async fn ingest_event(writer: &Writer, event: Event, owner: &str) -> Result<Ingested, SyncError> {
    if event.verify().is_err() {
        return Ok(Ingested::Rejected("signature ou identifiant invalide"));
    }
    let owner = owner.to_owned();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let outcome = writer
        .transaction(move |tx| {
            let claimed = tx.execute(
                "INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES (?1, ?2, ?3)",
                params![owner, event.id, i64::try_from(now).unwrap_or(i64::MAX)],
            )?;
            if claimed == 0 {
                return Ok(Ingested::Duplicate);
            }
            Ok(match insert_event(tx, &event)? {
                InsertOutcome::Inserted => {
                    if let Ok(plan) = Projector::plan(&event) {
                        apply_statements(tx, &plan)?;
                    }
                    Ingested::Inserted
                }
                InsertOutcome::Duplicate => Ingested::Duplicate,
                InsertOutcome::Superseded => Ingested::Superseded,
                InsertOutcome::Ephemeral => Ingested::Rejected("kind éphémère non stocké"),
                InsertOutcome::Deleted => Ingested::Rejected("supprimé par son auteur (NIP-09)"),
            })
        })
        .await?;
    Ok(outcome)
}

/// Purge les réclamations plus anciennes que `max_age_secs` (7 jours dans le legacy).
pub async fn purge_claimed(writer: &Writer, max_age_secs: u64) -> Result<usize, SyncError> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let cutoff = i64::try_from(now.saturating_sub(max_age_secs)).unwrap_or(0);
    let out = writer.execute("DELETE FROM claimed WHERE at < ?1", vec![cutoff.into()]).await?;
    Ok(out.changes)
}
