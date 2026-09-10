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
