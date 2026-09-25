use super::*;
use duckdb_rust::storage::{
    duckdb::{
        DuckDbFormat,
        wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
    },
    format::{DUCKDB_FORMAT, StorageVersion},
    log::{LogAppend, LogCheckpoint, LogSession, LogStart, TransactionChange, TransactionLog},
    logged::FileWal,
};
use std::sync::Mutex;

type Events = Arc<Mutex<Vec<(&'static str, Value)>>>;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn observe(events: &Events, phase: &'static str, query: &QueryContext) -> Result<()> {
    // Metadata availability only: log encoding must not reevaluate defaults.
    query.stored_expressions()?;
    events
        .lock()
        .unwrap()
        .push((phase, query.settings().get("default_order", query)?.clone()));
    Ok(())
}

struct ContextLog(Events);
struct ContextSession(Box<dyn LogSession>, Events);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn wrap(start: LogStart, events: &Events) -> LogStart {
    LogStart {
        header: start.header,
        session: Box::new(ContextSession(start.session, events.clone())),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TransactionLog for ContextLog {
    fn name(&self) -> &'static str {
        "context-log"
    }
    fn format_id(&self) -> FormatId {
        DUCKDB_FORMAT
    }
    fn start(&self, snapshot: &Snapshot, query: &QueryContext) -> Result<LogStart> {
        self.start_at(snapshot, None, query)
    }
    fn start_at(
        &self,
        snapshot: &Snapshot,
        version: Option<StorageVersion>,
        query: &QueryContext,
    ) -> Result<LogStart> {
        observe(&self.0, "start", query)?;
        Ok(wrap(
            DuckDbTransactionLog.start_at(snapshot, version, query)?,
            &self.0,
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl LogSession for ContextSession {
    fn prepare(&self, changes: &[TransactionChange], query: &QueryContext) -> Result<LogAppend> {
        observe(&self.1, "prepare", query)?;
        let append = self.0.prepare(changes, query)?;
        Ok(LogAppend {
            bytes: append.bytes,
            next: Box::new(Self(append.next, self.1.clone())),
        })
    }
    fn rebase(&self, checkpoint: LogCheckpoint<'_>, query: &QueryContext) -> Result<LogStart> {
        observe(&self.1, "rebase", query)?;
        Ok(wrap(self.0.rebase(checkpoint, query)?, &self.1))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn wal_context_survives_commit_manual_checkpoint_rollback_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("context.duckdb");
    let events = Arc::new(Mutex::new(Vec::new()));
    let checkpoint = FileCheckpoint::open(
        &path,
        OpenMode::ReadWrite,
        Arc::new(DuckDbFormat::default()),
    )?
    .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let wal = FileWal::new(checkpoint, Arc::new(ContextLog(events.clone())))?
        .with_checkpoint_policy(None);
    let database = DatabaseBuilder::new().durability(Arc::new(wal)).build()?;
    let mut connection = database.connect();
    connection.execute("CREATE TABLE t(i INTEGER PRIMARY KEY); INSERT INTO t VALUES (1)")?;
    connection.execute("SET SESSION default_order='DESC'; CHECKPOINT")?;
    connection.execute("BEGIN; INSERT INTO t VALUES(2); ROLLBACK; INSERT INTO t VALUES(3)")?;
    assert_eq!(
        connection.query("SELECT i FROM t ORDER BY i ASC")?.rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );
    let observed = events.lock().unwrap().clone();
    assert_eq!(
        observed
            .iter()
            .filter(|(phase, _)| *phase == "start")
            .count(),
        1
    );
    assert_eq!(
        observed
            .iter()
            .filter(|(phase, _)| *phase == "prepare")
            .count(),
        3
    );
    assert_eq!(
        observed
            .iter()
            .filter(|(phase, _)| *phase == "rebase")
            .count(),
        1
    );
    for (phase, setting) in observed {
        // Commits use retained startup context, explicit maintenance its caller.
        assert_eq!(
            setting,
            Value::Varchar(
                if phase == "rebase" {
                    "DESC"
                } else {
                    "ASCENDING"
                }
                .into()
            )
        );
    }
    drop(connection);
    drop(database);
    let database = Database::open_logged(&path)?;
    assert_eq!(
        database
            .connect()
            .query("SELECT i FROM t ORDER BY i ASC")?
            .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );
    Ok(())
}
