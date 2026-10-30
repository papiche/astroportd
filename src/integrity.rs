//! Contrôle d'intégrité du stockage, à 20h12 : confronte les blobs que l'uCloud référence à ce que la station détient, et
//! recalcule l'empreinte des blobs `raw` (le CID **est** le SHA-256 du blob chiffré : aucune clé n'est nécessaire).
//!
//! Un passage lit au plus un lot de CID et un budget d'octets, puis reprend où il s'était arrêté la nuit suivante : un gros
//! stockage est parcouru en quelques nuits sans saturer le disque. Un blob absent ou corrompu est dit au Capitaine
//! (bus d'activité) avant qu'un ami ne le découvre ; ce contrôle ne répare ni ne supprime rien.

use std::sync::Arc;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_dav::blob::{BlobError, BlobStore, cid_raw_sha256, is_cid};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

/// CID examinés au plus par nuit.
const BATCH: i64 = 500;
/// Octets relus au plus par nuit.
const BYTE_BUDGET: u64 = 1 << 30;

/// Le contrôle d'intégrité d'une station.
pub struct Integrity<B: BlobStore> {
    db: Arc<Db>,
    blobs: Arc<B>,
    activity: Option<ActivityBus>,
}

impl<B: BlobStore> Integrity<B> {
    /// `blobs` : le stockage de CETTE station (pas le maillage : un blob absent ici ne doit pas être cherché chez les pairs).
    pub fn new(db: Arc<Db>, blobs: Arc<B>, activity: Option<ActivityBus>) -> Self {
        Self { db, blobs, activity }
    }

    /// Un passage. Rend le résumé pour le journal de nuit.
    pub async fn run(&self, now: u64) -> Result<String, String> {
        let cursor: String = self.db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'integrity' AND key = 'cursor'", [], |r| r.get(0)).unwrap_or_default())).await.map_err(|e| e.to_string())?;
        // Blobs des fichiers et de leurs miniatures, dans l'ordre des CID pour pouvoir reprendre.
        let cids: Vec<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT cid FROM (SELECT cid FROM ucloud_entries WHERE cid IS NOT NULL UNION SELECT json_extract(data, '$.thumb.cid') FROM ucloud_entries WHERE json_extract(data, '$.thumb.cid') IS NOT NULL) WHERE cid > ?1 ORDER BY cid LIMIT ?2",
                )?;
                let rows = stmt.query_map(rusqlite::params![cursor, BATCH], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| e.to_string())?;
        let wrapped = (i64::try_from(cids.len()).unwrap_or(0)) < BATCH;
        let (mut checked, mut verified, mut spent) = (0u64, 0u64, 0u64);
        let (mut missing, mut corrupt) = (Vec::<String>::new(), Vec::<String>::new());
        let mut last = String::new();
        for cid in &cids {
            if spent >= BYTE_BUDGET {
                break;
            }
            last.clone_from(cid);
            checked += 1;
            if !is_cid(cid) {
                continue;
            }
            match self.blobs.open(cid).await {
                Err(BlobError::NotFound) => missing.push(cid.clone()),
                Err(e) => return Err(format!("lecture de {cid} : {e}")),
                Ok((mut reader, size)) => {
                    // Seuls les blocs `raw` se vérifient par leur empreinte ; une racine UnixFS (`bafybei`) est seulement présente ou non.
                    if cid.starts_with("bafkrei") {
                        let (mut hasher, mut buf) = (Sha256::new(), vec![0u8; 1 << 20]);
                        loop {
                            let n = reader.read(&mut buf).await.map_err(|e| format!("lecture de {cid} : {e}"))?;
                            if n == 0 {
                                break;
                            }
                            hasher.update(&buf[..n]);
                        }
                        let digest: [u8; 32] = hasher.finalize().into();
                        verified += 1;
                        if cid_raw_sha256(&digest) != *cid {
                            corrupt.push(cid.clone());
                        }
                    }
                    spent += size;
                }
            }
        }
        let done_pass = wrapped && checked == cids.len() as u64;
        let next = if done_pass { String::new() } else { last };
        let report = json!({ "at": now, "checked": checked, "verified": verified, "missing": missing, "corrupt": corrupt, "pass_complete": done_pass });
        let _ = self.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('integrity', 'cursor', ?1, ?2) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![next.into(), i64::try_from(now).unwrap_or(0).into()]).await;
        let _ = self.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('integrity', 'report', ?1, ?2) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![report.to_string().into(), i64::try_from(now).unwrap_or(0).into()]).await;
        let summary = format!("{checked} blob(s) examiné(s), {verified} empreinte(s) recalculée(s), {} absent(s), {} corrompu(s){}", missing.len(), corrupt.len(), if done_pass { " (passage complet)" } else { "" });
        if !missing.is_empty() || !corrupt.is_empty() {
            let msg = format!("Intégrité : {} blob(s) absent(s) et {} corrompu(s) sur cette station. Exemple : {}", missing.len(), corrupt.len(), missing.first().or(corrupt.first()).map_or("", String::as_str));
            if let Some(bus) = &self.activity {
                bus.publish_to(Audience::Captain, "system", "integrity", msg.clone(), Some(report.clone()));
            }
            return Err(msg);
        }
        Ok(summary)
    }

    /// Dernier rapport (`null` si aucun passage).
    pub async fn last_report(&self) -> Value {
        self.db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'integrity' AND key = 'report'", [], |r| r.get::<_, String>(0)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null))).await.unwrap_or(Value::Null)
    }
}
