use super::*;
use duckdb_rust::storage::checkpoint::policy::CommitCountCheckpoint;
use std::num::NonZeroU64;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn clean_reopen_preserves_prefix_and_physical_ids_through_later_mutations() -> Result<()> {
    for indexes in [
        Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
        Arc::new(BTreeIndexFactory),
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("resume.duckdb");
        seed(&path)?;
        let checkpoint = fs::read(&path)?;
        let expected;
        {
            let database = open(&path, true, indexes.clone(), None)?;
            let mut connection = database.connect();
            connection.execute("INSERT INTO t VALUES(2,'two'),(3,'three'); DELETE FROM t WHERE i=2; UPDATE t SET i=10 WHERE i=1; CREATE VIEW visible AS SELECT i,s FROM t")?;
            expected = connection.query("SELECT * FROM visible ORDER BY i")?.rows;
        }
        let prefix = log(&path);
        assert!(!prefix.is_empty());
        assert_eq!(fs::read(&path)?, checkpoint);
        {
            let database = open(&path, true, indexes.clone(), None)?;
            assert_eq!(fs::read(&path)?, checkpoint);
            assert_eq!(
                log(&path),
                prefix,
                "clean reopen must not publish a checkpoint"
            );
            let mut connection = database.connect();
            assert_eq!(
                connection.query("SELECT * FROM visible ORDER BY i")?.rows,
                expected
            );
            connection.execute("BEGIN; UPDATE t SET i=i+100 WHERE i=10; UPDATE t SET s='resumed' WHERE i=110; DELETE FROM t WHERE i=3; INSERT INTO t VALUES(4,'new'); COMMIT")?;
            assert!(log(&path).starts_with(&prefix));
            assert_eq!(fs::read(&path)?, checkpoint);
        }
        let after = log(&path);
        {
            let database = open(&path, true, indexes, None)?;
            assert_eq!(log(&path), after);
            let mut connection = database.connect();
            assert_eq!(
                connection.query("SELECT * FROM visible ORDER BY i")?.rows,
                vec![
                    vec![Value::Integer(4), Value::Varchar("new".into())],
                    vec![Value::Integer(110), Value::Varchar("resumed".into())]
                ]
            );
            connection.execute("CHECKPOINT; UPDATE t SET i=5 WHERE i=4; INSERT INTO t VALUES(6,'after checkpoint')")?;
        }
        assert_eq!(
            values(&path)?,
            vec![
                vec![Value::Integer(5), Value::Varchar("new".into())],
                vec![Value::Integer(6), Value::Varchar("after checkpoint".into())],
                vec![Value::Integer(110), Value::Varchar("resumed".into())],
            ]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn resumed_commits_count_toward_the_existing_checkpoint_policy() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("policy.duckdb");
    seed(&path)?;
    {
        let database = Database::open_logged(&path)?;
        database
            .connect()
            .execute("INSERT INTO t VALUES(2,'two'); INSERT INTO t VALUES(3,'three')")?;
    }
    let checkpoint_bytes = fs::read(&path)?;
    let prefix = log(&path);
    let storage = LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || unreachable!())?;
    let checkpoint = FileCheckpoint::new(Arc::new(storage), Arc::new(DuckDbFormat::default()))
        .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let durability = FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?
        .with_checkpoint_policy(Some(Arc::new(CommitCountCheckpoint(
            NonZeroU64::new(2).unwrap(),
        ))));
    let database = DatabaseBuilder::new()
        .durability(Arc::new(durability))
        .build()?;
    assert_eq!(log(&path), prefix);
    assert_eq!(fs::read(&path)?, checkpoint_bytes);
    database
        .connect()
        .execute("INSERT INTO t VALUES(4,'four')")?;
    assert_ne!(
        fs::read(&path)?,
        checkpoint_bytes,
        "the third commit must checkpoint two resumed commits"
    );
    drop(database);
    assert_eq!(values(&path)?.len(), 4);
    Ok(())
}

struct LegacyRecovery;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::storage::recovery::Recovery for LegacyRecovery {
    fn name(&self) -> &'static str {
        "legacy-recovery"
    }
    fn format_id(&self) -> duckdb_rust::storage::format::FormatId {
        duckdb_rust::storage::format::DUCKDB_FORMAT
    }
    fn supports_preparation(&self) -> bool {
        true
    }
    fn recover(
        &self,
        input: duckdb_rust::storage::recovery::RecoveryInput,
        format: &dyn duckdb_rust::storage::format::SnapshotFormat,
        context: &duckdb_rust::parallel::QueryContext,
    ) -> Result<duckdb_rust::storage::table::Snapshot> {
        duckdb_rust::storage::recovery::Recovery::recover(
            &DuckDbWalRecovery,
            input,
            format,
            context,
        )
    }
    fn prepare(
        &self,
        input: duckdb_rust::storage::recovery::RecoveryInput,
        format: &dyn duckdb_rust::storage::format::SnapshotFormat,
        context: &duckdb_rust::parallel::QueryContext,
    ) -> Result<duckdb_rust::storage::recovery::PreparedRecovery> {
        duckdb_rust::storage::recovery::Recovery::prepare(
            &DuckDbWalRecovery,
            input,
            format,
            context,
        )
    }
}
struct LegacyLog;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::storage::log::TransactionLog for LegacyLog {
    fn name(&self) -> &'static str {
        "legacy-log"
    }
    fn format_id(&self) -> duckdb_rust::storage::format::FormatId {
        duckdb_rust::storage::format::DUCKDB_FORMAT
    }
    fn start(
        &self,
        snapshot: &duckdb_rust::storage::table::Snapshot,
        context: &duckdb_rust::parallel::QueryContext,
    ) -> Result<duckdb_rust::storage::log::LogStart> {
        duckdb_rust::storage::log::TransactionLog::start(&DuckDbTransactionLog, snapshot, context)
    }
    fn start_at(
        &self,
        snapshot: &duckdb_rust::storage::table::Snapshot,
        version: Option<duckdb_rust::storage::format::StorageVersion>,
        context: &duckdb_rust::parallel::QueryContext,
    ) -> Result<duckdb_rust::storage::log::LogStart> {
        duckdb_rust::storage::log::TransactionLog::start_at(
            &DuckDbTransactionLog,
            snapshot,
            version,
            context,
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn either_legacy_adapter_retains_eager_recovery_fallback() -> Result<()> {
    for legacy_recovery in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("legacy.duckdb");
        seed(&path)?;
        Database::open_logged(&path)?
            .connect()
            .execute("INSERT INTO t VALUES(2,'two')")?;
        let before = fs::read(&path)?;
        assert!(!log(&path).is_empty());
        let storage = LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || unreachable!())?;
        let recovery: Arc<dyn duckdb_rust::storage::recovery::Recovery> = if legacy_recovery {
            Arc::new(LegacyRecovery)
        } else {
            Arc::new(DuckDbWalRecovery)
        };
        let encoder: Arc<dyn duckdb_rust::storage::log::TransactionLog> = if legacy_recovery {
            Arc::new(DuckDbTransactionLog)
        } else {
            Arc::new(LegacyLog)
        };
        let checkpoint = FileCheckpoint::new(Arc::new(storage), Arc::new(DuckDbFormat::default()))
            .with_recovery(recovery)?;
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileWal::new(checkpoint, encoder)?))
            .build()?;
        assert!(
            log(&path).is_empty(),
            "declined resume must use prior recovery publication"
        );
        assert_ne!(fs::read(&path)?, before);
        database
            .connect()
            .execute("INSERT INTO t VALUES(3,'after fallback')")?;
        drop(database);
        assert_eq!(values(&path)?.len(), 3);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn continuation_accepts_only_complete_committed_prefixes_and_retains_limits() -> Result<()> {
    use duckdb_rust::{
        catalog::TableName,
        parallel::QueryContext,
        storage::{
            log::{TransactionChange, TransactionLog},
            recovery::{Recovery, RecoveryInput},
        },
    };
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("tails.duckdb");
    seed(&path)?;
    let checkpoint = fs::read(&path)?;
    let first;
    {
        let database = Database::open_logged(&path)?;
        database
            .connect()
            .execute("INSERT INTO t VALUES(2,'two')")?;
        first = log(&path).len();
        database
            .connect()
            .execute("UPDATE t SET s='second' WHERE i=2")?;
    }
    let bytes = log(&path);
    let context = QueryContext::background();
    let format = DuckDbFormat::default();
    for end in 0..=bytes.len() {
        let resumed = Recovery::resume(
            &DuckDbWalRecovery,
            RecoveryInput {
                checkpoint: checkpoint.clone(),
                log: bytes[..end].to_vec(),
            },
            &format,
            &context,
        );
        if end == first || end == bytes.len() {
            let result = resumed?.expect("complete committed prefix must resume");
            assert_eq!(result.resume.length, end as u64);
            assert_eq!(result.resume.commits, if end == first { 1 } else { 2 });
            assert!(result.resume.entries >= result.resume.commits as usize);
        } else {
            assert!(
                !matches!(resumed, Ok(Some(_))),
                "uncommitted or partial prefix {end} resumed"
            );
        }
    }
    let interrupt = duckdb_rust::parallel::InterruptHandle::default();
    interrupt.interrupt();
    let cancelled = QueryContext::new(interrupt, None, 64, 10_000)?;
    assert!(matches!(
        Recovery::resume(
            &DuckDbWalRecovery,
            RecoveryInput {
                checkpoint: checkpoint.clone(),
                log: bytes.clone(),
            },
            &format,
            &cancelled
        ),
        Err(Error::Interrupted)
    ));
    let recovered = Recovery::resume(
        &DuckDbWalRecovery,
        RecoveryInput {
            checkpoint: checkpoint.clone(),
            log: bytes.clone(),
        },
        &format,
        &context,
    )?
    .unwrap();
    let mut metadata = recovered.resume;
    metadata.entries = 1_000_000;
    let start = TransactionLog::resume(
        &DuckDbTransactionLog,
        &recovered.snapshot,
        &metadata,
        &context,
    )?
    .unwrap();
    let change = TransactionChange::Insert {
        table: TableName::new("main", "t"),
        rows: vec![vec![Value::Integer(3), Value::Varchar("limit".into())]],
    };
    assert!(matches!(
        start.session.prepare(&[change], &context),
        Err(Error::Resource(_))
    ));
    metadata.entries = 1_000_001;
    assert!(
        TransactionLog::resume(
            &DuckDbTransactionLog,
            &recovered.snapshot,
            &metadata,
            &context
        )
        .is_err()
    );
    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(matches!(
        Recovery::resume(
            &DuckDbWalRecovery,
            RecoveryInput {
                checkpoint,
                log: corrupt,
            },
            &format,
            &context
        ),
        Err(Error::Corrupt(_))
    ));
    assert_eq!(log(&path), bytes);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn incomplete_logs_take_publication_fallback_before_later_writes() -> Result<()> {
    for committed_second in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fallback.duckdb");
        seed(&path)?;
        {
            let database = Database::open_logged(&path)?;
            database
                .connect()
                .execute("INSERT INTO t VALUES(2,'two'); INSERT INTO t VALUES(3,'three')")?;
        }
        let mut bytes = log(&path);
        if committed_second {
            bytes.push(0);
        } else {
            bytes.truncate(bytes.len() - 21);
        }
        fs::write(path.with_extension("duckdb.wal"), &bytes)?;
        let expected = if committed_second { 3 } else { 2 };
        let database = Database::open_logged(&path)?;
        assert!(
            log(&path).is_empty(),
            "torn/uncommitted tail must use prior publication fallback"
        );
        assert_eq!(
            database.connect().query("SELECT count(*) FROM t")?.rows,
            vec![vec![Value::Integer(expected)]]
        );
        database
            .connect()
            .execute("INSERT INTO t VALUES(4,'after fallback')")?;
        drop(database);
        assert_eq!(values(&path)?.len(), expected as usize + 1);
    }
    Ok(())
}

struct CountReads {
    storage: LocalCheckpointStorage,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::storage::filesystem::CheckpointStorage for CountReads {
    fn name(&self) -> &'static str {
        "count-checkpoint-reads"
    }
    fn writable(&self) -> bool {
        self.storage.writable()
    }
    fn read(&self) -> Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.storage.read()
    }
    fn read_log(&self) -> Result<Vec<u8>> {
        self.storage.read_log()
    }
    fn replace(&self, bytes: &[u8]) -> Result<()> {
        self.storage.replace(bytes)
    }
    fn supports_recovery_publication(&self) -> bool {
        true
    }
    fn supports_log_append(&self) -> bool {
        true
    }
    fn initialize_log(&self, header: &[u8]) -> Result<u64> {
        self.storage.initialize_log(header)
    }
    fn initialize_log_transaction(&self, header: &[u8], transaction: &[u8]) -> Result<Option<u64>> {
        self.storage.initialize_log_transaction(header, transaction)
    }
    fn append_log(&self, expected: u64, bytes: &[u8]) -> Result<u64> {
        self.storage.append_log(expected, bytes)
    }
    fn publish_recovery(
        &self,
        basis: &duckdb_rust::storage::recovery::RecoveryInput,
        publication: &duckdb_rust::storage::recovery::RecoveryPublication,
    ) -> Result<()> {
        self.storage.publish_recovery(basis, publication)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn empty_wal_startup_reads_the_checkpoint_once() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("read-once.duckdb");
    seed(&path)?;
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let storage = CountReads {
        storage: LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || unreachable!())?,
        reads: reads.clone(),
    };
    let checkpoint = FileCheckpoint::new(Arc::new(storage), Arc::new(DuckDbFormat::default()))
        .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let database = DatabaseBuilder::new()
        .durability(Arc::new(FileWal::new(
            checkpoint,
            Arc::new(DuckDbTransactionLog),
        )?))
        .build()?;
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    database
        .connect()
        .execute("INSERT INTO t VALUES(2,'after empty log')")?;
    drop(database);
    assert_eq!(values(&path)?.len(), 2);
    Ok(())
}
