//! Pool de lecteurs : connexions SQLite en lecture seule (WAL), exécutées hors du runtime async.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::Semaphore;

use crate::writer::VectorTable;
use crate::{DbError, register_sqlite_vec};

/// Pool de connexions en lecture seule. Cloner est peu coûteux.
#[derive(Clone)]
pub struct ReaderPool {
    conns: Arc<Mutex<Vec<Connection>>>,
    permits: Arc<Semaphore>,
}

impl ReaderPool {
    /// Ouvre `count` connexions de lecture. La base doit déjà exister (créée par l'écrivain).
    pub(crate) fn open(path: &Path, count: usize) -> Result<Self, DbError> {
        register_sqlite_vec();
        let mut conns = Vec::with_capacity(count);
        for _ in 0..count {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.execute_batch("PRAGMA busy_timeout = 30000; PRAGMA query_only = ON;")?;
            conns.push(conn);
        }
        Ok(Self { conns: Arc::new(Mutex::new(conns)), permits: Arc::new(Semaphore::new(count)) })
    }

    /// Exécute `f` sur une connexion de lecture, dans un thread bloquant.
    ///
    /// Attend qu'une connexion se libère (backpressure naturelle).
    pub async fn read<T, F>(&self, f: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, DbError> + Send + 'static,
    {
        let permit = self.permits.clone().acquire_owned().await.map_err(|_| DbError::ReaderPoolClosed)?;
        let conns = self.conns.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conns.lock().unwrap_or_else(|e| e.into_inner()).pop().ok_or(DbError::ReaderPoolClosed)?;
            let out = catch_unwind(AssertUnwindSafe(|| f(&conn)));
            conns.lock().unwrap_or_else(|e| e.into_inner()).push(conn);
            drop(permit);
            out.unwrap_or(Err(DbError::ReaderPanicked))
        })
        .await
        .map_err(|_| DbError::ReaderPanicked)?
    }

    /// Les `k` plus proches voisins (distance cosinus croissante) de `query` parmi les vecteurs de `owner_hex`.
    ///
    /// Retourne `(identifiant, distance)`.
    pub async fn knn(
        &self,
        table: VectorTable,
        owner_hex: &str,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<(i64, f64)>, DbError> {
        let blob = table.encode(query)?;
        let owner = owner_hex.to_owned();
        let sql = format!(
            "SELECT {id}, distance FROM {t} WHERE embedding MATCH ?1 AND owner_hex = ?2 AND k = ?3 ORDER BY distance",
            id = table.id_column(),
            t = table.table()
        );
        self.read(move |conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map(rusqlite::params![blob, owner, k as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }
}
