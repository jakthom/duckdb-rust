//! Versioned native publication through real transactions, not private JSON.
use super::*;
use duckdb_rust::{
    Error,
    execution::index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
    storage::{
        duckdb::{
            DuckDbFormat,
            wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
        },
        logged::FileWal,
    },
};
use std::{fs, path::Path};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn open(path: &Path, version: u64, indexes: Arc<dyn IndexFactory>) -> Result<Database> {
    DatabaseBuilder::new()
        .indexes(indexes)
        .durability(Arc::new(FileCheckpoint::open(
            path,
            OpenMode::ReadWrite,
            Arc::new(DuckDbFormat::default().with_storage_version(version)?),
        )?))
        .build()
}

const QUERY: &str =
    "SELECT id,variant_typeof(v),v::VARCHAR,xs::VARCHAR,n.d,n.ts FROM t ORDER BY id";

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_variant_versions_cross_prepared_mutations_indexes_rollback_and_reopen() -> Result<()> {
    for version in [68, 69] {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("mixed.duckdb");
            let mut c = open(&path, version, indexes.clone())?.connect();
            c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,v VARIANT,xs VARIANT[],n STRUCT(d DECIMAL(12,2),ts TIMESTAMP_NS)); INSERT INTO t VALUES (1,{'d':1.25::DECIMAL(12,2),'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789','items':[1,NULL]}::VARIANT,[42::VARIANT,NULL],{'d':1.25,'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789'}),(2,NULL,[],NULL)")?;
            if version == 69 {
                c.execute("CREATE TABLE tuples(id INTEGER PRIMARY KEY,v TUPLE(DECIMAL(12,2),TIMESTAMP_NS,VARIANT[])); INSERT INTO tuples SELECT id,row(n.d,n.ts,xs) FROM t; CREATE TABLE empty_values AS SELECT row() e,struct_pack() s")?;
            }
            let before = c.query(QUERY)?.rows;
            let bytes = fs::read(&path)?;
            let value = c.query("SELECT {'d':2.50::DECIMAL(12,2),'ts':TIMESTAMP_NS '2001-01-01 00:00:00.987654321','items':[7,NULL]}::VARIANT")?.rows[0][0].clone();
            let update = c.prepare("UPDATE t SET v=$1,xs=[$1,NULL] WHERE id=$2")?;
            c.execute("BEGIN")?;
            c.execute_prepared(&update, &[value.clone(), Value::Integer(1)])?;
            c.execute("DELETE FROM t WHERE id=2; ROLLBACK")?;
            assert_eq!(c.query(QUERY)?.rows, before);
            assert!(fs::read(&path)? == bytes, "rollback changed file");
            c.execute_prepared(&update, &[value, Value::Integer(1)])?;
            let after = c.query(QUERY)?.rows;
            assert_ne!(after, before);
            assert!(matches!(
                c.execute("UPDATE t SET id=1 WHERE id=2"),
                Err(Error::Constraint(_))
            ));
            assert_eq!(c.query(QUERY)?.rows, after);
            assert_eq!(
                c.query("SELECT count(*) FROM t a JOIN t b ON a.v=b.xs[1]")?
                    .rows,
                vec![vec![Value::Integer(1)]]
            );
            assert_eq!(
                c.query("SELECT count(*) OVER (PARTITION BY v) FROM t ORDER BY id")?
                    .rows,
                vec![vec![Value::Integer(1)]; 2]
            );
            let tuples = if version == 69 {
                Some(c.query("SELECT id,v::VARCHAR,e::VARCHAR,s::VARCHAR,typeof(e),typeof(s) FROM tuples CROSS JOIN empty_values ORDER BY id")?.rows)
            } else {
                None
            };
            drop(c);
            let mut c = open(&path, 64, indexes)?.connect();
            assert_eq!(c.query(QUERY)?.rows, after);
            if let Some(tuples) = tuples {
                assert_eq!(c.query("SELECT id,v::VARCHAR,e::VARCHAR,s::VARCHAR,typeof(e),typeof(s) FROM tuples CROSS JOIN empty_values ORDER BY id")?.rows, tuples);
            }
            c.execute("DELETE FROM t WHERE id=2")?;
            assert_eq!(
                c.query("SELECT count(DISTINCT v),count(*) FROM t")?.rows,
                vec![vec![Value::Integer(1), Value::Integer(1)]]
            );
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?
                    .connect()
                    .query("SELECT count(*) FROM t")?
                    .rows,
                vec![vec![Value::Integer(1)]]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_version_gates_reject_empty_nested_tables_and_wal_without_changing_files() -> Result<()> {
    for version in [64, 68] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("gated.duckdb");
        let mut c = open(&path, version, Arc::new(HashIndexFactory))?.connect();
        c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY); INSERT INTO t VALUES(1)")?;
        drop(c);
        let before = fs::read(&path)?;
        // A newer new-file preference is not permission to upgrade this file.
        let mut c = open(&path, 69, Arc::new(HashIndexFactory))?.connect();
        let mut rejected = vec![
            "CREATE TABLE bad(x TUPLE(INTEGER)[])",
            "CREATE TABLE bad AS SELECT struct_pack() x",
            "ALTER TABLE t ADD COLUMN v TUPLE(INTEGER)",
        ];
        if version == 64 {
            rejected.extend([
                "CREATE TABLE bad(x STRUCT(v VARIANT))",
                "ALTER TABLE t ADD COLUMN v VARIANT[]",
            ]);
        }
        for sql in rejected {
            assert!(
                matches!(c.execute(sql), Err(Error::InvalidInput(_))),
                "{sql}"
            );
            assert!(
                fs::read(&path)? == before,
                "failed statement changed file: {sql}"
            );
            assert!(c.query("SELECT * FROM bad").is_err());
            assert_eq!(
                c.query("SELECT id FROM t")?.rows,
                vec![vec![Value::Integer(1)]]
            );
        }
        drop(c);
    }
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("wal.duckdb");
    let checkpoint = FileCheckpoint::open(
        &path,
        OpenMode::ReadWrite,
        Arc::new(DuckDbFormat::default().with_storage_version(69)?),
    )?
    .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let mut c = DatabaseBuilder::new()
        .durability(Arc::new(FileWal::new(
            checkpoint,
            Arc::new(DuckDbTransactionLog),
        )?))
        .build()?
        .connect();
    for sql in [
        "CREATE TABLE bad(v VARIANT[])",
        "CREATE TABLE bad(v TUPLE(INTEGER))",
        "CREATE TABLE bad AS SELECT struct_pack() v",
    ] {
        assert!(
            matches!(c.execute(sql), Err(Error::Unsupported(_))),
            "{sql}"
        );
        assert!(!path.with_extension("duckdb.wal").exists());
    }
    c.execute("CREATE TABLE t(id INTEGER); INSERT INTO t VALUES(1)")?;
    let log = fs::read(path.with_extension("duckdb.wal"))?;
    assert!(matches!(
        c.execute("ALTER TABLE t ADD COLUMN v VARIANT"),
        Err(Error::Unsupported(_))
    ));
    assert!(
        fs::read(path.with_extension("duckdb.wal"))? == log,
        "failed ALTER changed WAL"
    );
    assert_eq!(
        c.query("SELECT id FROM t")?.rows,
        vec![vec![Value::Integer(1)]]
    );
    drop(c);
    assert_eq!(
        Database::open(&path)?
            .connect()
            .query("SELECT id FROM t")?
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    Ok(())
}
