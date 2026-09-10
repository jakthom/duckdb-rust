use duckdb_rust::common::{
    cast::{CastMode, CastRegistry},
    numeric::decimal,
    type_registry::builtin_types,
};
use duckdb_rust::parallel::QueryContext;
use duckdb_rust::{DataType, Database, Error, Result, Value};
use std::sync::Arc;

#[path = "numeric_absolute.rs"]
mod absolute;
#[path = "numeric_batches.rs"]
mod batches;
#[path = "numeric_contracts.rs"]
mod contracts;
#[path = "numeric_direction.rs"]
mod direction;
#[path = "numeric_precision.rs"]
mod precision;
#[path = "numeric_rounding.rs"]
mod rounding;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn trunc_preserves_integer_domains_and_discards_fraction_toward_zero() -> Result<()> {
    let mut c = Database::memory()?.connect();
    let result = c.query("SELECT trunc(1.5),trunc(-1.5),trunc(123.45::DECIMAL(5,2)),trunc('-170141183460469231731687303715884105728'::HUGEINT),trunc('340282366920938463463374607431768211455'::UHUGEINT),trunc(-1.9::FLOAT),trunc(1.9::DOUBLE),trunc(NULL)")?;
    assert_eq!(
        result.rows,
        vec![vec![
            decimal(1, 2, 0)?,
            decimal(-1, 2, 0)?,
            decimal(123, 5, 0)?,
            Value::Integer(i128::MIN),
            Value::Unsigned(u128::MAX),
            Value::Float(-1.0),
            Value::Double(1.0),
            Value::Null
        ]]
    );
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| column.data_type.clone())
            .collect::<Vec<_>>(),
        vec![
            DataType::Decimal { width: 2, scale: 0 },
            DataType::Decimal { width: 2, scale: 0 },
            DataType::Decimal { width: 5, scale: 0 },
            DataType::HugeInt,
            DataType::UHugeInt,
            DataType::Float,
            DataType::Double,
            DataType::BigInt
        ]
    );
    for ty in [
        "TINYINT",
        "SMALLINT",
        "INTEGER",
        "BIGINT",
        "HUGEINT",
        "UTINYINT",
        "USMALLINT",
        "UINTEGER",
        "UBIGINT",
        "UHUGEINT",
    ] {
        assert_eq!(
            c.query(&format!("SELECT typeof(trunc(1::{ty}))"))?.rows,
            vec![vec![Value::Varchar(ty.into())]]
        );
    }
    assert!(c.query("SELECT trunc('1.5'::VARCHAR)").is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn insert_values_assign_each_expression_without_intermediate_common_coercion() -> Result<()> {
    use duckdb_rust::{
        DatabaseBuilder,
        execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
        optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    };
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for batch_size in [1, 3, 2048] {
                let mut c = DatabaseBuilder::new()
                    .expressions(expressions.clone())
                    .optimizer(optimizer.clone())
                    .batch_size(batch_size)
                    .build()?
                    .connect();
                c.execute(
                    "CREATE TABLE v(s VARCHAR); INSERT INTO v VALUES (1.25),(0),('hello'),(NULL)",
                )?;
                assert_eq!(
                    c.query("SELECT s FROM v")?.rows,
                    vec![
                        vec![Value::Varchar("1.25".into())],
                        vec![Value::Varchar("0".into())],
                        vec![Value::Varchar("hello".into())],
                        vec![Value::Null]
                    ]
                );
                c.execute("DELETE FROM v; INSERT INTO v(s) (VALUES (1.25),(0))")?;
                assert_eq!(
                    c.query("SELECT s FROM v")?.rows,
                    vec![
                        vec![Value::Varchar("1.25".into())],
                        vec![Value::Varchar("0".into())]
                    ]
                );
                c.execute("DELETE FROM v; INSERT INTO v SELECT * FROM (VALUES (1.25),(0)) q")?;
                assert_eq!(
                    c.query("SELECT s FROM v")?.rows,
                    vec![
                        vec![Value::Varchar("1.25".into())],
                        vec![Value::Varchar("0.00".into())]
                    ]
                );
                c.execute(
                    "DELETE FROM v; INSERT INTO v WITH unused AS (SELECT 1) VALUES (1.25),(0)",
                )?;
                assert_eq!(
                    c.query("SELECT s FROM v")?.rows,
                    vec![
                        vec![Value::Varchar("1.25".into())],
                        vec![Value::Varchar("0.00".into())]
                    ]
                );
                c.execute("CREATE TABLE ordered(d DECIMAL(4,1), s VARCHAR, u UTINYINT); INSERT INTO ordered(s,u,d) VALUES (1.25,255,1.249),(0,0,2)")?;
                assert_eq!(
                    c.query("SELECT * FROM ordered")?.rows,
                    vec![
                        vec![
                            decimal(12, 4, 1)?,
                            Value::Varchar("1.25".into()),
                            Value::Unsigned(255)
                        ],
                        vec![
                            decimal(20, 4, 1)?,
                            Value::Varchar("0".into()),
                            Value::Unsigned(0)
                        ]
                    ]
                );
                assert!(
                    c.execute("INSERT INTO ordered VALUES (1,'ok',1),(2,'bad',256)")
                        .is_err()
                );
                assert_eq!(
                    c.query("SELECT count(*) FROM ordered")?.rows,
                    vec![vec![Value::Integer(2)]]
                );
                for sql in [
                    "INSERT INTO ordered VALUES (1,2)",
                    "INSERT INTO ordered VALUES (1,2,3),(1,2)",
                    "INSERT INTO ordered(s,s) VALUES (1,2)",
                ] {
                    assert!(c.execute(sql).is_err(), "{sql}");
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn full_unsigned_domains_and_checked_casts() -> Result<()> {
    let query = QueryContext::background();
    let types = builtin_types();
    let casts = CastRegistry::builtins();
    for (data_type, bits) in [
        (DataType::UTinyInt, 8),
        (DataType::USmallInt, 16),
        (DataType::UInteger, 32),
        (DataType::UBigInt, 64),
        (DataType::UHugeInt, 128),
    ] {
        let maximum = u128::MAX >> (128 - bits);
        let cast = casts.bind(&DataType::Varchar, &data_type, CastMode::Explicit, &types)?;
        for n in [0, 1, maximum / 2, maximum] {
            let value = cast.apply(&Value::Varchar(n.to_string()), &query)?;
            assert_eq!(value, Value::Unsigned(n));
            assert!(value.fits_type(&data_type));
            assert_eq!(value.to_string(), n.to_string());
        }
        for text in [
            "-1".to_owned(),
            maximum
                .checked_add(1)
                .map(|n| n.to_string())
                .unwrap_or_else(|| "340282366920938463463374607431768211456".into()),
        ] {
            assert!(matches!(
                cast.apply(&Value::Varchar(text), &query),
                Err(Error::Conversion(_))
            ));
        }
    }
    assert_eq!(
        Value::Unsigned(u128::MAX).cast(&DataType::Varchar)?,
        Value::Varchar(u128::MAX.to_string())
    );
    assert!(Value::Unsigned(u128::MAX).cast(&DataType::HugeInt).is_err());
    assert_eq!(
        Value::Unsigned(u64::MAX as u128).cast(&DataType::HugeInt)?,
        Value::Integer(u64::MAX as i128)
    );
    for (text, expected) in [
        ("0xf_f", 255),
        ("0b1_0", 2),
        ("1_2.3_4", 12),
        ("1e1_0", 10000000000),
    ] {
        assert_eq!(
            Value::Varchar(text.into()).cast(&DataType::UHugeInt)?,
            Value::Unsigned(expected)
        );
    }
    for text in ["+0xff", "-0xff", "0x_ff", "0x+1", "1__2", "1_.0"] {
        assert!(
            Value::Varchar(text.into())
                .cast(&DataType::UHugeInt)
                .is_err(),
            "{text}"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn development_numeric_types_errors_and_coercion_are_authoritative() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT typeof(round(1::UTINYINT)),typeof(round(1::UBIGINT)),typeof(round(1::UHUGEINT)),typeof(1::DECIMAL(18,0)+1::DECIMAL(18,0)),typeof(CAST('bad' AS DECIMAL(5,2))),typeof(coalesce(1::DECIMAL(38,0),1::DECIMAL(38,38)))")?.rows,
        vec![vec!["BIGINT", "HUGEINT", "DOUBLE", "DECIMAL(19,0)", "DECIMAL(5,2)", "DECIMAL(38,0)"].into_iter().map(|v| Value::Varchar(v.into())).collect::<Vec<_>>()]);
    for t in ["UTINYINT", "USMALLINT", "UINTEGER", "UBIGINT", "UHUGEINT"] {
        assert!(matches!(
            c.query(&format!("SELECT -1::{t}")),
            Err(Error::OutOfRange(_))
        ));
        assert_eq!(
            c.query(&format!("SELECT -0::{t}"))?.rows,
            vec![vec![Value::Unsigned(0)]]
        );
        for op in ["//", "%"] {
            assert!(matches!(
                c.query(&format!("SELECT 1::{t}{op}0::{t}")),
                Err(Error::InvalidInput(_))
            ));
        }
    }
    assert!(matches!(
        c.query("SELECT 1.0%0.0"),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(
        decimal(99999999999999999999999999999999999999, 38, 1)?.compare(&decimal(
            9999999999999999999999999999999999999,
            37,
            0
        )?)?,
        std::cmp::Ordering::Greater
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_type_adapters_share_order_keys_replacement_and_cancellation() -> Result<()> {
    use duckdb_rust::common::type_registry::{
        TypeAdapter, TypeRegistry,
        numeric::{ExactNumericTypes, LexicalNumericTypes},
    };
    use duckdb_rust::parallel::InterruptHandle;
    let query = QueryContext::background();
    for adapter in [
        Arc::new(ExactNumericTypes) as Arc<dyn TypeAdapter>,
        Arc::new(LexicalNumericTypes),
    ] {
        let mut types = TypeRegistry::builtins();
        for t in [
            DataType::UTinyInt,
            DataType::USmallInt,
            DataType::UInteger,
            DataType::UBigInt,
            DataType::UHugeInt,
            DataType::Decimal {
                width: 38,
                scale: 17,
            },
        ] {
            types.replace(t.family(), adapter.clone())?;
            let bound = types.bind(&t)?;
            let values = if let Some(bits) = t.unsigned_bits() {
                vec![
                    Value::Unsigned(0),
                    Value::Unsigned(1),
                    Value::Unsigned(u128::MAX >> (128 - bits)),
                ]
            } else {
                [
                    -99999999999999999999999999999999999999,
                    -10,
                    -1,
                    0,
                    1,
                    10,
                    99999999999999999999999999999999999999,
                ]
                .into_iter()
                .map(|n| decimal(n, 38, 17))
                .collect::<Result<Vec<_>>>()?
            };
            for (i, a) in values.iter().enumerate() {
                for (j, b) in values.iter().enumerate() {
                    assert_eq!(bound.compare(a, b, &query)?, i.cmp(&j));
                    let (mut ak, mut bk) = (vec![], vec![]);
                    bound.append_key(a, &mut ak, &query)?;
                    bound.append_key(b, &mut bk, &query)?;
                    assert_eq!(ak == bk, i == j);
                }
            }
            types.replace(t.family(), Arc::new(ExactNumericTypes))?;
            assert_eq!(bound.adapter(), adapter.name());
            let interrupt = InterruptHandle::default();
            let cancelled = QueryContext::new(interrupt.clone(), None, 3, 100)?;
            interrupt.interrupt();
            let mut key = vec![7];
            assert!(matches!(
                bound.append_key(&values[0], &mut key, &cancelled),
                Err(Error::Interrupted)
            ));
            assert_eq!(key, vec![7]);
            assert!(
                bound
                    .validate(&Value::Varchar("bad".into()), &query)
                    .is_err()
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn decimal_precision_scale_rounding_and_metadata() -> Result<()> {
    for width in 1..=38 {
        for scale in 0..=width {
            let t = DataType::Decimal { width, scale };
            let limit = 10_i128.pow(width as u32);
            for n in [0, 1, -1, limit - 1, -(limit - 1)] {
                let value = decimal(n, width, scale)?;
                assert_eq!(Value::Varchar(value.to_string()).cast(&t)?, value);
                assert_eq!(
                    serde_json::from_str::<Value>(&serde_json::to_string(&value).unwrap()).unwrap(),
                    value
                );
            }
            assert!(decimal(limit, width, scale).is_err());
        }
    }
    let t = DataType::Decimal { width: 5, scale: 2 };
    for (text, expected) in [
        ("1.235", 124),
        ("-1.235", -124),
        ("1.234999999999999999999999999999999999", 123),
        ("9.995e1", 9995),
        ("1e-100", 0),
    ] {
        assert_eq!(
            Value::Varchar(text.into()).cast(&t)?,
            decimal(expected, 5, 2)?
        );
    }
    assert!(Value::Varchar("999.995".into()).cast(&t).is_err());
    assert!(
        builtin_types()
            .bind(&DataType::Decimal {
                width: 39,
                scale: 0
            })
            .is_err()
    );
    assert!(
        builtin_types()
            .bind(&DataType::Decimal { width: 4, scale: 5 })
            .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sql_numeric_arithmetic_aggregates_and_relational_keys() -> Result<()> {
    let mut c = Database::memory()?.connect();
    let result = c.query("SELECT 1.25+2.5, 1.25*2.5, 1.25/2.5, 255::UTINYINT, 18446744073709551615::UBIGINT, '340282366920938463463374607431768211455'::UHUGEINT")?;
    assert_eq!(
        result.rows,
        vec![vec![
            decimal(375, 4, 2)?,
            decimal(3125, 5, 3)?,
            Value::Double(0.5),
            Value::Unsigned(255),
            Value::Unsigned(u64::MAX as u128),
            Value::Unsigned(u128::MAX)
        ]]
    );
    assert_eq!(
        result.columns[0].data_type,
        DataType::Decimal { width: 4, scale: 2 }
    );
    for sql in [
        "SELECT 255::UTINYINT+1::UTINYINT",
        "SELECT 0::UBIGINT-1::UBIGINT",
        "SELECT '340282366920938463463374607431768211455'::UHUGEINT+1::UHUGEINT",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    c.execute("CREATE TABLE n(k DECIMAL(10,2), u UBIGINT); INSERT INTO n VALUES (1.25,18446744073709551615),(1.25,18446744073709551615),(2.50,0),(NULL,NULL)")?;
    assert_eq!(
        c.query("SELECT k,count(*),sum(k),sum(u) FROM n GROUP BY k ORDER BY k")?
            .rows,
        vec![
            vec![
                decimal(125, 10, 2)?,
                Value::Integer(2),
                decimal(250, 38, 2)?,
                Value::Integer(2 * (u64::MAX as i128))
            ],
            vec![
                decimal(250, 10, 2)?,
                Value::Integer(1),
                decimal(250, 38, 2)?,
                Value::Integer(0)
            ],
            vec![Value::Null, Value::Integer(1), Value::Null, Value::Null]
        ]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM n a JOIN n b ON a.k=b.k AND a.u=b.u")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        c.query("SELECT u FROM n INTERSECT SELECT u FROM n ORDER BY u")?
            .rows,
        vec![
            vec![Value::Unsigned(0)],
            vec![Value::Unsigned(u64::MAX as u128)],
            vec![Value::Null]
        ]
    );
    assert_eq!(
        c.query("SELECT sum(k) OVER () FROM n LIMIT 1")?.rows,
        vec![vec![decimal(500, 38, 2)?]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_defaults_indexes_and_snapshots_survive_restart() -> Result<()> {
    use duckdb_rust::{
        DatabaseBuilder,
        execution::index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        storage::{
            checkpoint::FileCheckpoint,
            duckdb::DuckDbFormat,
            filesystem::OpenMode,
            format::{JsonSnapshotFormat, SnapshotFormat},
        },
    };
    let directory = tempfile::tempdir()?;
    for format in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ] {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            let path = directory
                .path()
                .join(format!("{}-{}.db", format.name(), indexes.name()));
            let open = || {
                DatabaseBuilder::new()
                    .indexes(indexes.clone())
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format.clone(),
                    )?))
                    .build()
            };
            {
                let mut c = open()?.connect();
                c.execute("CREATE TABLE d(k DECIMAL(38,3) PRIMARY KEY DEFAULT 1.125, a UTINYINT DEFAULT 255, b USMALLINT DEFAULT 65535, c UINTEGER DEFAULT 4294967295, d UBIGINT DEFAULT 18446744073709551615, e UHUGEINT DEFAULT '340282366920938463463374607431768211455'); INSERT INTO d DEFAULT VALUES; INSERT INTO d VALUES (-99999999999999999999999999999999999.999,0,0,0,0,0)")?;
            }
            let mut c = open()?.connect();
            let result = c.query("SELECT * FROM d WHERE k=1.125")?;
            assert_eq!(
                result.rows,
                vec![vec![
                    decimal(1125, 38, 3)?,
                    Value::Unsigned(255),
                    Value::Unsigned(65535),
                    Value::Unsigned(u32::MAX as u128),
                    Value::Unsigned(u64::MAX as u128),
                    Value::Unsigned(u128::MAX)
                ]]
            );
            assert!(matches!(
                c.execute("INSERT INTO d DEFAULT VALUES"),
                Err(Error::Constraint(_))
            ));
            c.execute("BEGIN; DELETE FROM d; ROLLBACK; UPDATE d SET k=2.250 WHERE k=1.125")?;
            drop(c);
            assert_eq!(
                open()?
                    .connect()
                    .query("SELECT count(*) FROM d WHERE k=2.250")?
                    .rows,
                vec![vec![Value::Integer(1)]]
            );
        }
    }
    Ok(())
}
