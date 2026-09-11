use duckdb_rust::{
    Database, DatabaseBuilder, Error, Result, Value,
    execution::index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
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
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn open(
    path: &Path,
    logged: bool,
    indexes: Arc<dyn IndexFactory>,
    faults: Option<Arc<dyn FileFaultInjector>>,
) -> Result<Database> {
    let mut storage = LocalCheckpointStorage::open(path, OpenMode::ReadWrite, || unreachable!())?;
    if let Some(faults) = faults {
        storage = storage.with_faults(faults);
    }
    let checkpoint = FileCheckpoint::new(Arc::new(storage), Arc::new(DuckDbFormat::default()))
        .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let durability: Arc<dyn Durability> = if logged {
        Arc::new(FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?)
    } else {
        Arc::new(checkpoint)
    };
    DatabaseBuilder::new()
        .durability(durability)
        .indexes(indexes)
        .build()
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn seed(path: &Path) -> Result<()> {
    Database::open(path)?.connect().execute(
        "CREATE TABLE t(i INTEGER PRIMARY KEY,s VARCHAR); INSERT INTO t VALUES(1,'base')",
    )?;
    Ok(())
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn values(path: &Path) -> Result<Vec<Vec<Value>>> {
    Ok(Database::open_read_only(path)?
        .connect()
        .query("SELECT * FROM t ORDER BY i")?
        .rows
        .into_rows())
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn log(path: &Path) -> Vec<u8> {
    fs::read(path.with_extension("duckdb.wal")).unwrap_or_default()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn logging_and_checkpoint_adapters_share_transaction_contracts() -> Result<()> {
    for logged in [false, true] {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("case.duckdb");
            seed(&path)?;
            let checkpoint = fs::read(&path)?;
            let database = open(&path, logged, indexes, None)?;
            let mut writer = database.connect();
            let mut reader = database.connect();
            reader.execute("BEGIN")?;
            assert_eq!(
                reader.query("SELECT i FROM t")?.rows,
                vec![vec![Value::Integer(1)]]
            );
            writer.execute("BEGIN; CREATE SCHEMA transient; CREATE TABLE transient.gone(i INTEGER); INSERT INTO transient.gone VALUES(1); DROP TABLE transient.gone; DROP SCHEMA transient; CREATE TABLE IF NOT EXISTS t(x INTEGER); DROP TABLE IF EXISTS absent; INSERT INTO t VALUES(2,'two'),(3,'three'); UPDATE t SET i=10-i; UPDATE t SET s=NULL WHERE i=8; DELETE FROM t WHERE i=7; INSERT INTO t VALUES(4,'four'); UPDATE t SET i=i+10 WHERE i=4; COMMIT;")?;
            writer.execute("UPDATE t SET s='remapped' WHERE i=8; DELETE FROM t WHERE i=14; INSERT INTO t VALUES(5,'five')")?;
            assert_eq!(
                reader.query("SELECT i FROM t")?.rows,
                vec![vec![Value::Integer(1)]]
            );
            reader.execute("COMMIT")?;
            let before = log(&path);
            writer.execute("CREATE SCHEMA IF NOT EXISTS main; CREATE TABLE IF NOT EXISTS t(unused BOOLEAN); DROP TABLE IF EXISTS absent_again")?;
            assert!(
                writer
                    .execute("INSERT INTO t VALUES(8,'duplicate')")
                    .is_err()
            );
            writer.execute("BEGIN; INSERT INTO t VALUES(100,'rollback'); ROLLBACK")?;
            assert_eq!(log(&path), before);
            let expected = writer.query("SELECT * FROM t ORDER BY i")?.rows;
            if logged {
                assert_eq!(fs::read(&path)?, checkpoint);
                assert!(!before.is_empty());
            }
            drop(reader);
            drop(writer);
            drop(database);
            assert_eq!(values(&path)?, expected);
            let reopened = Database::open_logged(&path)?;
            reopened
                .connect()
                .execute("UPDATE t SET s='after reopen' WHERE i=5; DELETE FROM t WHERE i=9")?;
            let expected = reopened.connect().query("SELECT * FROM t ORDER BY i")?.rows;
            drop(reopened);
            assert_eq!(values(&path)?, expected);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn chained_relocations_keep_statement_order_after_wal_recovery() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("case.duckdb");
    Database::open(&path)?.connect().execute(
        "CREATE TABLE t(i INTEGER PRIMARY KEY,s VARCHAR); \
         INSERT INTO t VALUES(1,'one'),(2,'two'),(3,'three')",
    )?;
    let expected = vec![
        vec![Value::Integer(3)],
        vec![Value::Integer(102)],
        vec![Value::Integer(110)],
    ];

    {
        let database = Database::open_logged(&path)?;
        let mut connection = database.connect();
        connection.execute(
            "BEGIN; \
             UPDATE t SET i=10 WHERE i=1; \
             UPDATE t SET i=i+100 WHERE i IN (2,10); \
             COMMIT",
        )?;
        assert_eq!(connection.query("SELECT i FROM t")?.rows, expected);
    }
    {
        let database = Database::open_logged(&path)?;
        let mut connection = database.connect();
        assert_eq!(connection.query("SELECT i FROM t")?.rows, expected);
        connection.execute("CHECKPOINT")?;
    }
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT i FROM t")?
            .rows,
        expected
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn point_commits_append_small_logs_without_rewriting_checkpoint_or_prior_records() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("case.duckdb");
    Database::open(&path)?.connect().execute("CREATE TABLE t AS SELECT range::INTEGER i, 'initial' AS s FROM range(5000); DELETE FROM t WHERE i%7=0")?;
    let checkpoint = fs::read(&path)?;
    let database = Database::open_logged(&path)?;
    database
        .connect()
        .execute("UPDATE t SET s='first' WHERE i=42+1")?;
    let first = log(&path);
    database
        .connect()
        .execute("UPDATE t SET s='second' WHERE i=43; INSERT INTO t VALUES(6000,'insert')")?;
    let second = log(&path);
    assert!(second.starts_with(&first));
    assert!(first.len() < 1024 && second.len() - first.len() < 2048);
    assert_eq!(fs::read(&path)?, checkpoint);
    drop(database);
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT s FROM t WHERE i=43")?
            .rows,
        vec![vec![Value::Varchar("second".into())]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn logged_values_preserve_float_bits_nul_strings_and_atomic_tails() -> Result<()> {
    use duckdb_rust::{
        catalog::TableName,
        parallel::QueryContext,
        storage::{
            TableStorage,
            recovery::{Recovery, RecoveryInput},
        },
    };
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("case.duckdb");
    Database::open(&path)?
        .connect()
        .execute("CREATE TABLE t(i INTEGER, f FLOAT, d DOUBLE, s VARCHAR)")?;
    let checkpoint = fs::read(&path)?;
    let database = Database::open_logged(&path)?;
    let mut connection = database.connect();
    connection.execute("INSERT INTO t VALUES(1,NULL,NULL,'committed')")?;
    let boundary = log(&path).len();
    let statement =
        connection.prepare("INSERT INTO t VALUES($1,$2::FLOAT,$3::DOUBLE,$4::VARCHAR)")?;
    let expected_f = f32::from_bits(0xffc01234);
    let expected_d = f64::from_bits(0xfff8000000004321);
    connection.execute_prepared(
        &statement,
        &[
            Value::Integer(2),
            Value::Float(expected_f),
            Value::Double(expected_d),
            Value::Varchar("a\0🦆z".into()),
        ],
    )?;
    drop(connection);
    drop(database);
    let bytes = log(&path);
    let context = QueryContext::background();
    for end in boundary..=bytes.len() {
        let snapshot = DuckDbWalRecovery.recover(
            RecoveryInput {
                checkpoint: checkpoint.clone(),
                log: bytes[..end].to_vec(),
            },
            &DuckDbFormat::default(),
            &context,
        )?;
        let rows = snapshot.scan(&TableName::main("t"), &context)?;
        assert_eq!(rows.len(), if end == bytes.len() { 2 } else { 1 });
        if end == bytes.len() {
            let Value::Float(f) = rows[1].1[1] else {
                panic!("FLOAT changed type")
            };
            let Value::Double(d) = rows[1].1[2] else {
                panic!("DOUBLE changed type")
            };
            assert_eq!(f.to_bits(), expected_f.to_bits());
            assert_eq!(d.to_bits(), expected_d.to_bits());
            assert_eq!(rows[1].1[3], Value::Varchar("a\0🦆z".into()));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn incompatible_logging_compositions_fail_before_file_changes() -> Result<()> {
    use duckdb_rust::storage::{filesystem::CheckpointStorage, format::JsonSnapshotFormat};
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("case.duckdb");
    seed(&path)?;
    let checkpoint = fs::read(&path)?;
    for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
        let storage: Arc<dyn CheckpointStorage> = Arc::new(LocalCheckpointStorage::open(
            &path,
            mode,
            || unreachable!(),
        )?);
        let missing_recovery =
            FileCheckpoint::new(storage.clone(), Arc::new(DuckDbFormat::default()));
        assert!(matches!(
            FileWal::new(missing_recovery, Arc::new(DuckDbTransactionLog)),
            Err(Error::Unsupported(_))
        ));
        let wrong_format = FileCheckpoint::new(storage, Arc::new(JsonSnapshotFormat));
        assert!(matches!(
            FileWal::new(wrong_format, Arc::new(DuckDbTransactionLog)),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(fs::read(&path)?, checkpoint);
        assert!(log(&path).is_empty());
    }
    Ok(())
}

const STEPS: &[PublicationStep] = &[
    PublicationStep::LogInitializeCreate,
    PublicationStep::LogInitializeWrite,
    PublicationStep::LogInitializeSync,
    PublicationStep::LogInitializeRename,
    PublicationStep::LogInitializeDirectorySync,
    PublicationStep::LogAppendWrite,
    PublicationStep::LogAppendSync,
    PublicationStep::LogRollbackTruncate,
    PublicationStep::LogRollbackSync,
];
struct Fault {
    selected: PublicationStep,
    enabled: AtomicBool,
    exit: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FileFaultInjector for Fault {
    fn before(&self, step: PublicationStep) -> Result<()> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Ok(());
        }
        if step == self.selected {
            if self.exit {
                std::process::exit(86);
            }
            return Err(std::io::Error::other(format!("injected {step:?}")).into());
        }
        if step == PublicationStep::LogAppendSync
            && matches!(
                self.selected,
                PublicationStep::LogRollbackTruncate | PublicationStep::LogRollbackSync
            )
        {
            return Err(std::io::Error::other("injected append failure before rollback").into());
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn logging_errors_restore_commits_or_poison_uncertain_writers() -> Result<()> {
    for (ordinal, &step) in STEPS.iter().enumerate() {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("case.duckdb");
        seed(&path)?;
        let faults = Arc::new(Fault {
            selected: step,
            enabled: AtomicBool::new(false),
            exit: false,
        });
        let database = open(
            &path,
            true,
            Arc::new(HashIndexFactory),
            Some(faults.clone()),
        )?;
        let prior_log = ordinal >= 5;
        if prior_log {
            database
                .connect()
                .execute("INSERT INTO t VALUES(0,'acknowledged in log')")?;
        }
        let before = log(&path);
        faults.enabled.store(true, Ordering::Relaxed);
        let error = database
            .connect()
            .execute("INSERT INTO t VALUES(2,'pending')")
            .unwrap_err();
        let uncertain = matches!(
            step,
            PublicationStep::LogRollbackTruncate | PublicationStep::LogRollbackSync
        );
        let maintenance = step == PublicationStep::LogInitializeDirectorySync;
        assert_eq!(
            matches!(error, Error::CommitUnknown(_)),
            uncertain,
            "{step:?}: {error}"
        );
        assert_eq!(
            matches!(error, Error::RecoveryRequired(_)),
            maintenance,
            "{step:?}: {error}"
        );
        faults.enabled.store(false, Ordering::Relaxed);
        if uncertain || maintenance {
            let blocked = database.connect().query("SELECT * FROM t").unwrap_err();
            assert_eq!(matches!(blocked, Error::CommitUnknown(_)), uncertain);
            assert_eq!(matches!(blocked, Error::RecoveryRequired(_)), maintenance);
        } else {
            assert_eq!(
                database.connect().query("SELECT i FROM t ORDER BY i")?.rows,
                if prior_log {
                    vec![vec![Value::Integer(0)], vec![Value::Integer(1)]]
                } else {
                    vec![vec![Value::Integer(1)]]
                }
            );
            if prior_log {
                assert_eq!(log(&path), before);
            }
            database
                .connect()
                .execute("INSERT INTO t VALUES(2,'pending')")?;
        }
        drop(database);
        let expected_count = usize::from(prior_log)
            + if (!uncertain && !maintenance) || step == PublicationStep::LogRollbackTruncate {
                2
            } else {
                1
            };
        assert_eq!(values(&path)?.len(), expected_count, "{step:?}");
        Database::open_logged(&path)?
            .connect()
            .execute("INSERT INTO t VALUES(3,'retry')")?;
        assert_eq!(values(&path)?.len(), expected_count + 1);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Reused by the independent oracle. All environment hooks remain test-only.
#[test]
fn logging_child() -> Result<()> {
    let Ok(path) = std::env::var("DDB_LOG_CHILD_PATH") else {
        return Ok(());
    };
    let ordinal: usize = std::env::var("DDB_LOG_CHILD_STEP")
        .unwrap()
        .parse()
        .unwrap();
    let faults = STEPS.get(ordinal).map(|&selected| {
        Arc::new(Fault {
            selected,
            enabled: AtomicBool::new(false),
            exit: true,
        })
    });
    let database = open(
        Path::new(&path),
        true,
        Arc::new(HashIndexFactory),
        faults
            .clone()
            .map(|faults| faults as Arc<dyn FileFaultInjector>),
    )?;
    if ordinal >= 5 {
        database
            .connect()
            .execute("INSERT INTO t VALUES(0,'acknowledged in log')")?;
    }
    if let Some(faults) = faults {
        faults.enabled.store(true, Ordering::Relaxed);
    }
    database
        .connect()
        .execute("INSERT INTO t VALUES(2,'pending')")?;
    if ordinal == STEPS.len() {
        std::process::exit(86);
    }
    panic!("logging boundary was not reached");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn process_interruption_during_log_initialization_append_and_rollback_is_recoverable() -> Result<()>
{
    for ordinal in 0..=STEPS.len() {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("case.duckdb");
        seed(&path)?;
        let result = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "logging_child", "--nocapture"])
            .env("DDB_LOG_CHILD_PATH", &path)
            .env("DDB_LOG_CHILD_STEP", ordinal.to_string())
            .output()?;
        assert_eq!(
            result.status.code(),
            Some(86),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let expected = usize::from(ordinal >= 5) + if [6, 7, 9].contains(&ordinal) { 2 } else { 1 };
        assert_eq!(values(&path)?.len(), expected, "boundary {ordinal}");
        Database::open_logged(&path)?
            .connect()
            .execute("INSERT INTO t VALUES(3,'retry')")?;
        assert_eq!(values(&path)?.len(), expected + 1);
    }
    Ok(())
}
