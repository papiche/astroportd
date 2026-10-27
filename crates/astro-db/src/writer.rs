//! Acteur d'écriture unique.
//!
//! Un thread dédié possède l'unique connexion en écriture et consomme une file `tokio::sync::mpsc` bornée.
//! Chaque message est appliqué dans sa propre transaction : l'écriture est sérialisée, jamais de `SQLITE_BUSY`
//! entre écrivains, et la file bornée applique la contre-pression aux producteurs.

use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{Connection, Transaction, params, params_from_iter};
use tokio::sync::{mpsc, oneshot};

use crate::DbError;

/// Capacité de la file de commandes.
pub const QUEUE_CAPACITY: usize = 1024;

/// Requête SQL paramétrée.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// SQL avec paramètres positionnels `?1`, `?2`…
    pub sql: String,
    /// Valeurs liées.
    pub params: Vec<Value>,
}

impl Statement {
    /// Construit une requête paramétrée.
    pub fn new(sql: impl Into<String>, params: Vec<Value>) -> Self {
        Self { sql: sql.into(), params }
    }
}

/// Tables vectorielles `sqlite-vec` du schéma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorTable {
    /// `face_vectors` : 512 dimensions.
    Face,
    /// `memory_vectors` : 1024 dimensions (bge-m3).
    Memory,
    /// `ucloud_desc_vectors` : 1024 dimensions (bge-m3).
    UcloudDesc,
    /// `dream_vectors` : 768 dimensions.
    Dream,
}

impl VectorTable {
    /// Nom de la table virtuelle.
    pub fn table(self) -> &'static str {
        match self {
            Self::Face => "face_vectors",
            Self::Memory => "memory_vectors",
            Self::UcloudDesc => "ucloud_desc_vectors",
            Self::Dream => "dream_vectors",
        }
    }

    /// Colonne de clé primaire.
    pub fn id_column(self) -> &'static str {
        match self {
            Self::Face => "face_id",
            Self::Memory => "mem_id",
            Self::UcloudDesc => "emb_id",
            Self::Dream => "dream_id",
        }
    }

    /// Dimension des vecteurs.
    pub fn dim(self) -> usize {
        match self {
            Self::Face => 512,
            Self::Memory | Self::UcloudDesc => 1024,
            Self::Dream => 768,
        }
    }

    /// Valide et sérialise un vecteur en `f32` petit-boutiste.
    pub fn encode(self, v: &[f32]) -> Result<Vec<u8>, DbError> {
        if v.len() != self.dim() {
            return Err(DbError::DimensionMismatch { table: self.table(), expected: self.dim(), got: v.len() });
        }
        if v.iter().any(|x| !x.is_finite()) {
            return Err(DbError::NonFiniteVector);
        }
        Ok(v.iter().flat_map(|x| x.to_le_bytes()).collect())
    }
}

/// Résultat d'une écriture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    /// Lignes modifiées.
    pub changes: usize,
    /// Dernier `rowid` inséré sur la connexion d'écriture.
    pub last_insert_rowid: i64,
}

type Reply<T> = oneshot::Sender<Result<T, DbError>>;
type Job = Box<dyn FnOnce(&mut Connection) + Send>;

enum Command {
    Execute { stmt: Statement, reply: Reply<WriteOutcome> },
    Batch { stmts: Vec<Statement>, reply: Reply<WriteOutcome> },
    UpsertVector { table: VectorTable, id: i64, owner_hex: String, blob: Vec<u8>, reply: Reply<()> },
    DeleteVector { table: VectorTable, id: i64, reply: Reply<()> },
    VacuumInto { dest: PathBuf, reply: Reply<()> },
    Job(Job),
    Shutdown { reply: oneshot::Sender<()> },
}

/// Poignée de l'acteur d'écriture. Cloner pour la partager entre tâches.
#[derive(Clone)]
pub struct Writer {
    tx: mpsc::Sender<Command>,
}

impl Writer {
    /// Lance le thread d'écriture, propriétaire de `conn`.
    pub(crate) fn spawn(conn: Connection) -> Result<Self, DbError> {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        std::thread::Builder::new().name("astro-db-writer".into()).spawn(move || run(conn, rx))?;
        Ok(Self { tx })
    }

    async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(make(reply)).await.map_err(|_| DbError::WriterClosed)?;
        rx.await.map_err(|_| DbError::WriterClosed)?
    }

    /// Exécute une requête d'écriture (`INSERT`, `UPDATE`, `DELETE`…).
    pub async fn execute(&self, sql: impl Into<String>, params: Vec<Value>) -> Result<WriteOutcome, DbError> {
        let stmt = Statement::new(sql, params);
        self.send(|reply| Command::Execute { stmt, reply }).await
    }

    /// Exécute plusieurs requêtes dans **une seule transaction** (tout ou rien).
    pub async fn batch(&self, stmts: Vec<Statement>) -> Result<WriteOutcome, DbError> {
        self.send(|reply| Command::Batch { stmts, reply }).await
    }

    /// Insère ou remplace le vecteur `id` de `owner_hex`. La dimension est vérifiée avant l'envoi.
    pub async fn upsert_vector(
        &self,
        table: VectorTable,
        id: i64,
        owner_hex: &str,
        embedding: &[f32],
    ) -> Result<(), DbError> {
        let blob = table.encode(embedding)?;
        let owner_hex = owner_hex.to_owned();
        self.send(|reply| Command::UpsertVector { table, id, owner_hex, blob, reply }).await
    }

    /// Supprime le vecteur `id`.
    pub async fn delete_vector(&self, table: VectorTable, id: i64) -> Result<(), DbError> {
        self.send(|reply| Command::DeleteVector { table, id, reply }).await
    }

    /// Exécute `f` dans une transaction sur la connexion d'écriture ; `Err` annule tout.
    ///
    /// Sert aux écritures qui mêlent lecture et écriture (lecture-modification-écriture atomique).
    pub async fn transaction<T, F>(&self, f: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T, DbError> + Send + 'static,
    {
        let (result_tx, result_rx) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let out = (|| {
                let tx = conn.transaction()?;
                let value = f(&tx)?;
                tx.commit()?;
                Ok(value)
            })();
            let _ = result_tx.send(out);
        });
        self.tx.send(Command::Job(job)).await.map_err(|_| DbError::WriterClosed)?;
        result_rx.await.map_err(|_| DbError::WriterClosed)?
    }

    /// Sauvegarde atomique : `VACUUM INTO` vers un fichier temporaire voisin, `fsync`, puis `rename`.
    ///
    /// L'acteur d'écriture est occupé pendant la copie (les autres écritures attendent dans la file) ;
    /// les lecteurs, eux, continuent de servir. `dest` est remplacé atomiquement s'il existe.
    pub async fn vacuum_into(&self, dest: &Path) -> Result<(), DbError> {
        let dest = dest.to_owned();
        self.send(|reply| Command::VacuumInto { dest, reply }).await
    }

    /// Termine proprement : applique les commandes déjà en file, ferme la connexion, puis rend la main.
    pub async fn shutdown(&self) -> Result<(), DbError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Command::Shutdown { reply }).await.map_err(|_| DbError::WriterClosed)?;
        rx.await.map_err(|_| DbError::WriterClosed)
    }
}

fn run(mut conn: Connection, mut rx: mpsc::Receiver<Command>) {
    let mut shutdown_reply = None;
    while let Some(cmd) = rx.blocking_recv() {
        if let Command::Shutdown { reply } = cmd {
            shutdown_reply = Some(reply);
            break;
        }
        // Une panique dans une commande ne doit pas tuer l'acteur : le demandeur reçoit `WriterClosed`
        // (son canal de réponse est détruit), les suivants sont traités normalement.
        let _ = catch_unwind(AssertUnwindSafe(|| apply(&mut conn, cmd)));
    }
    drop(conn);
    if let Some(reply) = shutdown_reply {
        let _ = reply.send(());
    }
}

fn apply(conn: &mut Connection, cmd: Command) {
    match cmd {
        Command::Execute { stmt, reply } => {
            let _ = reply.send(in_tx(conn, |tx| run_statement(tx, &stmt)));
        }
        Command::Batch { stmts, reply } => {
            let _ = reply.send(in_tx(conn, |tx| apply_statements(tx, &stmts)));
        }
        Command::UpsertVector { table, id, owner_hex, blob, reply } => {
            let _ = reply.send(in_tx(conn, |tx| {
                let (t, c) = (table.table(), table.id_column());
                tx.execute(&format!("DELETE FROM {t} WHERE {c} = ?1"), params![id])?;
                tx.execute(&format!("INSERT INTO {t}({c}, owner_hex, embedding) VALUES (?1, ?2, ?3)"), params![id, owner_hex, blob])?;
                Ok(())
            }));
        }
        Command::DeleteVector { table, id, reply } => {
            let _ = reply.send(in_tx(conn, |tx| {
                tx.execute(&format!("DELETE FROM {} WHERE {} = ?1", table.table(), table.id_column()), params![id])?;
                Ok(())
            }));
        }
        Command::VacuumInto { dest, reply } => {
            let _ = reply.send(vacuum_atomic(conn, &dest));
        }
        Command::Job(job) => job(conn),
        Command::Shutdown { .. } => {}
    }
}

fn in_tx<T>(conn: &mut Connection, f: impl FnOnce(&Transaction<'_>) -> Result<T, DbError>) -> Result<T, DbError> {
    let tx = conn.transaction()?;
    let value = f(&tx)?;
    tx.commit()?;
    Ok(value)
}

/// Exécute des requêtes dans une transaction ouverte (pour composer des étapes dans [`Writer::transaction`]).
pub fn apply_statements(tx: &Transaction<'_>, stmts: &[Statement]) -> Result<WriteOutcome, DbError> {
    let mut total = WriteOutcome::default();
    for s in stmts {
        let o = run_statement(tx, s)?;
        total.changes += o.changes;
        total.last_insert_rowid = o.last_insert_rowid;
    }
    Ok(total)
}

fn run_statement(tx: &Transaction<'_>, s: &Statement) -> Result<WriteOutcome, DbError> {
    let changes = tx.prepare_cached(&s.sql)?.execute(params_from_iter(s.params.iter()))?;
    Ok(WriteOutcome { changes, last_insert_rowid: tx.last_insert_rowid() })
}

fn vacuum_atomic(conn: &Connection, dest: &Path) -> Result<(), DbError> {
    let mut partial = dest.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let partial_str = partial.to_str().ok_or_else(|| DbError::BadPath(partial.display().to_string()))?;
    match std::fs::remove_file(&partial) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let copied = conn.execute("VACUUM INTO ?1", params![partial_str]).map_err(DbError::from).and_then(|_| {
        File::open(&partial)?.sync_all()?;
        std::fs::rename(&partial, dest)?;
        Ok(())
    });
    if copied.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    copied
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testutil::TempDir;
    use crate::Db;

    fn text(s: &str) -> Value {
        Value::Text(s.to_owned())
    }

    fn unit(dim: usize, hot: usize) -> Vec<f32> {
        (0..dim).map(|i| if i == hot { 1.0 } else { 0.0 }).collect()
    }

    fn open_db(tmp: &TempDir) -> Db {
        Db::open(&tmp.path().join("uplanet.db"), 4).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_concurrent_writers_never_hit_busy() {
        let tmp = TempDir::new("busy");
        let db = Arc::new(open_db(&tmp));
        let mut tasks = Vec::new();
        for t in 0..8 {
            let db = db.clone();
            tasks.push(tokio::spawn(async move {
                for i in 0..100 {
                    db.writer()
                        .execute(
                            "INSERT INTO kv(scope, key, value, updated_at) VALUES ('t', ?1, 'v', 0)",
                            vec![text(&format!("{t}-{i}"))],
                        )
                        .await
                        .unwrap();
                }
            }));
        }
        // Lectures concurrentes pendant les écritures.
        let reader = {
            let db = db.clone();
            tokio::spawn(async move {
                let mut last = 0;
                for _ in 0..50 {
                    let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM kv", [], |r| r.get(0))?)).await.unwrap();
                    assert!(n >= last, "le compte ne décroît jamais");
                    last = n;
                    tokio::task::yield_now().await;
                }
            })
        };
        for t in tasks {
            t.await.unwrap();
        }
        reader.await.unwrap();
        let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM kv", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 800);
    }

    #[tokio::test]
    async fn batch_is_all_or_nothing() {
        let tmp = TempDir::new("batch");
        let db = open_db(&tmp);
        let ok = Statement::new("INSERT INTO kv(scope,key,value,updated_at) VALUES ('b','1','v',0)", vec![]);
        let dup = ok.clone(); // clé primaire en double : échoue
        let r = db.writer().batch(vec![ok, dup]).await;
        assert!(r.is_err());
        let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM kv WHERE scope='b'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 0, "la première insertion doit être annulée");
    }

    #[tokio::test]
    async fn transaction_rolls_back_on_error_and_returns_values() {
        let tmp = TempDir::new("tx");
        let db = open_db(&tmp);
        let r: Result<(), DbError> = db
            .writer()
            .transaction(|tx| {
                tx.execute("INSERT INTO kv(scope,key,value,updated_at) VALUES ('x','1','v',0)", [])?;
                Err(DbError::NonFiniteVector)
            })
            .await;
        assert!(matches!(r, Err(DbError::NonFiniteVector)));
        let v = db
            .writer()
            .transaction(|tx| {
                tx.execute("INSERT INTO kv(scope,key,value,updated_at) VALUES ('x','2','v',0)", [])?;
                Ok(tx.query_row("SELECT count(*) FROM kv WHERE scope='x'", [], |r| r.get::<_, i64>(0))?)
            })
            .await
            .unwrap();
        assert_eq!(v, 1);
    }

    #[tokio::test]
    async fn readers_cannot_write() {
        let tmp = TempDir::new("ro");
        let db = open_db(&tmp);
        let r = db.readers().read(|c| Ok(c.execute("INSERT INTO kv(scope,key,value,updated_at) VALUES ('r','1','v',0)", [])?)).await;
        assert!(matches!(r, Err(DbError::Sqlite(_))));
    }

    #[tokio::test]
    async fn vector_upsert_replaces_and_knn_is_partitioned() {
        let tmp = TempDir::new("vec");
        let db = open_db(&tmp);
        let w = db.writer();
        w.upsert_vector(VectorTable::Face, 1, "alice", &unit(512, 0)).await.unwrap();
        w.upsert_vector(VectorTable::Face, 2, "alice", &unit(512, 1)).await.unwrap();
        w.upsert_vector(VectorTable::Face, 3, "bob", &unit(512, 0)).await.unwrap();
        // Remplacement : le visage 1 devient orthogonal à la requête.
        w.upsert_vector(VectorTable::Face, 1, "alice", &unit(512, 2)).await.unwrap();
        let hits = db.readers().knn(VectorTable::Face, "alice", &unit(512, 1), 5).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, 2);
        assert!(hits[0].1.abs() < 1e-6);
        assert!(hits[1].1 > 0.9);
        w.delete_vector(VectorTable::Face, 2).await.unwrap();
        let hits = db.readers().knn(VectorTable::Face, "alice", &unit(512, 1), 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        let bob = db.readers().knn(VectorTable::Face, "bob", &unit(512, 0), 5).await.unwrap();
        assert_eq!(bob.len(), 1);
    }

    #[tokio::test]
    async fn bad_vectors_are_rejected_before_reaching_the_writer() {
        let tmp = TempDir::new("badvec");
        let db = open_db(&tmp);
        let r = db.writer().upsert_vector(VectorTable::Memory, 1, "a", &unit(768, 0)).await;
        assert!(matches!(r, Err(DbError::DimensionMismatch { expected: 1024, got: 768, .. })));
        let mut v = unit(768, 0);
        v[3] = f32::NAN;
        assert!(matches!(db.writer().upsert_vector(VectorTable::Dream, 1, "a", &v).await, Err(DbError::NonFiniteVector)));
        let r = db.readers().knn(VectorTable::Face, "a", &unit(10, 0), 1).await;
        assert!(matches!(r, Err(DbError::DimensionMismatch { .. })));
    }

    #[tokio::test]
    async fn vacuum_into_makes_a_consistent_atomic_copy_while_readers_keep_reading() {
        let tmp = TempDir::new("vacuum");
        let db = Arc::new(open_db(&tmp));
        for i in 0..200 {
            db.writer().execute("INSERT INTO kv(scope,key,value,updated_at) VALUES ('v', ?1, 'x', 0)", vec![text(&i.to_string())]).await.unwrap();
        }
        db.writer().upsert_vector(VectorTable::Dream, 7, "z", &unit(768, 5)).await.unwrap();
        let dest = tmp.path().join("snapshots").join("daily.db");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();

        let reads = {
            let db = db.clone();
            tokio::spawn(async move {
                for _ in 0..30 {
                    db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM kv", [], |r| r.get::<_, i64>(0))?)).await.unwrap();
                }
            })
        };
        db.writer().vacuum_into(&dest).await.unwrap();
        reads.await.unwrap();

        assert!(!dest.with_extension("db.partial").exists(), "pas de fichier partiel résiduel");
        let copy = crate::open(&dest).unwrap();
        let n: i64 = copy.query_row("SELECT count(*) FROM kv WHERE scope='v'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 200);
        let d: i64 = copy.query_row("SELECT count(*) FROM dream_vectors", [], |r| r.get(0)).unwrap();
        assert_eq!(d, 1);
        let check: String = copy.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(check, "ok");

        // Un second instantané remplace le premier.
        db.writer().execute("INSERT INTO kv(scope,key,value,updated_at) VALUES ('v','extra','x',0)", vec![]).await.unwrap();
        db.writer().vacuum_into(&dest).await.unwrap();
        let again = crate::open(&dest).unwrap();
        let n: i64 = again.query_row("SELECT count(*) FROM kv WHERE scope='v'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 201);
    }

    #[tokio::test]
    async fn failed_vacuum_leaves_no_partial_file() {
        let tmp = TempDir::new("vacfail");
        let db = open_db(&tmp);
        let dest = tmp.path().join("missing-dir").join("snap.db");
        assert!(db.writer().vacuum_into(&dest).await.is_err());
        assert!(!tmp.path().join("missing-dir").exists());
    }

    #[tokio::test]
    async fn shutdown_flushes_queued_writes_then_rejects_new_ones() {
        let tmp = TempDir::new("shutdown");
        let path = tmp.path().join("uplanet.db");
        let db = Db::open(&path, 1).unwrap();
        let w = db.writer().clone();
        let pending: Vec<_> = (0..50)
            .map(|i| {
                let w = w.clone();
                tokio::spawn(async move {
                    w.execute("INSERT INTO kv(scope,key,value,updated_at) VALUES ('s', ?1, 'x', 0)", vec![Value::Integer(i)]).await
                })
            })
            .collect();
        for p in pending {
            p.await.unwrap().unwrap();
        }
        w.shutdown().await.unwrap();
        assert!(matches!(w.execute("SELECT 1", vec![]).await, Err(DbError::WriterClosed)));
        let n: i64 = crate::open(&path).unwrap().query_row("SELECT count(*) FROM kv WHERE scope='s'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 50);
    }

    #[tokio::test]
    #[allow(clippy::panic)] // panique volontaire : on vérifie que l'acteur survit
    async fn a_panicking_job_does_not_kill_the_writer() {
        let tmp = TempDir::new("panic");
        let db = open_db(&tmp);
        let r: Result<(), DbError> = db.writer().transaction(|_| panic!("boom")).await;
        assert!(matches!(r, Err(DbError::WriterClosed)));
        db.writer().execute("INSERT INTO kv(scope,key,value,updated_at) VALUES ('p','1','v',0)", vec![]).await.unwrap();
    }

    #[test]
    fn zero_readers_is_refused() {
        let tmp = TempDir::new("zero");
        assert!(matches!(Db::open(&tmp.path().join("x.db"), 0), Err(DbError::NoReaders)));
    }
}
