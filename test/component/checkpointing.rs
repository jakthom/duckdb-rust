use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    catalog::{
        Catalog, ColumnDefinition, TableAlteration, TableName,
        expression::{
            StoredArgumentStyle, StoredExpression, StoredExpressionEvaluator, StoredExpressionKind,
        },
    },
    execution::index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
    parallel::{InterruptHandle, QueryContext},
    storage::{
        UpdateMetadata,
        checkpoint::{
            FileCheckpoint,
            policy::{CheckpointPolicy, CommitCountCheckpoint, LogSizeCheckpoint},
        },
        duckdb::{
            DuckDbFormat,
            wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
        },
        filesystem::{FileFaultInjector, LocalCheckpointStorage, OpenMode, PublicationStep},
        format::SnapshotFormat,
        logged::FileWal,
    },
    transaction::{SnapshotTransactions, TransactionManager},
};
use std::{
    fs,
    num::NonZeroU64,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[path = "checkpoint_exact.rs"]
mod exact;
#[path = "checkpoint_publication.rs"]
mod publication;
#[path = "checkpoint_versions.rs"]
mod versions;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn seed(path: &Path) -> Result<()> {
    Database::open(path)?.connect().execute(
        "CREATE TABLE t(i INTEGER PRIMARY KEY,s VARCHAR); INSERT INTO t VALUES(1,'base')",
    )?;
    Ok(())
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn open(
    path: &Path,
    policy: Option<Arc<dyn CheckpointPolicy>>,
    indexes: Arc<dyn IndexFactory>,
    faults: Option<Arc<dyn FileFaultInjector>>,
) -> Result<(Database, Arc<SnapshotTransactions>)> {
    let mut storage = LocalCheckpointStorage::open(path, OpenMode::ReadWrite, || unreachable!())?;
    if let Some(faults) = faults {
        storage = storage.with_faults(faults);
    }
    let checkpoint = FileCheckpoint::new(Arc::new(storage), Arc::new(DuckDbFormat::default()))
        .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let wal =
        FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?.with_checkpoint_policy(policy);
    let transactions = Arc::new(SnapshotTransactions::with_indexes(Arc::new(wal), indexes)?);
    Ok((
        DatabaseBuilder::new()
            .transactions(transactions.clone())
            .build()?,
        transactions,
    ))
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn count(path: &Path) -> Result<i128> {
    let result = Database::open_read_only(path)?
        .connect()
        .query("SELECT count(*) FROM t")?;
    Ok(result.rows[0][0].as_i128().unwrap())
}

struct CountPhysicalDefaults(AtomicUsize);

impl StoredExpressionEvaluator for CountPhysicalDefaults {
    fn evaluate(
        &self,
        _expression: &StoredExpression,
        _target: &DataType,
        _catalog: &dyn Catalog,
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        Ok(Value::Integer(
            self.0.fetch_add(1, Ordering::SeqCst) as i128 + 1,
        ))
    }
}

fn counted_default() -> StoredExpression {
    StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Cast {
            expression: Box::new(StoredExpression::literal(
                DataType::Integer,
                Value::Integer(1),
            )),
            target: DataType::Integer,
            try_cast: false,
        },
    }
}

fn effect_default() -> StoredExpression {
    StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Function {
            name: vec!["count_physical_defaults".into()],
            arguments: vec![],
            is_operator: false,
            argument_style: StoredArgumentStyle::Named,
        },
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn wal_recovery_preserves_regular_and_relocated_update_order() -> Result<()> {
    use duckdb_rust::{
        catalog::{CatalogMut, TableDefinition, UniqueKey},
        storage::{
            TableStorage, TableStorageMut, UpdateMetadata,
            log::{TransactionChange, TransactionLog},
            recovery::{Recovery, RecoveryInput},
            table::Snapshot,
        },
    };

    let recover = |indexed: bool| -> Result<(Vec<(u64, Vec<Value>)>, usize)> {
        let table = TableName::main("update_order");
        let context = QueryContext::background();
        let mut original = Snapshot::default();
        let mut id = ColumnDefinition::new("id", DataType::Integer);
        id.nullable = !indexed;
        original.create_table(
            TableDefinition {
                name: table.clone(),
                columns: vec![id, ColumnDefinition::new("v", DataType::Integer)],
                unique_keys: indexed
                    .then(|| UniqueKey {
                        columns: vec![0],
                        primary: true,
                    })
                    .into_iter()
                    .collect(),
            },
            false,
        )?;
        original.insert(
            &table,
            vec![
                vec![Value::Integer(1), Value::Integer(10)],
                vec![Value::Integer(2), Value::Integer(20)],
                vec![Value::Integer(3), Value::Integer(30)],
            ],
            &context,
        )?;
        let format = DuckDbFormat::default();
        let checkpoint = format.encode(&original)?;
        let start = DuckDbTransactionLog.start(&original, &context)?;
        let columns = if indexed { vec![0] } else { vec![1] };
        let metadata = UpdateMetadata::for_table(&original.table(&table)?, columns)?;
        let row = if indexed {
            vec![Value::Integer(10), Value::Integer(10)]
        } else {
            vec![Value::Integer(1), Value::Integer(11)]
        };
        let append = start.session.prepare(
            &[TransactionChange::Update {
                table: table.clone(),
                metadata,
                rows: vec![(0, row)],
            }],
            &context,
        )?;
        let mut log = start.header;
        log.extend(append.bytes);
        let mut recovered =
            DuckDbWalRecovery.recover(RecoveryInput { checkpoint, log }, &format, &context)?;
        let calls = Arc::new(CountPhysicalDefaults(AtomicUsize::new(0)));
        let effect_context = context.with_stored_expressions(calls.clone());
        recovered.alter_table(
            &table,
            &TableAlteration::AddColumn {
                column: ColumnDefinition::new("observed", DataType::Integer)
                    .with_default(effect_default()),
                if_not_exists: false,
            },
            &effect_context,
        )?;
        Ok((
            recovered.scan(&table, &effect_context)?,
            calls.0.load(Ordering::SeqCst),
        ))
    };

    let (regular, calls) = recover(false)?;
    assert_eq!(calls, 3);
    assert_eq!(
        regular,
        vec![
            (
                0,
                vec![Value::Integer(1), Value::Integer(11), Value::Integer(1)]
            ),
            (
                1,
                vec![Value::Integer(2), Value::Integer(20), Value::Integer(2)]
            ),
            (
                2,
                vec![Value::Integer(3), Value::Integer(30), Value::Integer(3)]
            ),
        ]
    );

    let (relocated, calls) = recover(true)?;
    assert_eq!(calls, 3);
    assert_eq!(
        relocated,
        vec![
            (
                1,
                vec![Value::Integer(2), Value::Integer(20), Value::Integer(1)]
            ),
            (
                2,
                vec![Value::Integer(3), Value::Integer(30), Value::Integer(2)]
            ),
            (
                3,
                vec![Value::Integer(10), Value::Integer(10), Value::Integer(3)]
            ),
        ]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn automatic_policy_reclaims_only_checkpointed_basis_slots() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("automatic_reclamation.duckdb");
    seed(&path)?;
    let (database, transactions) = open(
        &path,
        Some(Arc::new(CommitCountCheckpoint(NonZeroU64::new(1).unwrap()))),
        Arc::new(HashIndexFactory),
        None,
    )?;
    let mut connection = database.connect();
    connection.execute(
        "BEGIN; INSERT INTO t VALUES(2,'deleted'),(3,'live'); \
         DELETE FROM t WHERE i IN (1,2); COMMIT",
    )?;
    let mut retained = transactions.begin()?;
    connection.execute(
        "BEGIN; INSERT INTO t VALUES(4,'incoming'); DELETE FROM t WHERE i=3; \
         ALTER TABLE t RENAME TO renamed; COMMIT",
    )?;

    let calls = Arc::new(CountPhysicalDefaults(AtomicUsize::new(0)));
    let query = QueryContext::background().with_stored_expressions(calls.clone());
    retained.catalog_mut()?.alter_table(
        &TableName::main("t"),
        &TableAlteration::AddColumn {
            column: ColumnDefinition::new("old_demand", DataType::Integer)
                .with_default(counted_default()),
            if_not_exists: false,
        },
        &query,
    )?;
    assert_eq!(calls.0.load(Ordering::SeqCst), 3);
    drop(retained);

    calls.0.store(0, Ordering::SeqCst);
    let mut current = transactions.begin()?;
    let renamed = TableName::main("renamed");
    current.catalog_mut()?.alter_table(
        &renamed,
        &TableAlteration::AddColumn {
            column: ColumnDefinition::new("current_demand", DataType::Integer)
                .with_default(counted_default()),
            if_not_exists: false,
        },
        &query,
    )?;
    assert_eq!(calls.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        current.storage().scan(&renamed, &query)?,
        vec![(
            3,
            vec![
                Value::Integer(4),
                Value::Varchar("incoming".into()),
                Value::Integer(2)
            ]
        )]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn checkpoints_preserve_live_ids_readers_and_pending_writers_across_policies() -> Result<()> {
    let policies: Vec<Option<Arc<dyn CheckpointPolicy>>> = vec![
        None,
        Some(Arc::new(LogSizeCheckpoint(NonZeroU64::new(1).unwrap()))),
        Some(Arc::new(CommitCountCheckpoint(NonZeroU64::new(1).unwrap()))),
    ];
    for policy in policies {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("case.duckdb");
            seed(&path)?;
            let (database, transactions) = open(&path, policy.clone(), indexes, None)?;
            let mut reader = database.connect();
            reader.execute("BEGIN")?;
            assert_eq!(
                reader.query("SELECT i FROM t")?.rows,
                vec![vec![Value::Integer(1)]]
            );
            let mut writer = database.connect();
            writer.execute("INSERT INTO t VALUES(2,'two'),(3,'three'); UPDATE t SET i=i+10; DELETE FROM t WHERE i=12")?;
            let table = TableName::main("t");
            let context = QueryContext::background();
            let before = transactions.begin()?.storage().scan(&table, &context)?;
            assert_eq!(
                before.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                vec![0, 2]
            );
            let mut pending = transactions.begin()?;
            let update = UpdateMetadata::for_table(&pending.catalog().table(&table)?, vec![0, 1])?;
            pending.storage_mut()?.update(
                &table,
                &update,
                vec![(
                    0,
                    vec![Value::Integer(11), Value::Varchar("pending writer".into())],
                )],
                &context,
            )?;
            writer.execute("CHECKPOINT")?;
            assert!(!path.with_extension("duckdb.wal").exists());
            assert_eq!(
                transactions.begin()?.storage().scan(&table, &context)?,
                before
            );
            let physical = DuckDbFormat::default().decode(
                fs::read(&path)?,
                duckdb_rust::common::type_registry::builtin_types(),
            )?;
            assert_eq!(physical.row_ids(&table)?, vec![0, 1]);
            pending.commit()?;
            assert_eq!(
                writer.query("SELECT s FROM t WHERE i=11")?.rows,
                vec![vec![Value::Varchar("pending writer".into())]]
            );
            assert_eq!(
                reader.query("SELECT i FROM t")?.rows,
                vec![vec![Value::Integer(1)]]
            );
            reader.execute("COMMIT")?;
            writer.execute(
                "DELETE FROM t; CHECKPOINT; INSERT INTO t VALUES(99,'after empty checkpoint')",
            )?;
            assert_eq!(
                transactions.begin()?.storage().scan(&table, &context)?[0].0,
                3
            );
            writer.checkpoint()?;
            drop(reader);
            drop(writer);
            drop(database);
            drop(transactions);
            assert_eq!(count(&path)?, 1);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn checkpoint_syntax_session_and_cancellation_contracts() -> Result<()> {
    let mut memory = Database::memory()?.connect();
    let results = memory.execute(
        ";; SELECT 'CHECKPOINT; still a string'; /* checkpoint */ CHECKPOINT; -- next\n SELECT 42;",
    )?;
    assert_eq!(results.len(), 3);
    assert!(memory.execute("CHECKPOINT SELECT 1").is_err());
    assert!(memory.execute("\"CHECKPOINT\"").is_err());
    let prepared = memory.prepare("checkpoint")?;
    memory.execute_prepared(&prepared, &[])?;
    memory.execute_plan(duckdb_rust::planner::BoundStatement::Checkpoint)?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("case.duckdb");
    seed(&path)?;
    let (database, transactions) = open(&path, None, Arc::new(HashIndexFactory), None)?;
    let mut connection = database.connect();
    connection.execute("BEGIN; INSERT INTO t VALUES(2,'uncommitted')")?;
    for sql in ["CHECKPOINT", "checkpoint;"] {
        assert!(matches!(
            connection.execute(sql),
            Err(Error::Transaction(_))
        ));
    }
    assert!(matches!(
        connection.checkpoint(),
        Err(Error::Transaction(_))
    ));
    assert_eq!(
        connection.query("SELECT count(*) FROM t")?.rows,
        vec![vec![Value::Integer(2)]]
    );
    connection.execute("ROLLBACK; INSERT INTO t VALUES(2,'committed')")?;
    let checkpoint = fs::read(&path)?;
    let log = fs::read(path.with_extension("duckdb.wal"))?;
    let interrupt = InterruptHandle::default();
    interrupt.interrupt();
    let context = QueryContext::new(interrupt, None, 32, 100)?;
    assert!(matches!(
        transactions.checkpoint(&context),
        Err(Error::Interrupted)
    ));
    let context = QueryContext::new(InterruptHandle::default(), None, 1, 1)?;
    assert!(matches!(
        transactions.checkpoint(&context),
        Err(Error::Resource(_))
    ));
    assert_eq!(fs::read(&path)?, checkpoint);
    assert_eq!(fs::read(path.with_extension("duckdb.wal"))?, log);
    connection.checkpoint()?;
    drop(connection);
    drop(database);
    drop(transactions);
    let mut readonly = Database::open_read_only(&path)?.connect();
    assert!(matches!(readonly.checkpoint(), Err(Error::Unsupported(_))));
    assert_eq!(
        readonly.query("SELECT count(*) FROM t")?.rows,
        vec![vec![Value::Integer(2)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn policies_control_real_checkpoint_publication() -> Result<()> {
    let policies: Vec<(Option<Arc<dyn CheckpointPolicy>>, usize)> = vec![
        (None, 0),
        (
            Some(Arc::new(LogSizeCheckpoint(NonZeroU64::new(1).unwrap()))),
            2,
        ),
        (
            Some(Arc::new(CommitCountCheckpoint(NonZeroU64::new(2).unwrap()))),
            3,
        ),
    ];
    for (policy, trigger) in policies {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("case.duckdb");
        seed(&path)?;
        let original = fs::read(&path)?;
        let (database, _) = open(&path, policy, Arc::new(HashIndexFactory), None)?;
        for commit in 1..=3 {
            database
                .connect()
                .execute(&format!("INSERT INTO t VALUES({},'logged')", commit + 1))?;
            if trigger == 0 || commit < trigger {
                assert_eq!(fs::read(&path)?, original);
            }
            if commit == trigger {
                let physical = DuckDbFormat::default().decode(
                    fs::read(&path)?,
                    duckdb_rust::common::type_registry::builtin_types(),
                )?;
                // The incoming transaction remains in the WAL; only earlier
                // acknowledged transactions belong to this checkpoint.
                assert_eq!(physical.row_ids(&TableName::main("t"))?.len(), commit);
            }
        }
        database.connect().checkpoint()?;
        assert!(!path.with_extension("duckdb.wal").exists());
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn checkpoint_layouts_preserve_duplicate_rows_nan_bits_and_reject_aliases() -> Result<()> {
    use duckdb_rust::{
        catalog::{CatalogMut, ColumnDefinition, TableDefinition},
        common::DataType,
        storage::{TableStorageMut, table::Snapshot},
    };
    let mut snapshot = Snapshot::default();
    let mut column = ColumnDefinition::new("f", DataType::Float);
    column.default = Some(duckdb_rust::catalog::expression::StoredExpression::literal(
        DataType::Float,
        Value::Float(f32::from_bits(0xffc01234)),
    ));
    let table = TableName::main("duplicate_nan_rows");
    snapshot.create_table(
        TableDefinition {
            name: table.clone(),
            columns: vec![column],
            unique_keys: vec![],
        },
        false,
    )?;
    let context = QueryContext::background();
    snapshot.insert(
        &table,
        vec![vec![Value::Float(f32::from_bits(0xffc01234))]; 3],
        &context,
    )?;
    snapshot.delete(&table, &[1], &context)?;
    let format = DuckDbFormat::default();
    let image = format.encode_successor(&snapshot, &format.encode(&snapshot)?)?;
    let mut decoded = format.decode(
        image.bytes,
        duckdb_rust::common::type_registry::builtin_types(),
    )?;
    let update = UpdateMetadata::for_table(&decoded.table(&table)?, vec![0])?;
    snapshot.validate_checkpoint_layout(&decoded, &image.layout, &context)?;
    for fault in 0..4 {
        let mut bad = image.layout.clone();
        let mapping = bad.tables.get_mut(&table).unwrap();
        match fault {
            0 => {
                mapping.rows.remove(&0);
            }
            1 => {
                mapping.rows.insert(2, 0);
            }
            2 => {
                mapping.rows.insert(0, 99);
            }
            _ => mapping.next_row_id += 1,
        }
        assert!(matches!(
            snapshot.validate_checkpoint_layout(&decoded, &bad, &context),
            Err(Error::Internal(_))
        ));
    }
    decoded.update(
        &table,
        &update,
        vec![(0, vec![Value::Float(f32::from_bits(0xffc01235))])],
        &context,
    )?;
    assert!(matches!(
        snapshot.validate_checkpoint_layout(&decoded, &image.layout, &context),
        Err(Error::Internal(_))
    ));
    Ok(())
}

struct BadLayout {
    invalid: Arc<AtomicBool>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotFormat for BadLayout {
    fn name(&self) -> &'static str {
        "fault-injected-checkpoint-layout"
    }
    fn format_id(&self) -> duckdb_rust::storage::format::FormatId {
        duckdb_rust::storage::format::DUCKDB_FORMAT
    }
    fn decode(
        &self,
        bytes: Vec<u8>,
        types: Arc<duckdb_rust::common::type_registry::TypeRegistry>,
    ) -> Result<duckdb_rust::storage::table::Snapshot> {
        DuckDbFormat::default().decode(bytes, types)
    }
    fn encode(&self, snapshot: &duckdb_rust::storage::table::Snapshot) -> Result<Vec<u8>> {
        DuckDbFormat::default().encode(snapshot)
    }
    fn supports_successor(&self) -> bool {
        true
    }
    fn encode_successor(
        &self,
        snapshot: &duckdb_rust::storage::table::Snapshot,
        previous: &[u8],
    ) -> Result<duckdb_rust::storage::layout::CheckpointImage> {
        let mut image = DuckDbFormat::default().encode_successor(snapshot, previous)?;
        if self.invalid.load(Ordering::Relaxed) {
            image.layout.tables.clear();
        }
        Ok(image)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn invalid_format_layout_fails_before_io_and_keeps_the_session_usable() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("case.duckdb");
    seed(&path)?;
    let invalid = Arc::new(AtomicBool::new(true));
    let checkpoint = FileCheckpoint::open(
        &path,
        OpenMode::ReadWrite,
        Arc::new(BadLayout {
            invalid: invalid.clone(),
        }),
    )?
    .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let wal = FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?;
    let database = DatabaseBuilder::new().durability(Arc::new(wal)).build()?;
    database
        .connect()
        .execute("INSERT INTO t VALUES(2,'acknowledged')")?;
    let checkpoint = fs::read(&path)?;
    let log = fs::read(path.with_extension("duckdb.wal"))?;
    assert!(matches!(
        database.connect().checkpoint(),
        Err(Error::Internal(_))
    ));
    assert_eq!(fs::read(&path)?, checkpoint);
    assert_eq!(fs::read(path.with_extension("duckdb.wal"))?, log);
    database
        .connect()
        .execute("INSERT INTO t VALUES(3,'still usable')")?;
    invalid.store(false, Ordering::Relaxed);
    database.connect().checkpoint()?;
    drop(database);
    assert_eq!(count(&path)?, 3);
    Ok(())
}

const STEPS: &[PublicationStep] = &[
    PublicationStep::CheckpointCreate,
    PublicationStep::CheckpointWrite,
    PublicationStep::CheckpointSync,
    PublicationStep::RecoveryLogCreate,
    PublicationStep::RecoveryLogWrite,
    PublicationStep::RecoveryLogSync,
    PublicationStep::RecoveryLogRename,
    PublicationStep::RecoveryLogDirectorySync,
    PublicationStep::CheckpointRename,
    PublicationStep::CheckpointDirectorySync,
    PublicationStep::LogRemove,
    PublicationStep::LogRetirementDirectorySync,
];
struct Fault {
    step: PublicationStep,
    armed: AtomicBool,
    exit: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FileFaultInjector for Fault {
    fn before(&self, step: PublicationStep) -> Result<()> {
        if self.armed.load(Ordering::Relaxed) && self.step == step {
            if self.exit {
                std::process::exit(86);
            }
            return Err(std::io::Error::other(format!("injected {step:?}")).into());
        }
        Ok(())
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn policy(automatic: bool) -> Option<Arc<dyn CheckpointPolicy>> {
    automatic.then(|| {
        Arc::new(CommitCountCheckpoint(NonZeroU64::new(1).unwrap())) as Arc<dyn CheckpointPolicy>
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn checkpoint_retires_a_header_only_log_after_rejected_first_append() -> Result<()> {
    for step in [
        PublicationStep::LogAppendWrite,
        PublicationStep::LogAppendSync,
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("case.duckdb");
        seed(&path)?;
        let before = fs::read(&path)?;
        let faults = Arc::new(Fault {
            step,
            armed: AtomicBool::new(true),
            exit: false,
        });
        let (database, transactions) = open(
            &path,
            None,
            Arc::new(HashIndexFactory),
            Some(faults.clone()),
        )?;
        assert!(matches!(
            database
                .connect()
                .execute("INSERT INTO t VALUES(2,'rejected')"),
            Err(Error::Io(_))
        ));
        assert!(path.with_extension("duckdb.wal").exists());
        faults.armed.store(false, Ordering::Relaxed);
        database.connect().checkpoint()?;
        assert!(!path.with_extension("duckdb.wal").exists());
        assert_eq!(fs::read(&path)?, before);
        assert_eq!(
            database.connect().query("SELECT i FROM t")?.rows,
            vec![vec![Value::Integer(1)]]
        );
        database
            .connect()
            .execute("INSERT INTO t VALUES(2,'retry')")?;
        drop(database);
        drop(transactions);
        assert_eq!(count(&path)?, 2);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn interrupted_checkpoint_publication_does_not_commit_the_incoming_transaction() -> Result<()> {
    for automatic in [false, true] {
        for &step in STEPS {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("case.duckdb");
            seed(&path)?;
            let faults = Arc::new(Fault {
                step,
                armed: AtomicBool::new(false),
                exit: false,
            });
            let (database, transactions) = open(
                &path,
                policy(automatic),
                Arc::new(HashIndexFactory),
                Some(faults.clone()),
            )?;
            database
                .connect()
                .execute("INSERT INTO t VALUES(2,'acknowledged')")?;
            faults.armed.store(true, Ordering::Relaxed);
            let error = if automatic {
                database
                    .connect()
                    .execute("INSERT INTO t VALUES(3,'incoming')")
                    .map(|_| ())
            } else {
                database.connect().checkpoint()
            }
            .unwrap_err();
            assert!(
                matches!(error, Error::RecoveryRequired(_)),
                "{step:?}: {error}"
            );
            assert!(matches!(
                database.connect().query("SELECT * FROM t"),
                Err(Error::RecoveryRequired(_))
            ));
            drop(database);
            drop(transactions);
            assert_eq!(count(&path)?, 2);
            Database::open_logged(&path)?
                .connect()
                .execute("INSERT INTO t VALUES(3,'retry')")?;
            assert_eq!(count(&path)?, 3);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn checkpoint_child() -> Result<()> {
    let Ok(path) = std::env::var("DDB_CHECKPOINT_CHILD_PATH") else {
        return Ok(());
    };
    let ordinal: usize = std::env::var("DDB_CHECKPOINT_CHILD_STEP")
        .unwrap()
        .parse()
        .unwrap();
    let automatic = std::env::var("DDB_CHECKPOINT_CHILD_AUTO").unwrap() == "1";
    let faults = STEPS.get(ordinal).map(|&step| {
        Arc::new(Fault {
            step,
            armed: AtomicBool::new(false),
            exit: true,
        })
    });
    let (database, _) = open(
        Path::new(&path),
        policy(automatic),
        Arc::new(HashIndexFactory),
        faults.clone().map(|f| f as Arc<dyn FileFaultInjector>),
    )?;
    database
        .connect()
        .execute("INSERT INTO t VALUES(2,'acknowledged')")?;
    if let Some(faults) = faults {
        faults.armed.store(true, Ordering::Relaxed);
    }
    if automatic {
        database
            .connect()
            .execute("INSERT INTO t VALUES(3,'incoming')")?;
    } else {
        database.connect().checkpoint()?;
    }
    if ordinal == STEPS.len() {
        std::process::exit(86);
    }
    panic!("checkpoint boundary was not reached");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn process_exits_during_manual_and_automatic_checkpoints_preserve_acknowledged_work() -> Result<()>
{
    for automatic in [false, true] {
        for ordinal in 0..=STEPS.len() {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("case.duckdb");
            seed(&path)?;
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", "checkpoint_child", "--nocapture"])
                .env("DDB_CHECKPOINT_CHILD_PATH", &path)
                .env("DDB_CHECKPOINT_CHILD_STEP", ordinal.to_string())
                .env(
                    "DDB_CHECKPOINT_CHILD_AUTO",
                    if automatic { "1" } else { "0" },
                )
                .output()?;
            assert_eq!(
                output.status.code(),
                Some(86),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let expected = if automatic && ordinal == STEPS.len() {
                3
            } else {
                2
            };
            assert_eq!(count(&path)?, expected);
            Database::open_logged(&path)?
                .connect()
                .execute("CHECKPOINT; INSERT INTO t VALUES(4,'retry')")?;
            assert_eq!(count(&path)?, expected + 1);
        }
    }
    Ok(())
}
