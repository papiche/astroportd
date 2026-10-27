//! Cache local SQLite + recherche vectorielle (`sqlite-vec`).
//!
//! Nostr et IPFS sont les sources de vérité ; `uplanet.db` se reconstruit par `astroportd replay`.
//! Le schéma ([`SCHEMA`]) distingue les tables *projection* (rejouables) des tables *locales*.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod events;
mod reader;
mod replay;
mod writer;

use std::path::Path;
use std::sync::Once;

pub use reader::ReaderPool;
pub use replay::SqliteReplayStore;
use rusqlite::Connection;
pub use writer::{QUEUE_CAPACITY, Statement, VectorTable, WriteOutcome, Writer, apply_statements};

/// Schéma complet (DDL idempotent).
pub const SCHEMA: &str = include_str!("schema.sql");

/// Erreurs de la couche base.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// Erreur SQLite.
    #[error("sqlite : {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// L'acteur d'écriture est arrêté (ou a perdu la commande en cours).
    #[error("acteur d'écriture arrêté")]
    WriterClosed,
    /// Le pool de lecture est fermé.
    #[error("pool de lecture fermé")]
    ReaderPoolClosed,
    /// Une fermeture de lecture a paniqué.
    #[error("la lecture a paniqué")]
    ReaderPanicked,
    /// Le vecteur n'a pas la dimension de la table.
    #[error("vecteur de {got} dimensions pour {table} ({expected} attendues)")]
    DimensionMismatch {
        /// Table vectorielle visée.
        table: &'static str,
        /// Dimension attendue.
        expected: usize,
        /// Dimension reçue.
        got: usize,
    },
    /// Le vecteur contient NaN ou une valeur infinie.
    #[error("vecteur non fini refusé")]
    NonFiniteVector,
    /// Chemin de destination inutilisable (UTF-8 requis par SQLite).
    #[error("chemin invalide : {0}")]
    BadPath(String),
    /// Nombre de lecteurs nul.
    #[error("au moins un lecteur est requis")]
    NoReaders,
    /// Erreur d'entrée/sortie.
    #[error("entrée/sortie : {0}")]
    Io(#[from] std::io::Error),
    /// Transaction volontairement annulée par l'appelant (l'erreur métier est portée à part).
    #[error("transaction annulée")]
    Aborted,
    /// Ligne illisible (JSON de tags corrompu…).
    #[error("donnée corrompue : {0}")]
    Corrupt(String),
}

/// Base `uplanet.db` : un écrivain unique et un pool de lecteurs en lecture seule (WAL).
///
/// Toutes les mutations passent par [`Writer`] (file `mpsc` bornée, thread dédié, une transaction par message) :
/// plus de `SQLITE_BUSY` entre écrivains. Les lecteurs ne bloquent ni ne sont bloqués par l'écrivain.
pub struct Db {
    writer: Writer,
    readers: ReaderPool,
}

impl Db {
    /// Ouvre (ou crée) la base `path` avec `readers` connexions de lecture.
    pub fn open(path: &Path, readers: usize) -> Result<Self, DbError> {
        if readers == 0 {
            return Err(DbError::NoReaders);
        }
        let conn = open(path)?; // applique le schéma et crée le fichier WAL avant d'ouvrir les lecteurs
        let pool = ReaderPool::open(path, readers)?;
        Ok(Self { writer: Writer::spawn(conn)?, readers: pool })
    }

    /// Écrivain unique (cloner pour le partager entre tâches).
    pub fn writer(&self) -> &Writer {
        &self.writer
    }

    /// Pool de lecteurs.
    pub fn readers(&self) -> &ReaderPool {
        &self.readers
    }
}

pub(crate) static VEC_INIT: Once = Once::new();

/// Enregistre `sqlite-vec` pour toutes les connexions ouvertes ensuite dans ce processus.
#[allow(unsafe_code)]
pub fn register_sqlite_vec() {
    VEC_INIT.call_once(|| {
        // SAFETY : `sqlite3_vec_init` est le point d'entrée d'extension SQLite documenté par sqlite-vec ;
        // `sqlite3_auto_extension` attend exactement ce pointeur de fonction, appelé une seule fois.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::os::raw::c_int,
            >(sqlite_vec::sqlite3_vec_init as *const ())));
        }
    });
}

/// Crée le fichier en 0600 s'il n'existe pas : la base contient le trousseau des clés de fichiers. SQLite donne
/// aux fichiers `-wal` et `-shm` les droits du fichier principal.
fn create_private(path: &Path) -> Result<(), DbError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?;
    Ok(())
}

/// Ouvre (et crée si besoin) la base à `path`, applique les pragmas et le schéma.
pub fn open(path: &Path) -> Result<Connection, DbError> {
    register_sqlite_vec();
    create_private(path)?;
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Colonnes ajoutées après la première version du schéma : `(table, colonne, définition)`.
/// `CREATE TABLE IF NOT EXISTS` ne modifie pas une table existante ; une base créée avant l'ajout reçoit la colonne ici.
const ADDED_COLUMNS: [(&str, &str, &str); 3] = [("identities", "peer_id", "TEXT"), ("capital_assets", "node_hex", "TEXT"), ("resource_leases", "unit_price_cents", "INTEGER NOT NULL DEFAULT 0")];

/// Ajoute à une base ancienne les colonnes apparues depuis (idempotent).
fn migrate(conn: &Connection) -> Result<(), DbError> {
    for (table, column, def) in ADDED_COLUMNS {
        let has: bool = conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = '{column}')"), [], |r| r.get(0))?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {def}"))?;
        }
    }
    Ok(())
}

/// Base en mémoire avec le schéma complet (tests).
pub fn open_in_memory() -> Result<Connection, DbError> {
    register_sqlite_vec();
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Dossier temporaire supprimé à la destruction.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("astro-db-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_literal(dim: usize, hot: usize) -> Vec<u8> {
        (0..dim).flat_map(|i| if i == hot { 1.0f32 } else { 0.0 }.to_le_bytes()).collect()
    }

    #[test]
    fn schema_applies_twice() {
        let conn = open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let v: String = conn.query_row("SELECT value FROM schema_info WHERE key='schema_version'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "1");
    }

    #[test]
    fn vector_tables_have_the_expected_dimensions() {
        let conn = open_in_memory().unwrap();
        for (table, dim) in [("face_vectors", 512), ("memory_vectors", 1024), ("ucloud_desc_vectors", 1024)] {
            let id_col = if table == "face_vectors" { "face_id" } else if table == "memory_vectors" { "mem_id" } else { "emb_id" };
            conn.execute(
                &format!("INSERT INTO {table}({id_col}, owner_hex, embedding) VALUES (1, 'aa', ?1)"),
                [vec_literal(dim, 0)],
            )
            .unwrap();
            let wrong = conn.execute(
                &format!("INSERT INTO {table}({id_col}, owner_hex, embedding) VALUES (2, 'aa', ?1)"),
                [vec_literal(dim + 1, 0)],
            );
            assert!(wrong.is_err(), "{table} doit refuser une mauvaise dimension");
        }
        conn.execute("INSERT INTO dream_vectors(dream_id, owner_hex, embedding) VALUES (1, 'aa', ?1)", [vec_literal(768, 3)]).unwrap();
        assert!(conn.execute("INSERT INTO dream_vectors(dream_id, owner_hex, embedding) VALUES (2, 'aa', ?1)", [vec_literal(512, 3)]).is_err());
    }

    #[test]
    fn cosine_knn_is_partitioned_by_owner() {
        let conn = open_in_memory().unwrap();
        let ins = "INSERT INTO face_vectors(face_id, owner_hex, embedding) VALUES (?1, ?2, ?3)";
        conn.execute(ins, rusqlite::params![1, "alice", vec_literal(512, 0)]).unwrap();
        conn.execute(ins, rusqlite::params![2, "alice", vec_literal(512, 1)]).unwrap();
        conn.execute(ins, rusqlite::params![3, "bob", vec_literal(512, 0)]).unwrap();
        let (id, dist): (i64, f64) = conn
            .query_row(
                "SELECT face_id, distance FROM face_vectors WHERE embedding MATCH ?1 AND owner_hex = 'alice' AND k = 1",
                [vec_literal(512, 0)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(id, 1);
        assert!(dist.abs() < 1e-6);
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM face_vectors WHERE embedding MATCH ?1 AND owner_hex = 'bob' AND k = 5",
                [vec_literal(512, 0)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn key_formula_is_constrained() {
        let conn = open_in_memory().unwrap();
        let ins = |f: &str| {
            conn.execute(
                "INSERT INTO identities(hex, role, key_formula) VALUES (?1, 'multipass', ?2)",
                rusqlite::params![f.repeat(2), f],
            )
        };
        assert!(ins("legacy").is_ok());
        assert!(ins("destroyed").is_err());
    }

    #[test]
    fn keyring_rejects_malformed_keys() {
        let conn = open_in_memory().unwrap();
        let r = conn.execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex) VALUES ('a','Qm1','abcd')", []);
        assert!(r.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_new_database_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("astro-db-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("uplanet.db");
        let conn = open(&path).unwrap();
        conn.execute_batch("INSERT INTO kv(scope, key, value, updated_at) VALUES ('t', 'k', 'v', 0)").unwrap();
        for f in [path.clone(), dir.join("uplanet.db-wal"), dir.join("uplanet.db-shm")] {
            if let Ok(m) = std::fs::metadata(&f) {
                assert_eq!(m.permissions().mode() & 0o077, 0, "{} doit être en 0600", f.display());
            }
        }
        drop(conn);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_old_database_gets_the_columns_added_since() {
        let dir = std::env::temp_dir().join(format!("astro-db-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("uplanet.db");
        {
            // Base de la première version : `identities` sans `peer_id`.
            let old = Connection::open(&path).unwrap();
            let without = SCHEMA.lines().filter(|l| !l.trim_start().starts_with("peer_id")).collect::<Vec<_>>().join("\n");
            assert!(without.len() < SCHEMA.len(), "la colonne peer_id doit exister dans le schéma courant");
            old.execute_batch(&without).unwrap();
        }
        let conn = open(&path).unwrap();
        conn.execute("INSERT INTO identities(hex, role, peer_id) VALUES ('aa', 'node', '12D3KooWx')", []).unwrap();
        drop(conn);
        drop(open(&path).unwrap()); // seconde ouverture : rien à refaire
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_creates_a_wal_file_database() {
        let dir = std::env::temp_dir().join(format!("astro-db-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("uplanet.db");
        let conn = open(&path).unwrap();
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        drop(conn);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
