use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    catalog::{Catalog, CatalogMut, ColumnDefinition, TableAlteration, TableDefinition, TableName},
    execution::{
        Executor, MaterializingExecutor, PullExecutor,
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::{InterruptHandle, QueryContext},
    storage::{TableStorage, TableStorageMut, table::Snapshot},
};
use std::{fs, sync::Arc};

#[path = "../runner/mod.rs"]
mod runner;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn alter_sql_contracts_across_indexes_optimizers_and_delivery() -> Result<()> {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/sql/alter.test");
    for indexes in [
        Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
        Arc::new(BTreeIndexFactory),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for executor in [
                Arc::new(PullExecutor) as Arc<dyn Executor>,
                Arc::new(MaterializingExecutor),
            ] {
                for batch_size in [1, 7, 2048] {
                    let db = DatabaseBuilder::new()
                        .indexes(indexes.clone())
                        .optimizer(optimizer.clone())
                        .executor(executor.clone())
                        .batch_size(batch_size)
                        .build()?;
                    assert!(runner::run_file(&db, &corpus)? >= 35);
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn catalog_alter_is_atomic_preserves_ids_and_retained_snapshots() -> Result<()> {
    let table = TableName::main("t");
    let query = QueryContext::background();
    let mut snapshot = Snapshot::default();
    snapshot.create_table(
        TableDefinition {
            name: table.clone(),
            columns: vec![
                ColumnDefinition::new("i", DataType::Integer),
                ColumnDefinition::new("j", DataType::Integer),
            ],
            unique_keys: vec![],
        },
        false,
    )?;
    snapshot.insert(
        &table,
        vec![
            vec![Value::Integer(1), Value::Null],
            vec![Value::Integer(2), Value::Integer(3)],
            vec![Value::Integer(4), Value::Integer(5)],
        ],
        &query,
    )?;
    snapshot.delete(&table, &[1], &query)?;
    let retained = snapshot.clone();
    let original = snapshot.table(&table)?;
    let original_rows = snapshot.scan(&table, &query)?;
    let invalid = [
        TableAlteration::RenameColumn {
            column: "j".into(),
            name: "i".into(),
        },
        TableAlteration::SetDefault {
            column: "i".into(),
            value: Value::Varchar("bad integer".into()),
        },
        TableAlteration::SetNullability {
            column: "j".into(),
            nullable: false,
        },
    ];
    for change in invalid {
        assert!(snapshot.alter_table(&table, &change, &query).is_err());
        assert_eq!(snapshot.table(&table)?, original);
        assert_eq!(snapshot.scan(&table, &query)?, original_rows);
    }
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
    interrupt.interrupt();
    assert!(matches!(
        snapshot.alter_table(
            &table,
            &TableAlteration::RenameTable("cancelled".into()),
            &cancelled
        ),
        Err(Error::Interrupted)
    ));
    let change = TableAlteration::AddColumn {
        column: ColumnDefinition {
            default: Value::Integer(42),
            ..ColumnDefinition::new("answer", DataType::BigInt)
        },
        if_not_exists: false,
    };
    snapshot.alter_table(&table, &change, &query)?;
    snapshot.alter_table(
        &table,
        &TableAlteration::DropColumn {
            column: "j".into(),
            if_exists: false,
        },
        &query,
    )?;
    snapshot.alter_table(
        &table,
        &TableAlteration::RenameTable("renamed".into()),
        &query,
    )?;
    let renamed = TableName::main("renamed");
    assert_eq!(snapshot.row_ids(&renamed)?, vec![0, 2]);
    assert_eq!(snapshot.next_row_id(&renamed)?, 3);
    assert_eq!(
        snapshot.scan(&renamed, &query)?,
        vec![
            (0, vec![Value::Integer(1), Value::Integer(42)]),
            (2, vec![Value::Integer(4), Value::Integer(42)])
        ]
    );
    assert_eq!(retained.table(&table)?, original);
    assert_eq!(retained.scan(&table, &query)?, original_rows);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn alter_rollback_prepared_rebinding_and_writer_conflicts() -> Result<()> {
    let db = Database::memory()?;
    let mut reader = db.connect();
    let mut writer = db.connect();
    writer.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES(1)")?;
    let prepared = writer.prepare("SELECT * FROM t")?;
    let retained = reader.query("SELECT * FROM t")?;
    reader.execute("BEGIN")?;
    writer.execute("BEGIN; ALTER TABLE t ADD COLUMN j BIGINT DEFAULT 9; ALTER TABLE t RENAME COLUMN i TO id; COMMIT")?;
    let result = writer.execute_prepared(&prepared, &[])?;
    assert_eq!(
        result
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["id", "j"]
    );
    assert_eq!(
        result.rows,
        vec![vec![Value::Integer(1), Value::Integer(9)]]
    );
    assert_eq!(reader.query("SELECT * FROM t")?.rows, retained.rows);
    reader.execute("ALTER TABLE t RENAME TO conflict")?;
    assert!(matches!(reader.execute("COMMIT"), Err(Error::Conflict)));
    writer
        .execute("BEGIN; ALTER TABLE t RENAME TO gone; ALTER TABLE gone DROP COLUMN j; ROLLBACK")?;
    assert_eq!(writer.query("SELECT * FROM t")?.rows, result.rows);
    writer.execute("BEGIN; ALTER TABLE t ADD COLUMN doomed INT")?;
    assert!(
        writer
            .execute("ALTER TABLE t ALTER COLUMN doomed SET NOT NULL")
            .is_err()
    );
    writer.execute("ROLLBACK")?;
    assert_eq!(writer.query("SELECT * FROM t")?.columns.len(), 2);
    assert_eq!(retained.rows, vec![vec![Value::Integer(1)]]);
    Ok(())
}

const MUTATIONS: &str = include_str!("../sql/alter_transactions.sql");

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn alter_with_prior_and_later_mutations_survives_wal_recovery_and_checkpoint() -> Result<()> {
    for logged in [false, true] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("alter.duckdb");
        Database::open(&path)?.connect().execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO t VALUES(1,10),(2,NULL),(3,30)")?;
        let db = if logged {
            Database::open_logged(&path)?
        } else {
            Database::open(&path)?
        };
        let mut connection = db.connect();
        connection.execute(MUTATIONS)?;
        let expected = vec![
            vec![Value::Integer(2), Value::Integer(21)],
            vec![Value::Integer(3), Value::Integer(31)],
            vec![Value::Integer(4), Value::Integer(41)],
            vec![Value::Integer(6), Value::Integer(61)],
        ];
        assert_eq!(
            connection.query("SELECT * FROM renamed ORDER BY id")?.rows,
            expected
        );
        let log_path = path.with_extension("duckdb.wal");
        let before = fs::read(&log_path).unwrap_or_default();
        connection.execute("ALTER TABLE renamed ADD COLUMN IF NOT EXISTS value BIGINT; ALTER TABLE renamed DROP COLUMN IF EXISTS absent; ALTER TABLE IF EXISTS absent RENAME TO untouched")?;
        assert_eq!(fs::read(&log_path).unwrap_or_default(), before);
        connection.execute("BEGIN; ALTER TABLE renamed RENAME TO rolled_back; ROLLBACK")?;
        assert_eq!(fs::read(&log_path).unwrap_or_default(), before);
        drop(connection);
        drop(db);
        let bytes = fs::read(&path)?;
        {
            let mut readonly = Database::open_read_only(&path)?.connect();
            assert_eq!(
                readonly.query("SELECT * FROM renamed ORDER BY id")?.rows,
                expected
            );
            assert!(
                readonly
                    .execute("ALTER TABLE renamed RENAME TO forbidden")
                    .is_err()
            );
        }
        assert_eq!(fs::read(&path)?, bytes);
        assert_eq!(fs::read(&log_path).unwrap_or_default(), before);
        let reopened = Database::open_logged(&path)?;
        let mut connection = reopened.connect();
        connection.execute("UPDATE renamed SET value=99 WHERE id=2; CHECKPOINT; ALTER TABLE renamed RENAME TO t; DELETE FROM t WHERE id=4; INSERT INTO t VALUES(7,70)")?;
        drop(connection);
        drop(reopened);
        assert_eq!(
            Database::open_read_only(&path)?
                .connect()
                .query("SELECT sum(id),sum(value) FROM t")?
                .rows,
            vec![vec![Value::Integer(18), Value::Integer(261)]]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn incomplete_alter_wal_never_exposes_a_partial_catalog_or_rows() -> Result<()> {
    use duckdb_rust::storage::{
        duckdb::{DuckDbFormat, wal::DuckDbWalRecovery},
        recovery::{Recovery, RecoveryInput},
    };
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("partial.duckdb");
    Database::open(&path)?
        .connect()
        .execute("CREATE TABLE t(i INTEGER,v INTEGER); INSERT INTO t VALUES(1,10),(2,20)")?;
    let checkpoint = fs::read(&path)?;
    {
        let db = Database::open_logged(&path)?;
        db.connect().execute("BEGIN; ALTER TABLE t ALTER COLUMN v SET NOT NULL; ALTER TABLE t ADD COLUMN x INTEGER DEFAULT 42; ALTER TABLE t RENAME TO renamed; UPDATE renamed SET v=99 WHERE i=2; COMMIT")?;
    }
    let log = fs::read(path.with_extension("duckdb.wal"))?;
    let context = QueryContext::background();
    // The complete header is eight bytes. Every later prefix lacks some part
    // of this transaction's FLUSH frame, including its length/checksum/payload.
    for cut in 8..=log.len() {
        let restored = DuckDbWalRecovery.recover(
            RecoveryInput {
                checkpoint: checkpoint.clone(),
                log: log[..cut].to_vec(),
            },
            &DuckDbFormat::default(),
            &context,
        )?;
        if cut == log.len() {
            let name = TableName::main("renamed");
            assert!(restored.table(&TableName::main("t")).is_err());
            assert_eq!(
                restored.scan(&name, &context)?,
                vec![
                    (
                        0,
                        vec![Value::Integer(1), Value::Integer(10), Value::Integer(42)]
                    ),
                    (
                        2,
                        vec![Value::Integer(2), Value::Integer(99), Value::Integer(42)]
                    )
                ]
            );
        } else {
            assert!(
                restored.table(&TableName::main("renamed")).is_err(),
                "prefix {cut}"
            );
            assert_eq!(
                restored.scan(&TableName::main("t"), &context)?,
                vec![
                    (0, vec![Value::Integer(1), Value::Integer(10)]),
                    (1, vec![Value::Integer(2), Value::Integer(20)])
                ],
                "prefix {cut}"
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn new_not_null_constraints_check_committed_and_transaction_local_rows() -> Result<()> {
    for sql in ["UPDATE t SET v=20 WHERE i=2", "DELETE FROM t WHERE i=2"] {
        let db = Database::memory()?;
        let mut connection = db.connect();
        connection
            .execute("CREATE TABLE t(i INTEGER,v INTEGER); INSERT INTO t VALUES(1,10),(2,NULL)")?;
        connection.execute(&format!("BEGIN; {sql}"))?;
        assert!(
            connection
                .execute("ALTER TABLE t ALTER COLUMN v SET NOT NULL")
                .is_err()
        );
        connection.execute("ROLLBACK")?;
        assert_eq!(
            connection.query("SELECT v FROM t ORDER BY i")?.rows,
            vec![vec![Value::Integer(10)], vec![Value::Null]]
        );
    }
    let mut connection = Database::memory()?.connect();
    connection.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES(1); BEGIN; INSERT INTO t VALUES(2); ALTER TABLE t RENAME TO renamed; ALTER TABLE renamed ALTER COLUMN i SET NOT NULL; COMMIT")?;
    assert!(
        connection
            .execute("INSERT INTO renamed VALUES(NULL)")
            .is_err()
    );
    assert_eq!(
        connection.query("SELECT * FROM renamed ORDER BY i")?.rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_duckdb_alter_wal_replays_with_both_index_adapters() -> Result<()> {
    use duckdb_rust::storage::{
        duckdb::{DuckDbFormat, wal::DuckDbWalRecovery},
        recovery::{Recovery, RecoveryInput},
    };
    use std::io::Read;
    let decode = |bytes: &[u8]| -> Result<Vec<u8>> {
        let mut output = Vec::new();
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut output)?;
        Ok(output)
    };
    let checkpoint = decode(include_bytes!("../data/alter/native.duckdb.gz"))?;
    let log = decode(include_bytes!("../data/alter/native.wal.gz"))?;
    let context = QueryContext::background();
    for indexes in [
        Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
        Arc::new(BTreeIndexFactory),
    ] {
        let snapshot = DuckDbWalRecovery
            .recover(
                RecoveryInput {
                    checkpoint: checkpoint.clone(),
                    log: log.clone(),
                },
                &DuckDbFormat::default(),
                &context,
            )?
            .with_indexes(indexes, &context)?;
        let table = TableName::main("renamed");
        let definition = snapshot.table(&table)?;
        assert_eq!(definition.columns[1].data_type, DataType::SmallInt);
        assert!(definition.columns[1].nullable);
        assert_eq!(definition.columns[1].default, Value::Null);
        assert_eq!(
            snapshot
                .scan(&table, &context)?
                .into_iter()
                .map(|(_, row)| row)
                .collect::<Vec<_>>(),
            vec![
                vec![Value::Integer(1), Value::Integer(8)],
                vec![Value::Integer(2), Value::Integer(8)],
                vec![Value::Integer(3), Value::Integer(8)],
                vec![Value::Integer(4), Value::Integer(9)],
                vec![Value::Integer(5), Value::Null],
            ]
        );
        assert_eq!(
            snapshot.lookup(&table, &[0], &vec![Value::Integer(4)], &context)?[0].1,
            vec![Value::Integer(4), Value::Integer(9)]
        );
    }
    Ok(())
}
