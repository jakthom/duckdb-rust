//! Existing file compatibility, not fresh-file preferences, owns WAL type gates.
use super::*;
use duckdb_rust::{
    common::type_registry::builtin_types,
    storage::{format::StorageVersion, table::Snapshot},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_log_retains_actual_version_through_typed_mutations_checkpoint_and_reopen() -> Result<()> {
    for version in [64, 68, 69] {
        for preference in [64, 69] {
            for indexes in [
                Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
                Arc::new(BTreeIndexFactory),
            ] {
                let directory = tempfile::tempdir()?;
                let path = directory.path().join("versioned_log.duckdb");
                let initial = DuckDbFormat::default()
                    .with_storage_version(version)?
                    .encode(&Snapshot::default())?;
                let storage = Arc::new(LocalCheckpointStorage::open(
                    &path,
                    OpenMode::ReadWrite,
                    || Ok(initial.clone()),
                )?);
                // Deliberately disagree with the image in half the cases.
                let selected = Arc::new(DuckDbFormat::default().with_storage_version(preference)?);
                let checkpoint = FileCheckpoint::new(storage, selected.clone())
                    .with_recovery(Arc::new(DuckDbWalRecovery))?;
                let wal = FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?
                    .with_checkpoint_policy(super::policy(preference == 69));
                let transactions =
                    Arc::new(SnapshotTransactions::with_indexes(Arc::new(wal), indexes)?);
                let database = DatabaseBuilder::new()
                    .transactions(transactions.clone())
                    .build()?;
                let mut c = database.connect();
                c.execute("CREATE TABLE base(i INTEGER PRIMARY KEY); INSERT INTO base VALUES(1)")?;
                let before = fs::read(&path)?;
                let log = fs::read(path.with_extension("duckdb.wal"))?;
                if version < 69 {
                    for sql in [
                        "CREATE TABLE t(i INTEGER PRIMARY KEY,v TUPLE(INTEGER,DECIMAL(12,2),TIMESTAMP_NS,INTEGER[]))",
                        "CREATE TABLE e AS SELECT row() AS r,struct_pack() AS s",
                        "ALTER TABLE base ADD COLUMN v TUPLE(INTEGER)[]",
                    ] {
                        assert!(
                            matches!(c.execute(sql), Err(Error::InvalidInput(_))),
                            "version {version}, preference {preference}: {sql}"
                        );
                        assert!(fs::read(&path)? == before);
                        assert!(fs::read(path.with_extension("duckdb.wal"))? == log);
                    }
                    c.execute("UPDATE base SET i=2; CHECKPOINT; INSERT INTO base VALUES(3)")?;
                } else {
                    c.execute("CREATE TABLE t(i INTEGER PRIMARY KEY,v TUPLE(INTEGER,DECIMAL(12,2),TIMESTAMP_NS,INTEGER[])); INSERT INTO t VALUES(1,row(7,1.25::DECIMAL(12,2),'2024-02-29 12:34:56.123456789'::TIMESTAMP_NS,[1,NULL])),(2,NULL); CREATE TABLE e AS SELECT row() AS r,struct_pack() AS s")?;
                    let value = c.query("SELECT v FROM t WHERE i=1")?.rows[0][0].clone();
                    let prepared = c.prepare("UPDATE t SET v=$1 WHERE i=$2")?;
                    let before_rollback = fs::read(path.with_extension("duckdb.wal"))?;
                    c.execute("BEGIN; DELETE FROM t; DROP TABLE e; ROLLBACK")?;
                    assert!(fs::read(path.with_extension("duckdb.wal"))? == before_rollback);
                    c.execute_prepared(&prepared, &[value.clone(), Value::Integer(2)])?;
                    assert_eq!(
                        c.query("SELECT count(*),count(v) FROM t")?.rows,
                        vec![vec![Value::Integer(2), Value::Integer(2)]]
                    );
                    c.execute("DELETE FROM t WHERE i=1; CHECKPOINT; UPDATE t SET i=12 WHERE i=2; ALTER TABLE base ADD COLUMN v TUPLE(INTEGER)[]; UPDATE base SET v=[row(9)]; CHECKPOINT; INSERT INTO t VALUES(22,NULL)")?;
                    assert_eq!(c.query("SELECT a.i,b.i,row_number() OVER(ORDER BY a.i) FROM t a JOIN t b ON a.v=b.v")?.rows, vec![vec![Value::Integer(12),Value::Integer(12),Value::Integer(1)]]);
                    assert_eq!(
                        c.query("SELECT count(*) FROM t GROUP BY v ORDER BY count(*)")?
                            .rows,
                        vec![vec![Value::Integer(1)], vec![Value::Integer(1)]]
                    );
                }
                let dynamic_expected = if version >= 68 {
                    c.execute("CREATE TABLE dynamic_values(i INTEGER PRIMARY KEY,v VARIANT,vs VARIANT[]); INSERT INTO dynamic_values VALUES(1,CAST({'d':1.25::DECIMAL(12,2),'ts':'2024-02-29 12:34:56.123456789'::TIMESTAMP_NS,'a':[1,NULL]::INTEGER[2]} AS VARIANT),[1::VARIANT,NULL]),(2,NULL,[])")?;
                    let value =
                        c.query("SELECT v FROM dynamic_values WHERE i=1")?.rows[0][0].clone();
                    let prepared = c.prepare("UPDATE dynamic_values SET v=$1 WHERE i=$2")?;
                    c.execute_prepared(&prepared, &[value.clone(), Value::Integer(2)])?;
                    let before_rollback = fs::read(path.with_extension("duckdb.wal"))?;
                    c.execute("BEGIN; UPDATE dynamic_values SET v=42::VARIANT; DELETE FROM dynamic_values WHERE i=2; ROLLBACK")?;
                    assert!(fs::read(path.with_extension("duckdb.wal"))? == before_rollback);
                    c.execute("DELETE FROM dynamic_values WHERE i=1; CHECKPOINT; UPDATE dynamic_values SET i=12 WHERE i=2")?;
                    c.execute_prepared(&prepared, &[value, Value::Integer(12)])?;
                    c.execute("CHECKPOINT; INSERT INTO dynamic_values VALUES(22,NULL,NULL)")?;
                    assert_eq!(c.query("SELECT a.i,b.i,row_number() OVER(ORDER BY a.i) FROM dynamic_values a JOIN dynamic_values b ON a.v=b.v")?.rows, vec![vec![Value::Integer(12),Value::Integer(12),Value::Integer(1)]]);
                    assert_eq!(
                        c.query(
                            "SELECT count(*) FROM dynamic_values GROUP BY v ORDER BY count(*)"
                        )?
                        .rows,
                        vec![vec![Value::Integer(1)], vec![Value::Integer(1)]]
                    );
                    Some(
                        c.query(
                            "SELECT i,variant_typeof(v),v::VARCHAR FROM dynamic_values ORDER BY i",
                        )?
                        .rows,
                    )
                } else {
                    None
                };
                let actual = selected
                    .checkpoint_encoder(&fs::read(&path)?)?
                    .unwrap()
                    .storage_version();
                assert_eq!(
                    actual,
                    Some(StorageVersion {
                        format: selected.format_id(),
                        version
                    })
                );
                drop(c);
                drop(database);
                drop(transactions);
                let mut readonly = Database::open_read_only(&path)?.connect();
                if let Some(expected) = dynamic_expected {
                    assert_eq!(
                        readonly
                            .query("SELECT i,variant_typeof(v),v::VARCHAR FROM dynamic_values ORDER BY i")?
                            .rows,
                        expected,
                    );
                }
                if version == 69 {
                    assert_eq!(
                        readonly.query("SELECT i FROM t ORDER BY i")?.rows,
                        vec![vec![Value::Integer(12)], vec![Value::Integer(22)]]
                    );
                    assert_eq!(readonly.query("SELECT r,s FROM e")?.rows.len(), 1);
                } else {
                    assert_eq!(
                        readonly.query("SELECT i FROM base ORDER BY i")?.rows,
                        vec![vec![Value::Integer(2)], vec![Value::Integer(3)]]
                    );
                }
                drop(readonly);
                let mut recovered = Database::open(&path)?.connect();
                recovered.execute("UPDATE base SET i=i+100")?;
                drop(recovered);
                let decoded = selected.decode(fs::read(&path)?, builtin_types())?;
                assert!(!decoded.row_ids(&TableName::main("base"))?.is_empty());
                assert_eq!(
                    selected
                        .checkpoint_encoder(&fs::read(&path)?)?
                        .unwrap()
                        .storage_version(),
                    actual
                );
            }
        }
    }
    Ok(())
}
