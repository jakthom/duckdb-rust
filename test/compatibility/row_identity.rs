//! Development storage 69 keeps physical cardinality separate from row IDs.
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
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::SnapshotFormat,
        recovery::{RecoveredChange as Change, RecoveryTarget},
    },
};
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bytes(producer: &str) -> Result<Vec<u8>> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test/data/native-row-identity-initial")
        .join(format!("storage-69-{producer}.duckdb.gz"));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(source)?).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn development_row_identity_gaps_keep_typed_rows_and_recovery_append_watermarks() -> Result<()> {
    let context = QueryContext::background();
    let table = TableName::main("t");
    for (producer, id) in [("rust", 1), ("development", 3)] {
        let mut snapshot = DuckDbFormat::default().decode(bytes(producer)?, builtin_types())?;
        assert_eq!(snapshot.row_ids(&table)?, vec![id], "{producer}");
        assert_eq!(snapshot.next_row_id(&table)?, id + 1, "{producer}");
        let before = snapshot.scan(&table, &context)?;
        let mut appended = before[0].1.clone();
        appended[0] = Value::Integer(12);
        snapshot.apply_committed(
            &[
                Change::Update {
                    table: table.clone(),
                    column: 0,
                    values: vec![(id, Value::Integer(21))],
                },
                Change::Insert {
                    table: table.clone(),
                    rows: vec![appended.clone()],
                },
            ],
            &context,
        )?;
        assert_eq!(snapshot.row_ids(&table)?, vec![id, id + 1]);
        assert_eq!(snapshot.next_row_id(&table)?, id + 2);
        let rows = snapshot.scan(&table, &context)?;
        assert_eq!(rows[0].1[0], Value::Integer(21));
        assert_eq!(&rows[0].1[1..], &before[0].1[1..]);
        assert_eq!(rows[1], (id + 1, appended));
        snapshot.apply_committed(
            &[Change::Delete {
                table: table.clone(),
                ids: vec![id],
            }],
            &context,
        )?;
        assert_eq!(snapshot.row_ids(&table)?, vec![id + 1]);
        assert_eq!(snapshot.next_row_id(&table)?, id + 2);
    }
    Ok(())
}

const QUERY: &str = "SELECT id,n.d::VARCHAR,n.ts::VARCHAR,xs::VARCHAR,hex(b),u::VARCHAR,v::VARCHAR,variant_typeof(v),vs::VARCHAR,p::VARCHAR,typeof(p),e::VARCHAR,s::VARCHAR FROM t CROSS JOIN empty_values ORDER BY id";

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn gapped_native_rows_cross_prepared_indexes_rollback_publication_and_reopen() -> Result<()> {
    let expected = vec![Value::Integer(11)]
        .into_iter()
        .chain(
            [
                "56.78",
                "2025-02-03 04:05:06.987654321",
                "[3, NULL, 4]",
                "610062",
                "340282366920938463463374607431768211455",
                "{'d': 12.34, 'ts': '2024-01-02 03:04:05.123456789', 'items': [1, NULL, 2]}",
                "OBJECT(d, ts, items)",
                "[1, NULL]",
                "(12.34, '2024-01-02 03:04:05.123456789')",
                "TUPLE(DECIMAL(8,2), TIMESTAMP_NS)",
                "()",
                "{}",
            ]
            .map(|text| Value::Varchar(text.into())),
        )
        .collect::<Vec<_>>();
    for producer in ["rust", "development"] {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("gaps.duckdb");
            let original = bytes(producer)?;
            fs::write(&path, &original)?;
            let mut c = DatabaseBuilder::new()
                .indexes(indexes)
                .durability(Arc::new(FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    Arc::new(DuckDbFormat::default()),
                )?))
                .build()?
                .connect();
            assert_eq!(c.query(QUERY)?.rows, vec![expected.clone()]);
            let update = c.prepare("UPDATE t SET id=$1,xs=[$1,NULL] WHERE id=$2")?;
            c.execute("BEGIN")?;
            c.execute_prepared(&update, &[Value::Integer(21), Value::Integer(11)])?;
            c.execute("DELETE FROM t WHERE id=21; ROLLBACK")?;
            assert_eq!(c.query(QUERY)?.rows, vec![expected.clone()]);
            assert!(
                fs::read(&path)? == original,
                "rollback changed {producer} image"
            );
            c.execute_prepared(&update, &[Value::Integer(21), Value::Integer(11)])?;
            c.execute("INSERT INTO t SELECT 22,n,xs,b,u,v,vs,p FROM t WHERE id=21")?;
            assert!(matches!(
                c.execute("UPDATE t SET id=21 WHERE id=22"),
                Err(Error::Constraint(_))
            ));
            assert_eq!(c.query("SELECT a.id,count(*) OVER(PARTITION BY a.n) FROM t a JOIN t b ON a.u=b.u AND a.id=b.id ORDER BY a.id")?.rows,
                vec![vec![Value::Integer(21),Value::Integer(2)],vec![Value::Integer(22),Value::Integer(2)]]);
            let after = c.query(QUERY)?.rows;
            for row in &after {
                assert_eq!(&row[4..], &expected[4..]);
            }
            drop(c);
            let mut c = Database::open(&path)?.connect();
            assert_eq!(c.query(QUERY)?.rows, after);
            c.execute("DELETE FROM t WHERE id=21")?;
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?
                    .connect()
                    .query(QUERY)?
                    .rows,
                vec![after[1].to_vec()]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn truncated_live_blocks_are_not_mistaken_for_a_valid_free_tail() -> Result<()> {
    let original = bytes("development")?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("invalid.duckdb");
    for length in [original.len() - 1, original.len() - 262144, 12288] {
        let shortened = &original[..length];
        fs::write(&path, shortened)?;
        assert!(matches!(
            Database::open_read_only(&path),
            Err(Error::Corrupt(_))
        ));
        assert!(
            fs::read(&path)? == shortened,
            "failed read changed its input"
        );
    }
    Ok(())
}
