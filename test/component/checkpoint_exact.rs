use super::*;
use duckdb_rust::{
    DataType,
    catalog::{CatalogMut, ColumnDefinition, TableDefinition},
    common::{NestedPayload, NestedType, NestedValue},
    storage::{
        TableStorageMut, format::JsonSnapshotFormat, layout::CheckpointLayout, table::Snapshot,
    },
};

#[path = "checkpoint_canonical.rs"]
mod canonical;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn wrapped(leaf: Value, shape: usize) -> Result<Value> {
    let ty = leaf.data_type();
    let (metadata, payload) = match shape {
        0 => (
            NestedType::List(ty),
            NestedPayload::Sequence(vec![leaf, Value::Null]),
        ),
        1 => (
            NestedType::Array {
                element: ty,
                length: 2,
            },
            NestedPayload::Sequence(vec![leaf, Value::Null]),
        ),
        2 => (
            NestedType::Struct(vec![("f".into(), ty)]),
            NestedPayload::Struct(vec![leaf]),
        ),
        3 => (
            NestedType::Tuple(vec![ty]),
            NestedPayload::Struct(vec![leaf]),
        ),
        4 => (
            NestedType::Object(vec![("".into(), ty)]),
            NestedPayload::Struct(vec![leaf]),
        ),
        5 => (
            NestedType::Map {
                key: DataType::Integer,
                value: ty,
            },
            NestedPayload::Map(vec![(Value::Integer(1), leaf)]),
        ),
        6 => (
            NestedType::Map {
                key: ty,
                value: DataType::Integer,
            },
            NestedPayload::Map(vec![(leaf, Value::Integer(1))]),
        ),
        7 => (
            NestedType::Union(vec![("f".into(), ty), ("s".into(), DataType::Varchar)]),
            NestedPayload::Union {
                tag: 0,
                value: leaf,
            },
        ),
        _ => (
            NestedType::Variant,
            NestedPayload::Variant {
                data_type: ty,
                value: leaf,
            },
        ),
    };
    NestedValue::value(metadata.data_type(), payload)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn checkpoint_layouts_compare_all_nested_float_bits_without_sql_equality() -> Result<()> {
    let query = QueryContext::background();
    for single in [true, false] {
        let float = |nan: bool, changed: bool| {
            if single {
                Value::Float(f32::from_bits(if nan {
                    0xffc01234 + u32::from(changed)
                } else if changed {
                    0
                } else {
                    0x80000000
                }))
            } else {
                Value::Double(f64::from_bits(if nan {
                    0xfff8000000001234 + u64::from(changed)
                } else if changed {
                    0
                } else {
                    0x8000000000000000
                }))
            }
        };
        for shape in 0..9 {
            let value = wrapped(float(true, false), shape)?;
            let mut column = ColumnDefinition::new("v", value.data_type());
            column.default = value.clone();
            let name = TableName::main("exact_values");
            let mut source = Snapshot::default();
            source.create_table(
                TableDefinition {
                    name: name.clone(),
                    columns: vec![column],
                    unique_keys: vec![],
                },
                false,
            )?;
            source.insert(
                &name,
                vec![vec![value], vec![wrapped(float(false, false), shape)?]],
                &query,
            )?;
            let target = JsonSnapshotFormat
                .decode(JsonSnapshotFormat.encode(&source)?, source.type_registry())?;
            let layout = CheckpointLayout::identity(&source)?;
            source.validate_checkpoint_layout(&target, &layout, &query)?;
            for (row, nan) in [(0, true), (1, false)] {
                let mut bad = target.clone();
                bad.update(
                    &name,
                    vec![(row, vec![wrapped(float(nan, true), shape)?])],
                    &query,
                )?;
                assert!(
                    matches!(
                        source.validate_checkpoint_layout(&bad, &layout, &query),
                        Err(Error::Internal(_))
                    ),
                    "shape {shape}, row {row}, FLOAT {single}"
                );
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nested_nan_values_cross_manual_automatic_checkpoints_and_writable_recovery() -> Result<()> {
    for automatic in [false, true] {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("nested_nan.duckdb");
            seed(&path)?;
            let (database, transactions) = open(&path, policy(automatic), indexes, None)?;
            let mut c = database.connect();
            c.execute("CREATE TABLE samples(i INTEGER PRIMARY KEY,v STRUCT(f FLOAT[],d DOUBLE[2],u UNION(x DOUBLE,y VARCHAR),m MAP(INTEGER,DOUBLE)))")?;
            c.execute("INSERT INTO samples VALUES(1,{'f':['NaN'::FLOAT,'-0'::FLOAT,NULL],'d':['NaN'::DOUBLE,'-0'::DOUBLE],'u':union_value(x:='NaN'::DOUBLE),'m':map([1,2],['NaN'::DOUBLE,'-0'::DOUBLE])})")?;
            let rows = c.query("SELECT v FROM samples")?.rows.into_rows();
            let expected = serde_json::to_vec(&rows).unwrap();
            let prepared = c.prepare("UPDATE samples SET v=$1 WHERE i=$2")?;
            c.execute("BEGIN; DELETE FROM samples; ROLLBACK")?;
            c.execute_prepared(&prepared, &[rows[0][0].clone(), Value::Integer(1)])?;
            c.checkpoint()?;
            assert!(!path.with_extension("duckdb.wal").exists());
            c.execute("UPDATE samples SET i=11 WHERE i=1")?;
            c.checkpoint()?;
            // Leave acknowledged work in the WAL and require writable recovery.
            c.execute("UPDATE samples SET i=21 WHERE i=11")?;
            drop(c);
            drop(database);
            drop(transactions);
            let mut c = Database::open(&path)?.connect();
            assert_eq!(
                c.query("SELECT i FROM samples")?.rows,
                vec![vec![Value::Integer(21)]]
            );
            let rows = c.query("SELECT v FROM samples")?.rows.into_rows();
            assert_eq!(serde_json::to_vec(&rows).unwrap(), expected);
            drop(c);
            let rows = Database::open_read_only(&path)?
                .connect()
                .query("SELECT v FROM samples")?
                .rows
                .into_rows();
            assert_eq!(serde_json::to_vec(&rows).unwrap(), expected);
        }
    }
    Ok(())
}
