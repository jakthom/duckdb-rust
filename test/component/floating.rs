use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    catalog::{Catalog, CatalogMut, ColumnDefinition, TableDefinition, TableName},
    execution::index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
    parallel::QueryContext,
    storage::{
        TableStorage, TableStorageMut,
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
        table::Snapshot,
    },
};
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn float_has_single_precision_casts_operations_and_result_types() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    let aliases = connection.query("SELECT 1::FLOAT, 1::REAL, 1::FLOAT4, 1::FLOAT32, 1::FLOAT(24), 1::FLOAT(25), 1::FLOAT(53), 1::FLOAT8")?;
    assert_eq!(
        aliases
            .columns
            .iter()
            .map(|c| c.data_type.clone())
            .collect::<Vec<_>>(),
        vec![
            DataType::Float,
            DataType::Float,
            DataType::Float,
            DataType::Float,
            DataType::Float,
            DataType::Double,
            DataType::Double,
            DataType::Double
        ]
    );
    for sql in [
        "SELECT 1::FLOAT(0)",
        "SELECT 1::FLOAT(54)",
        "SELECT 1::FLOAT(20,2)",
        "SELECT 1e100::FLOAT",
    ] {
        assert!(connection.query(sql).is_err(), "{sql}");
    }
    let result = connection.query("SELECT 16777217::FLOAT, 16777216::FLOAT+1::FLOAT, 1::FLOAT/3::FLOAT, (5::FLOAT%2::FLOAT), -1::FLOAT, abs(-1::FLOAT), round(1.75::FLOAT), ' 1.5 '::FLOAT, true::FLOAT, 1::FLOAT+1::DOUBLE")?;
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Float(16777216.0),
            Value::Float(16777216.0),
            Value::Float(1.0 / 3.0),
            Value::Float(1.0),
            Value::Float(-1.0),
            Value::Float(1.0),
            Value::Float(2.0),
            Value::Float(1.5),
            Value::Float(1.0),
            Value::Double(2.0)
        ]]
    );
    assert!(
        result.columns[..9]
            .iter()
            .all(|c| c.data_type == DataType::Float)
    );
    let result = connection.query(
        "SELECT sum(f), avg(f), min(f), max(f) FROM (VALUES (1::FLOAT),(2::FLOAT),(NULL)) t(f)",
    )?;
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Double(3.0),
            Value::Double(1.5),
            Value::Float(1.0),
            Value::Float(2.0)
        ]]
    );
    assert_eq!(connection.query("SELECT count(DISTINCT f) FROM (VALUES ('NaN'::FLOAT),('NaN'::FLOAT),('-0'::FLOAT),('0'::FLOAT),(NULL)) t(f)")?.rows, vec![vec![Value::Integer(2)]]);
    assert_eq!(connection.query("SELECT try_cast(1e100 AS FLOAT), '1e100'::FLOAT, 3.4028234663852886e38::FLOAT+3.4028234663852886e38::FLOAT")?.rows, vec![vec![Value::Null, Value::Float(f32::INFINITY), Value::Float(f32::INFINITY)]]);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn float_extrema_and_sampled_bits_survive_both_checkpoint_formats() -> Result<()> {
    let formats: Vec<Arc<dyn SnapshotFormat>> = vec![
        Arc::new(JsonSnapshotFormat),
        Arc::new(DuckDbFormat::default()),
    ];
    let table = TableName::main("float_bits");
    let mut snapshot = Snapshot::default();
    snapshot.create_table(
        TableDefinition {
            name: table.clone(),
            columns: vec![ColumnDefinition {
                default: Value::Float(-0.5),
                ..ColumnDefinition::new("v", DataType::Float)
            }],
            unique_keys: vec![],
        },
        false,
    )?;
    // Deterministic representatives across signs, exponents, mantissas and NaN
    // payloads, plus the exact boundaries that floating conversions often lose.
    let mut bits = vec![
        0, 0x80000000, 1, 0x80000001, 0x007fffff, 0x00800000, 0x7f7fffff, 0xff7fffff, 0x7f800000,
        0xff800000, 0x7fc00123, 0xff800001,
    ];
    bits.extend((0..4096u32).map(|i| i.wrapping_mul(0x87a625b1)));
    let mut rows: Vec<_> = bits
        .iter()
        .map(|&v| vec![Value::Float(f32::from_bits(v))])
        .collect();
    rows.push(vec![Value::Null]);
    snapshot.insert(&table, rows, &QueryContext::background())?;
    for format in formats {
        let restored = format.decode(
            format.encode(&snapshot)?,
            duckdb_rust::common::type_registry::builtin_types(),
        )?;
        let rows = restored.scan(&table, &QueryContext::background())?;
        for ((_, row), bits) in rows.iter().zip(&bits) {
            assert_eq!(row[0].as_f32()?.to_bits(), *bits, "{}", format.name());
        }
        assert_eq!(rows.last().unwrap().1, vec![Value::Null]);
        let definition = restored.table(&table)?;
        assert_eq!(definition.columns[0].data_type, DataType::Float);
        assert_eq!(definition.columns[0].default, Value::Float(-0.5));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn float_indexes_keep_nan_zero_and_mutation_semantics_across_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let formats: Vec<Arc<dyn SnapshotFormat>> = vec![
        Arc::new(JsonSnapshotFormat),
        Arc::new(DuckDbFormat::default()),
    ];
    let indexes: Vec<Arc<dyn IndexFactory>> =
        vec![Arc::new(HashIndexFactory), Arc::new(BTreeIndexFactory)];
    for format in formats {
        for index in &indexes {
            let path = directory
                .path()
                .join(format!("{}-{}.db", format.name(), index.name()));
            let open = || {
                DatabaseBuilder::new()
                    .indexes(index.clone())
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format.clone(),
                    )?))
                    .build()
            };
            {
                let mut connection = open()?.connect();
                connection.execute("CREATE TABLE t(f FLOAT UNIQUE DEFAULT -5e-1); INSERT INTO t DEFAULT VALUES; INSERT INTO t VALUES ('NaN'::FLOAT),('-0'::FLOAT),('Infinity'::FLOAT),('-Infinity'::FLOAT),(NULL)")?;
            }
            let mut connection = open()?.connect();
            for value in ["'NaN'", "'0'", "'Infinity'", "'-Infinity'"] {
                assert!(matches!(
                    connection.execute(&format!("INSERT INTO t VALUES ({value}::FLOAT)")),
                    Err(Error::Constraint(_))
                ));
            }
            assert_eq!(
                connection
                    .query("SELECT count(*) FROM t WHERE f='NaN'::FLOAT")?
                    .rows,
                vec![vec![Value::Integer(1)]]
            );
            connection.execute("BEGIN; DELETE FROM t WHERE f=-5e-1::FLOAT; ROLLBACK; UPDATE t SET f=1e-1::FLOAT WHERE f=-5e-1::FLOAT")?;
            assert_eq!(
                connection
                    .query("SELECT f FROM t WHERE f=1e-1::FLOAT")?
                    .rows,
                vec![vec![Value::Float(0.1)]]
            );
        }
    }
    Ok(())
}
