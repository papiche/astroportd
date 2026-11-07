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
    /// Le point de contrôle du journal WAL n'a pas pu tout rendre : un lecteur tenait encore une transaction.
    #[error("journal WAL non tronqué : un lecteur est resté ouvert pendant le point de contrôle (à rejouer)")]
    CheckpointBusy,
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
    apply_schema(&conn)?;
    migrate(&conn)?;
    tune_connection(&conn)?;
    Ok(conn)
}

/// Réglages de performance propres à chaque connexion (écrivain et lecteurs) : tables temporaires (tris, `DISTINCT`,
/// `GROUP BY` sans index) en mémoire plutôt que dans des fichiers, et lecture du fichier principal par projection
/// mémoire quand la machine s'y prête (voir [`mmap_budget`]).
pub(crate) fn tune_connection(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(&format!("PRAGMA temp_store = MEMORY; PRAGMA mmap_size = {};", mmap_budget()))?;
    Ok(())
}

/// Projection mémoire (`PRAGMA mmap_size`) accordée à chaque connexion : 64 Mo sur une machine 64 bits d'au moins
/// 4 Go de RAM, rien ailleurs. Sur 32 bits, l'espace d'adressage est trop court pour plusieurs connexions ; sur une
/// petite machine, les pages projetées concurrencent le cache du système. La RAM se lit dans `/proc/meminfo` : si
/// elle est inconnue (autre système, lecture impossible), la projection reste désactivée, comme avant.
pub fn mmap_budget() -> u64 {
    static BUDGET: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BUDGET.get_or_init(|| mmap_budget_for(cfg!(target_pointer_width = "64"), total_ram_bytes()))
}

fn mmap_budget_for(is_64_bit: bool, ram: Option<u64>) -> u64 {
    if is_64_bit && ram.is_some_and(|r| r >= 4 << 30) { 64 << 20 } else { 0 }
}

/// RAM totale de la machine (`MemTotal` de `/proc/meminfo`, Linux) ; `None` ailleurs ou si illisible.
pub fn total_ram_bytes() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// Applique le schéma, en faisant d'abord passer une base d'avant 0.5.0-beta à la nouvelle table d'événements.
fn apply_schema(conn: &Connection) -> Result<(), DbError> {
    let exists = |name: &str| -> Result<bool, DbError> { Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)", [name], |r| r.get(0))?) };
    // Avant 0.5.0-beta, `nostr_events` était WITHOUT ROWID (clé : l'identifiant de 64 caractères) et `nostr_tags` y renvoyait par
    // cet identifiant. On range l'ancienne paire de côté, le schéma crée la nouvelle, puis les lignes sont recopiées. Les deux étapes
    // sont reprenables : si le processus s'arrête entre elles, l'ouverture suivante termine la recopie (`nostr_events_old` existe).
    let legacy: bool = exists("nostr_events")? && !conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('nostr_events') WHERE name = 'seq')", [], |r| r.get::<_, bool>(0))?;
    if legacy {
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF; BEGIN; \
             DROP TRIGGER IF EXISTS nostr_fts_ai; DROP TRIGGER IF EXISTS nostr_fts_ad; \
             DROP TABLE IF EXISTS nostr_fts; DROP TABLE IF EXISTS nostr_fts_ids; \
             DROP INDEX IF EXISTS ne_kind_ts; DROP INDEX IF EXISTS ne_author; DROP INDEX IF EXISTS ne_replaceable; DROP INDEX IF EXISTS nt_lookup; \
             ALTER TABLE nostr_tags RENAME TO nostr_tags_old; ALTER TABLE nostr_events RENAME TO nostr_events_old; COMMIT;",
        )?;
    }
    // Même passage pour `ucloud_entries` (WITHOUT ROWID avant 0.5.0-beta) : même méthode, mêmes garanties de reprise.
    let legacy_vfs: bool = exists("ucloud_entries")? && !conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('ucloud_entries') WHERE name = 'id')", [], |r| r.get::<_, bool>(0))?;
    if legacy_vfs {
        conn.execute_batch(
            "BEGIN; \
             DROP TRIGGER IF EXISTS dav_changes_ai; DROP TRIGGER IF EXISTS dav_changes_au; DROP TRIGGER IF EXISTS dav_changes_ad; \
             DROP TRIGGER IF EXISTS dav_changes_ics_ai; DROP TRIGGER IF EXISTS dav_changes_ics_au; DROP TRIGGER IF EXISTS dav_changes_ics_ad; \
             DROP INDEX IF EXISTS ue_parent; DROP INDEX IF EXISTS ue_cid; DROP INDEX IF EXISTS ue_sha_orig; DROP INDEX IF EXISTS ue_sha_plain; \
             DROP INDEX IF EXISTS ue_taken; DROP INDEX IF EXISTS ue_phash; DROP INDEX IF EXISTS ue_geo; \
             ALTER TABLE ucloud_entries RENAME TO ucloud_entries_old; COMMIT;",
        )?;
    }
    conn.execute_batch(SCHEMA)?;
    if exists("ucloud_entries_old")? {
        conn.execute_batch(
            "BEGIN; \
             INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, sha_orig, sha_plain, faceid_status, origin, phash, lat, lon, desc_text, data, src_cid) \
               SELECT owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, sha_orig, sha_plain, faceid_status, origin, phash, lat, lon, desc_text, data, src_cid FROM ucloud_entries_old ORDER BY owner_hex, path; \
             DROP TABLE ucloud_entries_old; COMMIT;",
        )?;
    }
    if exists("nostr_events_old")? {
        // L'ordre d'insertion fixe les `seq` (du plus ancien au plus récent) ; les triggers remplissent l'index plein texte.
        conn.execute_batch(
            "BEGIN; \
             INSERT INTO nostr_events(id, pubkey, kind, created_at, content, sig, tags, d_tag, expires_at) \
               SELECT id, pubkey, kind, created_at, content, sig, tags, d_tag, expires_at FROM nostr_events_old ORDER BY created_at, id; \
             INSERT INTO nostr_tags(event_id, name, value) SELECT e.seq, t.name, t.value FROM nostr_tags_old t JOIN nostr_events e ON e.id = t.event_id; \
             DROP TABLE nostr_tags_old; DROP TABLE nostr_events_old; COMMIT;",
        )?;
    }
    Ok(())
}

/// Colonnes ajoutées après la première version du schéma : `(table, colonne, définition)`.
/// `CREATE TABLE IF NOT EXISTS` ne modifie pas une table existante ; une base créée avant l'ajout reçoit la colonne ici.
const ADDED_COLUMNS: [(&str, &str, &str); 4] = [("identities", "peer_id", "TEXT"), ("capital_assets", "node_hex", "TEXT"), ("faces", "contact_uid", "TEXT"), ("faces", "suggest_contact_uid", "TEXT")];

/// Ajoute à une base ancienne les colonnes apparues depuis (idempotent).
fn migrate(conn: &Connection) -> Result<(), DbError> {
    for (table, column, def) in ADDED_COLUMNS {
        let has: bool = conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = '{column}')"), [], |r| r.get(0))?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {def}"))?;
        }
    }
    // Index sur des colonnes ajoutées ci-dessus : après elles, jamais dans le schéma (une base ancienne ne les a pas encore).
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS faces_owner_contact ON faces(owner_hex, contact_uid) WHERE contact_uid IS NOT NULL; \
         CREATE INDEX IF NOT EXISTS identity_links_uid ON identity_links(owner_hex, uid) WHERE uid IS NOT NULL;",
    )?;
    // Base antérieure à l'index de recherche : les événements déjà là sont indexés une fois (les triggers font le reste).
    let (indexed, stored): (i64, i64) = conn.query_row("SELECT (SELECT count(*) FROM nostr_fts_docsize), (SELECT count(*) FROM nostr_events WHERE kind NOT IN (4, 1059))", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if indexed == 0 && stored > 0 {
        events::rebuild_search_index(conn)?;
    }
    Ok(())
}

/// Base en mémoire avec le schéma complet (tests).
pub fn open_in_memory() -> Result<Connection, DbError> {
    register_sqlite_vec();
    let conn = Connection::open_in_memory()?;
    apply_schema(&conn)?;
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
    fn a_database_from_before_0_5_keeps_its_events_tags_and_search_after_the_table_change() {
        let tmp = testutil::TempDir::new("legacy-events");
        let path = tmp.path().join("old.db");
        {
            // Ancienne forme : WITHOUT ROWID, tags rattachés par l'identifiant de 64 caractères, index plein texte à part.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE nostr_events(id TEXT PRIMARY KEY, pubkey TEXT NOT NULL, kind INTEGER NOT NULL, created_at INTEGER NOT NULL, content TEXT NOT NULL, sig TEXT NOT NULL, tags TEXT NOT NULL, d_tag TEXT, expires_at INTEGER) STRICT, WITHOUT ROWID;
                 CREATE INDEX ne_kind_ts ON nostr_events(kind, created_at DESC);
                 CREATE TABLE nostr_tags(event_id TEXT NOT NULL REFERENCES nostr_events(id) ON DELETE CASCADE, name TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(event_id, name, value)) STRICT, WITHOUT ROWID;
                 CREATE INDEX nt_lookup ON nostr_tags(name, value);
                 CREATE TABLE nostr_fts_ids(rowid INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE) STRICT;
                 INSERT INTO nostr_events VALUES ('aa', 'pk1', 1, 100, 'premier message', 's', '[[\"p\",\"zz\"]]', NULL, NULL), ('bb', 'pk1', 7, 200, 'second', 's', '[]', NULL, NULL);
                 INSERT INTO nostr_tags VALUES ('aa', 'p', 'zz');",
            )
            .unwrap();
        }
        let c = open(&path).unwrap();
        let (events, tagged, found): (i64, String, String) = c
            .query_row(
                "SELECT (SELECT count(*) FROM nostr_events), (SELECT e.id FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE t.name = 'p' AND t.value = 'zz'), \
                        (SELECT e.id FROM nostr_events e JOIN nostr_fts f ON f.rowid = e.seq AND nostr_fts MATCH 'premier*')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((events, tagged.as_str(), found.as_str()), (2, "aa", "aa"));
        let leftovers: i64 = c.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('nostr_events_old', 'nostr_tags_old', 'nostr_fts_ids')", [], |r| r.get(0)).unwrap();
        assert_eq!(leftovers, 0);
        drop(c);
        // Réouverture : rien ne change.
        let c = open(&path).unwrap();
        assert_eq!(c.query_row::<i64, _, _>("SELECT count(*) FROM nostr_events", [], |r| r.get(0)).unwrap(), 2);
    }

    #[test]
    fn a_database_from_before_0_5_keeps_its_virtual_filesystem_after_the_table_change() {
        let tmp = testutil::TempDir::new("legacy-vfs");
        let path = tmp.path().join("old.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE ucloud_entries(owner_hex TEXT NOT NULL, path TEXT NOT NULL, parent TEXT NOT NULL, type TEXT NOT NULL, cid TEXT, mime TEXT, size INTEGER, mtime INTEGER, taken_at INTEGER, sha_orig TEXT, sha_plain TEXT, faceid_status TEXT, origin TEXT, phash INTEGER, lat REAL, lon REAL, desc_text TEXT, data TEXT NOT NULL, src_cid TEXT, PRIMARY KEY(owner_hex, path)) STRICT, WITHOUT ROWID;
                 CREATE INDEX ue_parent ON ucloud_entries(owner_hex, parent);
                 INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES ('o', '/Contacts', '/', 'dir', NULL, NULL, 0, 1, '{}'), ('o', '/Contacts/a.vcf', '/Contacts', 'file', 'bafy1', 'text/vcard', 9, 2, '{}');",
            )
            .unwrap();
        }
        let c = open(&path).unwrap();
        let kids: Vec<String> = c.prepare("SELECT path FROM ucloud_entries WHERE owner_hex = 'o' AND parent = '/Contacts' ORDER BY path").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(kids, ["/Contacts/a.vcf"]);
        // Unicité conservée, déclencheurs du journal CardDAV reposés sur la nouvelle table.
        assert!(c.execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, data) VALUES ('o', '/Contacts/a.vcf', '/Contacts', 'file', '{}')", []).is_err());
        c.execute("DELETE FROM ucloud_entries WHERE path = '/Contacts/a.vcf'", []).unwrap();
        let logged: i64 = c.query_row("SELECT count(*) FROM dav_changes WHERE path = '/Contacts/a.vcf'", [], |r| r.get(0)).unwrap();
        assert!(logged >= 2, "ajout recopié puis suppression, journalisés");
        assert_eq!(c.query_row::<i64, _, _>("SELECT count(*) FROM sqlite_master WHERE name = 'ucloud_entries_old'", [], |r| r.get(0)).unwrap(), 0);
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
            // Base de la première version : `identities` sans `peer_id`, `faces` sans lien vers la fiche de contact.
            let old = Connection::open(&path).unwrap();
            let without = SCHEMA.lines().filter(|l| !["peer_id", "contact_uid", "suggest_contact_uid"].iter().any(|c| l.trim_start().starts_with(c))).collect::<Vec<_>>().join("\n");
            assert!(without.len() < SCHEMA.len(), "la colonne peer_id doit exister dans le schéma courant");
            old.execute_batch(&without).unwrap();
        }
        let conn = open(&path).unwrap();
        conn.execute("INSERT INTO identities(hex, role, peer_id) VALUES ('aa', 'node', '12D3KooWx')", []).unwrap();
        conn.execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, contact_uid, suggest_contact_uid) VALUES ('aa', 'Léa', 'h', 0, 'uid-1', NULL)", []).unwrap();
        let indexed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'faces_owner_contact')", [], |r| r.get(0)).unwrap();
        assert!(indexed, "l'index du lien visage → fiche est créé après la colonne");
        drop(conn);
        drop(open(&path).unwrap()); // seconde ouverture : rien à refaire
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_connection_keeps_temporary_tables_in_memory_and_maps_the_file_only_on_roomy_machines() {
        let tmp = testutil::TempDir::new("tuning");
        let path = tmp.path().join("uplanet.db");
        let writer = open(&path).unwrap();
        let readers = ReaderPool::open(&path, 2).unwrap();
        let pragmas = |c: &Connection| -> (i64, i64) { (c.query_row("PRAGMA temp_store", [], |r| r.get(0)).unwrap(), c.query_row("PRAGMA mmap_size", [], |r| r.get(0)).unwrap()) };
        let expected = (2, i64::try_from(mmap_budget()).unwrap()); // 2 = MEMORY
        assert_eq!(pragmas(&writer), expected, "écrivain");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for _ in 0..2 {
            let got = rt.block_on(readers.read(move |c| Ok((c.query_row("PRAGMA temp_store", [], |r| r.get::<_, i64>(0))?, c.query_row("PRAGMA mmap_size", [], |r| r.get::<_, i64>(0))?)))).unwrap();
            assert_eq!(got, expected, "lecteur");
        }
        // Projection mémoire : seulement 64 bits et au moins 4 Go ; RAM inconnue = désactivée.
        assert_eq!(mmap_budget_for(true, Some(8 << 30)), 64 << 20);
        assert_eq!(mmap_budget_for(true, Some(4 << 30)), 64 << 20);
        assert_eq!(mmap_budget_for(true, Some((4 << 30) - 1)), 0);
        assert_eq!(mmap_budget_for(false, Some(16 << 30)), 0);
        assert_eq!(mmap_budget_for(true, None), 0);
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
