//! Independently produced nested columns exercise physical child streams, not
//! values round-tripped through the same implementation's private serializer.
use super::*;
use std::{fs, io::Read, path::Path};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixture(target: &str, name: &str, path: &Path) -> Result<()> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("test/data/duckdb/nested-{target}"))
        .join(format!("{name}.duckdb.gz"));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(source)?).read_to_end(&mut bytes)?;
    fs::write(path, bytes)?;
    Ok(())
}

struct MissingDecimalValue;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::storage::compression::SegmentDecoder for MissingDecimalValue {
    fn id(&self) -> duckdb_rust::storage::compression::CodecId {
        duckdb_rust::storage::compression::CodecId(1)
    }
    fn name(&self) -> &'static str {
        "missing-decimal-test"
    }
    fn supports(&self, kind: duckdb_rust::storage::compression::SegmentType<'_>) -> bool {
        duckdb_rust::storage::duckdb::compression::UncompressedDecoder.supports(kind)
    }
    fn decode(
        &self,
        input: duckdb_rust::storage::compression::DecodeInput<'_>,
        context: &duckdb_rust::storage::compression::DecodeContext<'_>,
    ) -> Result<Vec<Value>> {
        let mut values = duckdb_rust::storage::duckdb::compression::UncompressedDecoder
            .decode(input, context)?;
        if matches!(
            input.kind,
            duckdb_rust::storage::compression::SegmentType::Values(DataType::Decimal { .. })
        ) {
            values.fill(Value::Null);
        }
        Ok(values)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_development_tuple_streams_preserve_empty_and_positional_children() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("development-tuple.duckdb");
    fixture("development", "nested_tuple", &path)?;
    let mut c = Database::open_read_only(&path)?.connect();
    assert_eq!(c.query("SELECT id,v[1],v[4][1],v[4][2],v[5]::VARCHAR,e::VARCHAR,s::VARCHAR,one::VARCHAR,typeof(e),typeof(s),typeof(one) FROM t ORDER BY id")?.rows,
        (0..3).map(|i|vec![Value::Integer(i),if i==2 {Value::Null}else{Value::Integer(i)},if i==2 {Value::Null}else{Value::Integer(i)},Value::Null,if i==2 {Value::Null}else{Value::Varchar("101".into())},Value::Varchar("()".into()),Value::Varchar("{}".into()),Value::Varchar(format!("({i},)")),Value::Varchar("TUPLE".into()),Value::Varchar("STRUCT".into()),Value::Varchar("TUPLE(INTEGER)".into())]).collect::<Vec<_>>());
    assert_eq!(
        c.query("SELECT v[2],v[3] FROM t WHERE id=0")?.rows,
        vec![vec![
            Value::Decimal {
                value: 12500,
                width: 12,
                scale: 2
            },
            Value::Temporal(duckdb_rust::common::TemporalValue::parse(
                "2000-01-01 00:00:00.123456789",
                &DataType::TimestampNs
            )?)
        ]]
    );
    drop(c);
    // A NULL placeholder is legal only while waiting for validity. The same
    // selected decoder output under a valid child mask remains corruption.
    let mut decoders = duckdb_rust::storage::duckdb::compression::decoders();
    decoders.replace(Arc::new(MissingDecimalValue))?;
    let malformed = DatabaseBuilder::new()
        .durability(Arc::new(FileCheckpoint::open(
            &path,
            OpenMode::ReadOnly,
            Arc::new(duckdb_rust::storage::duckdb::DuckDbFormat::new(decoders)),
        )?))
        .build();
    assert!(
        matches!(malformed, Err(duckdb_rust::Error::Corrupt(message)) if message.contains("valid row has no decoded value"))
    );
    // Its retained v2 compatibility permits tuple/empty-container publication.
    let mut c = Database::open(&path)?.connect();
    c.execute("BEGIN; UPDATE t SET id=7 WHERE id=0; ROLLBACK")?;
    assert_eq!(
        c.query("SELECT sum(id) FROM t")?.rows,
        vec![vec![Value::Integer(3)]]
    );
    c.execute("UPDATE t SET id=7 WHERE id=0")?;
    let expected = c.query("SELECT * FROM t ORDER BY id")?.rows;
    drop(c);
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT * FROM t ORDER BY id")?
            .rows,
        expected
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_nested_child_streams_survive_checkpoint_publication() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for target in ["release", "development"] {
        let path = directory.path().join(format!("{target}-nested.duckdb"));
        fixture(target, "nested_scalar", &path)?;
        let mut connection = Database::open(&path)?.connect();
        let before = connection.query("SELECT * FROM t ORDER BY id")?;
        assert_eq!(before.rows.len(), 3);
        assert_eq!(
            before.columns[1].data_type,
            NestedType::List(DataType::Integer).data_type()
        );
        assert_eq!(
            connection
                .query("SELECT xs[2],a[2],s.x,u.i,u.s FROM t ORDER BY id")?
                .rows,
            vec![
                vec![
                    Value::Null,
                    Value::Integer(2),
                    Value::Decimal {
                        value: 125,
                        width: 8,
                        scale: 2
                    },
                    Value::Integer(7),
                    Value::Null
                ],
                vec![
                    Value::Null,
                    Value::Integer(4),
                    Value::Null,
                    Value::Null,
                    Value::Varchar("x".into())
                ],
                vec![Value::Null; 5]
            ]
        );
        connection.execute("BEGIN; UPDATE t SET xs=[9],s={'x':9.00,'y':'aborted'} WHERE id=1; DELETE FROM t WHERE id=2; ROLLBACK")?;
        assert_eq!(
            connection.query("SELECT * FROM t ORDER BY id")?.rows,
            before.rows
        );
        connection.execute("UPDATE t SET xs=[4,5],s={'x':2.50,'y':'changed'} WHERE id=1; INSERT INTO t VALUES(4,[8],[5,6],{'x':3.75,'y':'new'},map([3],['c']),union_value(s:='new'))")?;
        let after = connection.query("SELECT * FROM t ORDER BY id")?;
        drop(connection);
        let mut reopened = Database::open_read_only(&path)?.connect();
        let result = reopened.query("SELECT * FROM t ORDER BY id")?;
        assert_eq!(
            result
                .columns
                .iter()
                .map(|field| (&field.name, &field.data_type))
                .collect::<Vec<_>>(),
            after
                .columns
                .iter()
                .map(|field| (&field.name, &field.data_type))
                .collect::<Vec<_>>()
        );
        assert_eq!(result.rows, after.rows);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_nested_bitpacking_and_row_group_boundaries() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for target in ["release", "development"] {
        for (name, count) in [("nested_bitpacking", 10013), ("nested_rowgroups", 125013)] {
            let path = directory.path().join(format!("{target}-{name}.duckdb"));
            fixture(target, name, &path)?;
            let mut connection = Database::open(&path)?.connect();
            let rows = connection.query("SELECT * FROM t")?.rows;
            assert_eq!(rows.len(), count);
            for (i, row) in rows.iter().enumerate() {
                assert_eq!(row[0], Value::Integer(i as i128));
                let expected = if i % 17 == 0 {
                    Value::Null
                } else {
                    NestedValue::value(
                        NestedType::List(DataType::BigInt).data_type(),
                        NestedPayload::Sequence(if i % 13 == 0 {
                            vec![]
                        } else {
                            vec![
                                Value::Integer(i as i128),
                                Value::Null,
                                Value::Integer(i as i128 + 1),
                            ]
                        }),
                    )?
                };
                assert_eq!(row[1], expected, "{name} LIST row {i}");
                let Value::Nested(record) = &row[row.len() - 1] else {
                    panic!("expected STRUCT at row {i}")
                };
                let NestedPayload::Struct(fields) = &record.payload else {
                    panic!("expected STRUCT payload")
                };
                let expected = if count == 10013 {
                    vec![
                        Value::Decimal {
                            value: (i % 100) as i128 * 100,
                            width: 8,
                            scale: 2,
                        },
                        if i % 19 == 0 {
                            Value::Null
                        } else {
                            Value::Varchar(format!("v{i}"))
                        },
                    ]
                } else {
                    vec![
                        Value::Integer(i as i128),
                        if i % 19 == 0 {
                            Value::Null
                        } else {
                            Value::Integer(i as i128)
                        },
                    ]
                };
                assert_eq!(fields, &expected, "{name} STRUCT row {i}");
                if count == 10013 {
                    assert_eq!(
                        row[2],
                        NestedValue::value(
                            NestedType::Array {
                                element: DataType::BigInt,
                                length: 2
                            }
                            .data_type(),
                            NestedPayload::Sequence(vec![Value::Integer(i as i128), Value::Null])
                        )?,
                        "ARRAY row {i}"
                    );
                }
            }
            if count == 125013 {
                connection.execute(
                    "CREATE TABLE published(id INTEGER); INSERT INTO published VALUES(1)",
                )?;
                drop(connection);
                let mut reopened = Database::open_read_only(&path)?.connect();
                assert_eq!(
                    reopened.query("SELECT * FROM t")?.rows,
                    rows,
                    "{target} multi-row-group publication"
                );
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_roaring_container_families_preserve_child_validity_and_boolean_values() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    for target in ["release", "development"] {
        let path = directory.path().join(format!("{target}-roaring.duckdb"));
        fixture(target, "nested_roaring", &path)?;
        let mut connection = Database::open_read_only(&path)?.connect();
        let rows = connection.query("SELECT * FROM t")?.rows;
        assert_eq!(rows.len(), 125013);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row[0], Value::Integer(i as i128));
            let Value::Nested(record) = &row[1] else {
                panic!("STRUCT row {i}")
            };
            let NestedPayload::Struct(fields) = &record.payload else {
                panic!("STRUCT payload")
            };
            let sparse = [2, 8, 2047].contains(&(i % 2048));
            let run = (300..=1100).contains(&(i % 2048));
            let many_runs = (30..=90).contains(&(i % 256));
            let alternating = i % 2 == 0;
            let mut expected = [sparse, !sparse, run, many_runs, alternating]
                .into_iter()
                .map(|null| {
                    if null {
                        Value::Null
                    } else {
                        Value::Integer(i as i128)
                    }
                })
                .collect::<Vec<_>>();
            expected.extend(
                [sparse, run, many_runs, alternating]
                    .into_iter()
                    .map(Value::Boolean),
            );
            assert_eq!(fields, &expected, "{target} roaring row {i}");
        }
    }
    Ok(())
}
