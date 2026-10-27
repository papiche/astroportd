//! Magasin anti-rejeu NIP-98 persistant (table `nip98_seen`).

use std::path::Path;
use std::sync::Mutex;

use astro_crypto::nip98::{ReplayError, ReplayStore};
use rusqlite::{Connection, params};

use crate::DbError;

/// Implémentation SQLite de [`ReplayStore`] : survit aux redémarrages du démon.
///
/// La connexion est protégée par un mutex ; ce chemin est bref (un `INSERT`) et
/// n'entre pas en concurrence avec l'acteur d'écriture principal quand il utilise sa propre base
/// ou la même en mode WAL avec `busy_timeout`.
pub struct SqliteReplayStore {
    conn: Mutex<Connection>,
}

impl SqliteReplayStore {
    /// Ouvre la base `path` (schéma appliqué si nécessaire).
    pub fn open(path: &Path) -> Result<Self, DbError> {
        Ok(Self { conn: Mutex::new(crate::open(path)?) })
    }

    /// Magasin sur une base en mémoire (tests).
    pub fn in_memory() -> Result<Self, DbError> {
        Ok(Self { conn: Mutex::new(crate::open_in_memory()?) })
    }
}

impl ReplayStore for SqliteReplayStore {
    fn check_and_insert(&self, event_id: &str, expires_at: u64, now: u64) -> Result<bool, ReplayError> {
        let conn = self.conn.lock().map_err(|_| ReplayError("verrou SQLite empoisonné".into()))?;
        let now = i64::try_from(now).map_err(|e| ReplayError(e.to_string()))?;
        let exp = i64::try_from(expires_at).map_err(|e| ReplayError(e.to_string()))?;
        conn.execute("DELETE FROM nip98_seen WHERE expires_at <= ?1", params![now])
            .map_err(|e| ReplayError(e.to_string()))?;
        let inserted = conn
            .execute("INSERT OR IGNORE INTO nip98_seen(event_id, expires_at) VALUES (?1, ?2)", params![event_id, exp])
            .map_err(|e| ReplayError(e.to_string()))?;
        Ok(inserted == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_insert_is_a_replay_until_expiry() {
        let s = SqliteReplayStore::in_memory().unwrap();
        assert!(s.check_and_insert("e1", 1_000, 900).unwrap());
        assert!(!s.check_and_insert("e1", 1_000, 901).unwrap());
        assert!(s.check_and_insert("e1", 2_000, 1_000).unwrap(), "entrée expirée purgée");
    }

    #[test]
    fn survives_reopening_the_database() {
        let dir = std::env::temp_dir().join(format!("astro-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("uplanet.db");
        {
            let s = SqliteReplayStore::open(&path).unwrap();
            assert!(s.check_and_insert("e1", 10_000, 100).unwrap());
        }
        let s = SqliteReplayStore::open(&path).unwrap();
        assert!(!s.check_and_insert("e1", 10_000, 101).unwrap());
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
