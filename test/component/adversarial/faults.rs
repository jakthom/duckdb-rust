use duckdb_rust::{
    Database, DatabaseBuilder, Error, Result, Value,
    storage::{
        checkpoint::{Durability, FileCheckpoint},
        duckdb::{
            DuckDbFormat,
            wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
        },
        filesystem::{FileFaultInjector, LocalCheckpointStorage, OpenMode, PublicationStep},
        logged::FileWal,
    },
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct FailAfter {
    ordinal: usize,
    persistent: bool,
    calls: AtomicUsize,
    fired: AtomicBool,
}
impl FileFaultInjector for FailAfter {
    fn before(&self, _: PublicationStep) -> Result<()> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.ordinal || (self.persistent && call > self.ordinal) {
            self.fired.store(true, Ordering::SeqCst);
            return Err(std::io::Error::other("generated I/O failure").into());
        }
        Ok(())
    }
}

fn open(path: &Path, logged: bool, faults: Arc<FailAfter>) -> Result<Database> {
    let storage = LocalCheckpointStorage::open(path, OpenMode::ReadWrite, || unreachable!())?
        .with_faults(faults);
    let checkpoint = FileCheckpoint::new(Arc::new(storage), Arc::new(DuckDbFormat::default()))
        .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let durability: Arc<dyn Durability> = if logged {
        Arc::new(FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?)
    } else {
        Arc::new(checkpoint)
    };
    DatabaseBuilder::new().durability(durability).build()
}

#[test]
fn every_observed_io_position_handles_once_and_persistent_failures() -> Result<()> {
    for logged in [false, true] {
        for persistent in [false, true] {
            let mut completed = false;
            for ordinal in 1..=64 {
                let directory = tempfile::tempdir()?;
                let path = directory.path().join("case.duckdb");
                Database::open(&path)?
                    .connect()
                    .execute("CREATE TABLE t(i INTEGER PRIMARY KEY); INSERT INTO t VALUES(1)")?;
                let faults = Arc::new(FailAfter {
                    ordinal,
                    persistent,
                    calls: AtomicUsize::new(0),
                    fired: AtomicBool::new(false),
                });
                let outcome = open(&path, logged, faults.clone())
                    .and_then(|db| db.connect().execute("INSERT INTO t VALUES(2)").map(|_| ()));
                let recovered = Database::open(&path)?;
                let rows = recovered
                    .connect()
                    .query("SELECT i FROM t ORDER BY i")?
                    .rows;
                let before = vec![vec![Value::Integer(1)]];
                let after = vec![vec![Value::Integer(1)], vec![Value::Integer(2)]];
                match outcome {
                    Ok(()) => assert_eq!(rows, after),
                    Err(Error::CommitUnknown(_)) => assert!(rows == before || rows == after),
                    Err(_) => assert_eq!(
                        rows, before,
                        "logged={logged}, persistent={persistent}, ordinal={ordinal}"
                    ),
                }
                recovered.connect().execute("INSERT INTO t VALUES(3)")?;
                drop(recovered);
                assert_eq!(
                    Database::open_read_only(&path)?
                        .connect()
                        .query("SELECT count(*) FROM t WHERE i=3")?
                        .rows,
                    vec![vec![Value::Integer(1)]]
                );
                if !faults.fired.load(Ordering::SeqCst) {
                    assert!(ordinal > 1);
                    completed = true;
                    break;
                }
            }
            assert!(
                completed,
                "fault sweep never reached an uninjected completion"
            );
        }
    }
    Ok(())
}
