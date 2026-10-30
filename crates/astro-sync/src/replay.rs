//! `astroportd replay` : reconstruit les tables de projection depuis `nostr_events`.

use astro_crypto::KeySource;
use astro_dav::BlobStore;
use astro_dav::manifest::{self, D_TAG, ManifestError};
use astro_db::events::row_to_event;
use astro_db::Db;

use crate::projector::Projector;
use crate::SyncError;

/// Kinds rejoués, dans l'ordre : identités, réseau social, puis virements.
const PHASES: [&[u32]; 4] = [&[30_800], &[3], &[30_850, 30_853], &[30_852, 30_851, 31_903]];

/// Bilan d'un replay.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReplayStats {
    /// Événements examinés.
    pub scanned: u64,
    /// Événements dont la projection a produit au moins une requête.
    pub projected: u64,
    /// Événements non projetables (voir `errors`).
    pub skipped: u64,
    /// Manifestes uCloud restaurés (VFS + trousseau).
    pub manifests: u64,
    /// Manifestes ignorés : clé du compte indisponible ici, blob absent ou contenu refusé.
    pub manifests_skipped: u64,
    /// Les 20 premières erreurs `(event_id, raison)`.
    pub errors: Vec<(String, String)>,
}

/// Rejoue tous les événements stockés. La base reste utilisable pendant l'opération ; les tables de projection
/// se remplissent progressivement (le relai peut servir, avec un cercle social incomplet jusqu'à la fin).
///
/// L'état LOCAL (identités `hosted = 1`, marqueurs `done` sans événement source) est conservé.
pub async fn replay_local(db: &Db, page_size: usize) -> Result<ReplayStats, SyncError> {
    let page_size = page_size.clamp(1, 10_000);
    let mut stats = ReplayStats::default();
    db.writer().batch(Projector::reset()).await?;
    for kinds in PHASES {
        let mut cursor: (i64, String) = (-1, String::new());
        loop {
            let (after, kinds_list) = (cursor.clone(), kinds.to_vec());
            let page = db
                .readers()
                .read(move |c| {
                    let marks = vec!["?"; kinds_list.len()].join(",");
                    let sql = format!(
                        "SELECT id, pubkey, kind, created_at, content, sig, tags FROM nostr_events \
                         WHERE kind IN ({marks}) AND (created_at > ? OR (created_at = ? AND id > ?)) \
                         ORDER BY created_at, id LIMIT ?"
                    );
                    let mut args: Vec<rusqlite::types::Value> = kinds_list.iter().map(|k| rusqlite::types::Value::Integer(i64::from(*k))).collect();
                    args.extend([after.0.into(), after.0.into(), after.1.clone().into(), i64::try_from(page_size).unwrap_or(i64::MAX).into()]);
                    let mut stmt = c.prepare(&sql)?;
                    let mut rows = stmt.query(rusqlite::params_from_iter(args.iter()))?;
                    let mut out = Vec::new();
                    while let Some(r) = rows.next()? {
                        out.push(row_to_event(r)?);
                    }
                    Ok(out)
                })
                .await?;
            let Some(last) = page.last() else { break };
            cursor = (i64::try_from(last.created_at).unwrap_or(i64::MAX), last.id.clone());
            let mut batch = Vec::new();
            for event in &page {
                stats.scanned += 1;
                match Projector::plan(event) {
                    Ok(plan) if plan.is_empty() => {}
                    Ok(plan) => {
                        stats.projected += 1;
                        batch.extend(plan);
                    }
                    Err(e) => {
                        stats.skipped += 1;
                        if stats.errors.len() < 20 {
                            stats.errors.push((event.id.clone(), e.to_string()));
                        }
                    }
                }
            }
            if !batch.is_empty() {
                db.writer().batch(batch).await?;
            }
        }
    }
    Ok(stats)
}

/// Replay complet **avec** restauration d'uCloud : après les projections habituelles, chaque événement
/// `30078 d="ucloud:manifest"` d'un compte dont la station détient la clé est déchiffré (NIP-44), son manifeste lu
/// dans `blobs`, puis le VFS et le trousseau du compte sont **remplacés** par ceux du manifeste.
///
/// Un compte sans manifeste n'est pas touché. Les modifications du VFS postérieures au dernier manifeste publié
/// (publication différée en cours) sont perdues : publier (`flush`) avant de lancer un replay sur une base vivante.
pub async fn replay_with_ucloud<B: BlobStore>(db: &Db, page_size: usize, blobs: &B, keys: &dyn KeySource) -> Result<ReplayStats, SyncError> {
    let mut stats = replay_local(db, page_size).await?;
    let events = db
        .readers()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, pubkey, kind, created_at, content, sig, tags FROM nostr_events WHERE kind = 30078 AND d_tag = ?1 ORDER BY created_at, id",
            )?;
            let mut rows = stmt.query([D_TAG])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                out.push(row_to_event(r)?);
            }
            Ok(out)
        })
        .await?;
    for event in &events {
        stats.scanned += 1;
        match manifest::restore_from_event(db, blobs, keys, event).await {
            Ok(_) => stats.manifests += 1,
            Err(ManifestError::NoKey) => stats.manifests_skipped += 1, // compte hébergé ailleurs : normal
            Err(e) => {
                stats.manifests_skipped += 1;
                if stats.errors.len() < 20 {
                    stats.errors.push((event.id.clone(), e.to_string()));
                }
            }
        }
    }
    Ok(stats)
}
