//! Independent complete/partial deletion vectors in nonzero row groups.
use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    catalog::TableName,
    common::type_registry::builtin_types,
    execution::index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
    parallel::QueryContext,
    storage::{
        TableStorage,
        checkpoint::FileCheckpoint,
        duckdb::{
            DuckDbFormat,
            wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
        },
        filesystem::OpenMode,
        format::SnapshotFormat,
        logged::FileWal,
    },
};
use std::sync::Arc;

const FIXTURES: [&str; 4] = [
    "release-64",
    "release-68",
    "development-64",
    "development-69",
];

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bytes(name: &str) -> Result<Vec<u8>> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test/data/native-deletion-identity")
        .join(format!("{name}.duckdb.gz"));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(source)?).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn live_ids(name: &str) -> Vec<u64> {
    (0..8192)
        .filter(|id| {
            if name == "development-69" {
                *id >= 6144 && *id != 6161
            } else {
                *id != 1 && !(*id >= 4096 && *id < 6144) && *id != 6161
            }
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_relative_deletion_vectors_keep_every_surviving_value_and_id() -> Result<()> {
    let context = QueryContext::background();
    let table = TableName::main("t");
    for name in FIXTURES {
        let snapshot = DuckDbFormat::default().decode(bytes(name)?, builtin_types())?;
        let expected = live_ids(name);
        assert_eq!(snapshot.row_ids(&table)?, expected, "{name}");
        assert_eq!(snapshot.next_row_id(&table)?, 8192);
        let rows = snapshot.scan(&table, &context)?;
        let mut expected_values = Database::memory()?.connect();
        let timestamp = expected_values
            .query("SELECT TIMESTAMP_NS '2024-01-02 03:04:05.123456789'")?
            .rows[0][0]
            .clone();
        for ((id, row), expected_id) in rows.iter().zip(expected) {
            assert_eq!(*id, expected_id);
            assert_eq!(row[0], Value::Integer(i128::from(*id)));
            assert_eq!(
                row[1],
                Value::Decimal {
                    value: i128::from(*id),
                    width: 12,
                    scale: 2
                }
            );
            assert_eq!(row[2], timestamp);
            assert_eq!(row[3].to_string(), format!("[{id}, NULL]"));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn open(path: &Path, indexes: Arc<dyn IndexFactory>) -> Result<Database> {
    let checkpoint =
        FileCheckpoint::open(path, OpenMode::ReadWrite, Arc::new(DuckDbFormat::default()))?
            .with_recovery(Arc::new(DuckDbWalRecovery))?;
    DatabaseBuilder::new()
        .indexes(indexes)
        .durability(Arc::new(
            FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?.with_checkpoint_policy(None),
        ))
        .build()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn relative_deletions_cross_prepared_wal_identity_rollback_checkpoint_and_recovery() -> Result<()> {
    for name in ["release-68", "development-69"] {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("deletions.duckdb");
            let original = bytes(name)?;
            fs::write(&path, &original)?;
            let mut c = open(&path, indexes.clone())?.connect();
            let update = c.prepare("UPDATE t SET amount=$1,xs=[$2,NULL] WHERE id=$2")?;
            let amount = c.query("SELECT 91.25::DECIMAL(12,2)")?.rows[0][0].clone();
            c.execute("BEGIN")?;
            c.execute_prepared(&update, &[amount.clone(), Value::Integer(6144)])?;
            c.execute("DELETE FROM t WHERE id=6145; ROLLBACK")?;
            assert!(fs::read(&path)? == original, "rollback changed {name}");
            c.execute_prepared(&update, &[amount, Value::Integer(6144)])?;
            c.execute("INSERT INTO t SELECT 9000,amount,stamp,xs FROM t WHERE id=6144; DELETE FROM t WHERE id=6145")?;
            assert!(matches!(
                c.execute("UPDATE t SET id=6144 WHERE id=9000"),
                Err(Error::Constraint(_))
            ));
            let sql = "SELECT id,amount,stamp,xs FROM t ORDER BY id";
            let after = c.query(sql)?.rows;
            drop(c);
            // Read-only replay and writable recovery must address the original
            // physical IDs, not compacted row ordinals.
            assert_eq!(
                Database::open_read_only(&path)?.connect().query(sql)?.rows,
                after
            );
            let mut c = open(&path, indexes)?.connect();
            assert_eq!(c.query(sql)?.rows, after);
            c.execute(
                "UPDATE t SET amount=92.25 WHERE id=9000; CHECKPOINT; DELETE FROM t WHERE id=6144",
            )?;
            let final_rows = c.query(sql)?.rows;
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?.connect().query(sql)?.rows,
                final_rows
            );
        }
    }
    Ok(())
}
