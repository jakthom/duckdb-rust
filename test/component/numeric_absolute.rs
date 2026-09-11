use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    common::{
        cast::{CastFunction, CastSpec},
        vector::Vector,
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionRegistry, ScalarBindArguments},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::InterruptHandle,
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

struct Arguments(DataType);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for Arguments {
    fn len(&self) -> usize {
        1
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        if index == 0 {
            Ok(self.0.clone())
        } else {
            Err(Error::Bind("index".into()))
        }
    }
    fn constant(&self, _: usize) -> Result<Value> {
        Err(Error::Bind("column".into()))
    }
    fn is_provably_null(&self, index: usize) -> Result<bool> {
        self.data_type(index).map(|_| false)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn absolute_retains_logical_bounds_decimal_metadata_and_float_bits() -> Result<()> {
    let functions = FunctionRegistry::builtins();
    let root = functions.scalar("abs")?;
    let types = builtin_types();
    let query = QueryContext::background();
    for (ty, bits) in [
        (DataType::TinyInt, 8),
        (DataType::SmallInt, 16),
        (DataType::Integer, 32),
        (DataType::BigInt, 64),
        (DataType::HugeInt, 128),
        (DataType::UTinyInt, 8),
        (DataType::USmallInt, 16),
        (DataType::UInteger, 32),
        (DataType::UBigInt, 64),
        (DataType::UHugeInt, 128),
    ] {
        let bound = root.bind(&Arguments(ty.clone()), &query)?.unwrap();
        assert_eq!(bound.return_type(std::slice::from_ref(&ty), &types)?, ty);
        let values = if ty.is_unsigned_integer() {
            let maximum = u128::MAX >> (128 - bits);
            vec![
                Value::Null,
                Value::Unsigned(0),
                Value::Unsigned(1),
                Value::Unsigned(maximum),
            ]
        } else {
            let minimum = i128::MIN >> (128 - bits);
            for values in [
                vec![Value::Integer(minimum)],
                vec![Value::Null, Value::Integer(1), Value::Integer(minimum)],
            ] {
                for value in values {
                    if let Err(error) = bound.evaluate(std::slice::from_ref(&value), &query) {
                        assert!(matches!(error, Error::OutOfRange(_)), "{ty}: {error}");
                        assert_eq!(value, Value::Integer(minimum));
                    } else {
                        assert_ne!(value, Value::Integer(minimum));
                    }
                }
            }
            vec![
                Value::Null,
                Value::Integer(minimum + 1),
                Value::Integer(-1),
                Value::Integer(0),
                Value::Integer(1),
                Value::Integer(-(minimum + 1)),
            ]
        };
        let flat = Vector::flat(ty.clone(), values)?;
        for vector in [
            flat.clone(),
            flat.slice(1, flat.len() - 1)?,
            Arc::new(flat.clone()).select((0..flat.len()).rev().collect())?,
            Vector::constant(ty.clone(), flat.values().last().unwrap().clone(), 3)?,
        ] {
            for value in vector.values() {
                let expected = match value {
                    Value::Integer(v) => Value::Integer(i128::try_from(v.unsigned_abs()).unwrap()),
                    other => other.clone(),
                };
                assert_eq!(
                    bound.evaluate(std::slice::from_ref(value), &query)?,
                    expected
                );
            }
        }
        assert!(matches!(
            bound.evaluate(&[Value::Double(1.0)], &query),
            Err(Error::Internal(_))
        ));
    }
    for width in 1..=38 {
        let limit = 10_i128.pow(u32::from(width));
        for scale in 0..=width {
            let ty = DataType::Decimal { width, scale };
            let bound = root.bind(&Arguments(ty.clone()), &query)?.unwrap();
            assert_eq!(bound.return_type(std::slice::from_ref(&ty), &types)?, ty);
            for value in [-limit + 1, -1, 0, 1, limit - 1] {
                assert_eq!(
                    bound.evaluate(&[decimal(value, width, scale)?], &query)?,
                    decimal(value.unsigned_abs() as i128, width, scale)?
                );
            }
            assert_eq!(bound.evaluate(&[Value::Null], &query)?, Value::Null);
        }
    }
    for ty in [DataType::Float, DataType::Double] {
        let bound = root.bind(&Arguments(ty.clone()), &query)?.unwrap();
        for bits in [
            0,
            1,
            0x3ff0000000000000,
            0x7ff0000000000000,
            0x7ff8000000000001,
            0x8000000000000000,
            0x8000000000000001,
            0xbff0000000000000,
            0xfff0000000000000,
            0xfff8000000000001,
        ] {
            let v = f64::from_bits(bits);
            let value = if ty == DataType::Float {
                Value::Float(v as f32)
            } else {
                Value::Double(v)
            };
            let output = bound.evaluate(std::slice::from_ref(&value), &query)?;
            match (value, output) {
                (Value::Float(a), Value::Float(b)) => {
                    assert_eq!(b.to_bits(), a.to_bits() & 0x7fffffff)
                }
                (Value::Double(a), Value::Double(b)) => {
                    assert_eq!(b.to_bits(), a.to_bits() & 0x7fffffffffffffff)
                }
                _ => unreachable!(),
            }
        }
    }
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
    interrupt.interrupt();
    assert!(matches!(
        root.bind(&Arguments(DataType::Integer), &cancelled),
        Err(Error::Interrupted)
    ));
    let bound = root.bind(&Arguments(DataType::TinyInt), &query)?.unwrap();
    assert!(matches!(
        bound.evaluate(&[Value::Null], &cancelled),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        bound.evaluate(&[], &query),
        Err(Error::Internal(_))
    ));
    assert!(matches!(
        bound.evaluate(&[Value::Integer(128)], &query),
        Err(Error::Internal(_))
    ));
    Ok(())
}

#[derive(Debug)]
struct SelectedCast;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedCast {
    fn name(&self) -> &'static str {
        "selected-absolute-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Bignum && spec.target == DataType::Double
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if value.to_string() == "999" {
            return Err(Error::Resource("selected ABS cast failure".into()));
        }
        Ok(Value::Double(-7.25))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn absolute_sql_overloads_selected_casts_and_lazy_decimal_probes() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer.clone())
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(c.query("SELECT typeof(abs(NULL)),typeof(abs(NULL::DECIMAL(4,2))),typeof(abs(NULL::TINYINT)),typeof(abs('-1'::BIGNUM)),abs('-0.0'::FLOAT)::VARCHAR,abs('-0.0'::DOUBLE)::VARCHAR")?.rows,
                vec![vec![Value::Varchar("BIGINT".into()),Value::Varchar("\"NULL\"".into()),Value::Varchar("TINYINT".into()),Value::Varchar("DOUBLE".into()),Value::Varchar("0.0".into()),Value::Varchar("0.0".into())]]);
            assert_eq!(c.query("SELECT abs(x),typeof(abs(x)) FROM (VALUES (NULL::DECIMAL(4,2)),(-1.25::DECIMAL(4,2)))t(x)")?.rows,
                vec![vec![Value::Null,Value::Varchar("DECIMAL(4,2)".into())],vec![decimal(125,4,2)?,Value::Varchar("DECIMAL(4,2)".into())]]);
            assert_eq!(
                c.query("SELECT CASE WHEN false THEN abs(CAST('bad' AS DECIMAL(4,2))) ELSE 1 END")?
                    .rows,
                vec![vec![decimal(100, 12, 2)?]]
            );
            assert_eq!(c.query("SELECT [abs(-1.25::DOUBLE),NULL]::VARCHAR,{'d':abs(-1.25::DECIMAL(4,2))}::VARCHAR,concat(abs('-0.0'::FLOAT))")?.rows,
                vec![vec![Value::Varchar("[1.25, NULL]".into()),Value::Varchar("{'d': 1.25}".into()),Value::Varchar("0.0".into())]]);
            for (ty, minimum) in [
                ("TINYINT", i8::MIN as i128),
                ("SMALLINT", i16::MIN as i128),
                ("INTEGER", i32::MIN as i128),
                ("BIGINT", i64::MIN as i128),
                ("HUGEINT", i128::MIN),
            ] {
                assert!(matches!(
                    c.query(&format!("SELECT abs('{minimum}'::{ty})")),
                    Err(Error::OutOfRange(_))
                ));
                assert!(matches!(
                    c.query(&format!(
                        "SELECT abs(x) FROM (VALUES (1::{ty}),('{minimum}'::{ty})) t(x)"
                    )),
                    Err(Error::OutOfRange(_))
                ));
                assert_eq!(
                    c.query(&format!(
                        "SELECT CASE WHEN false THEN abs('{minimum}'::{ty}) ELSE 1 END"
                    ))?
                    .rows,
                    vec![vec![Value::Integer(1)]]
                );
            }
            let prepared = c.prepare("SELECT abs($1),typeof(abs($1))")?;
            assert_eq!(
                c.execute_prepared(&prepared, &[Value::Integer(-128)])?.rows,
                vec![vec![Value::Integer(128), Value::Varchar("INTEGER".into())]]
            );
            assert_eq!(
                c.execute_prepared(&prepared, &[decimal(-125, 4, 2)?])?.rows,
                vec![vec![
                    decimal(125, 4, 2)?,
                    Value::Varchar("DECIMAL(4,2)".into())
                ]]
            );
            for argument in [
                "'1'",
                "'1'::VARCHAR",
                "TRUE",
                "'1'::ENUM('1')",
                "[1]",
                "DATE '2024-01-01'",
                "",
                "1,2",
            ] {
                assert!(matches!(
                    c.query(&format!("SELECT abs({argument})")),
                    Err(Error::Bind(_))
                ));
            }
            let mut casts = CastRegistry::builtins();
            casts.replace(
                CastSpec {
                    source: DataType::Bignum,
                    target: DataType::Double,
                    mode: CastMode::Implicit,
                },
                Arc::new(SelectedCast),
            )?;
            let mut selected = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .casts(casts)
                .build()?
                .connect();
            assert_eq!(
                selected.query("SELECT abs('1'::BIGNUM)")?.rows,
                vec![vec![Value::Double(7.25)]]
            );
            assert!(matches!(
                selected.query("SELECT TRY_CAST(abs('999'::BIGNUM) AS VARCHAR)"),
                Err(Error::Resource(_))
            ));
            assert_eq!(
                selected
                    .query("SELECT CASE WHEN false THEN abs('999'::BIGNUM) ELSE 1 END")?
                    .rows,
                vec![vec![Value::Double(1.0)]]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn absolute_values_cross_keys_relations_atomic_mutation_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (index, format) in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ]
    .into_iter()
    .enumerate()
    {
        for expressions in [
            Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
            Arc::new(BatchedEvaluator),
        ] {
            let path = directory
                .path()
                .join(format!("absolute-{index}-{}.db", expressions.name()));
            let open = || {
                DatabaseBuilder::new()
                    .expressions(expressions.clone())
                    .batch_size(2)
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format.clone(),
                    )?))
                    .build()
            };
            {
                let mut c = open()?.connect();
                c.execute("CREATE TABLE t(k TINYINT PRIMARY KEY,s TINYINT,d DECIMAL(38,2),n DECIMAL(38,2)[]); INSERT INTO t SELECT abs(k),s,abs(d),[abs(d),NULL] FROM (VALUES (-1::TINYINT,-127::TINYINT,-1.25::DECIMAL(38,2)),(-2,-128,-2.25)) v(k,s,d)")?;
                assert_eq!(c.query("SELECT a.k,count(*),sum(abs(a.d)) FROM t a JOIN t b ON a.k=abs(b.k) GROUP BY a.k ORDER BY a.k")?.rows,
                    vec![vec![Value::Integer(1),Value::Integer(1),decimal(125,38,2)?],vec![Value::Integer(2),Value::Integer(1),decimal(225,38,2)?]]);
                assert_eq!(c.query("SELECT abs(d),sum(abs(d)) OVER(ORDER BY k ROWS UNBOUNDED PRECEDING) FROM t ORDER BY k")?.rows,
                    vec![vec![decimal(125,38,2)?,decimal(125,38,2)?],vec![decimal(225,38,2)?,decimal(350,38,2)?]]);
                assert!(matches!(
                    c.execute("UPDATE t SET s=abs(s)"),
                    Err(Error::OutOfRange(_))
                ));
                assert_eq!(
                    c.query("SELECT s FROM t ORDER BY k")?.rows,
                    vec![vec![Value::Integer(-127)], vec![Value::Integer(-128)]]
                );
                assert!(c.execute("UPDATE t SET k=abs(-1::TINYINT)").is_err());
                c.execute("BEGIN; UPDATE t SET k=abs(-k)+10,d=abs(-d); DELETE FROM t; ROLLBACK; CHECKPOINT")?;
            }
            let mut c = open()?.connect();
            let p = c.prepare("SELECT k,s,abs(d),n[1],n[2] FROM t WHERE k=abs($1::TINYINT)")?;
            assert_eq!(
                c.execute_prepared(&p, &[Value::Integer(-1)])?.rows,
                vec![vec![
                    Value::Integer(1),
                    Value::Integer(-127),
                    decimal(125, 38, 2)?,
                    decimal(125, 38, 2)?,
                    Value::Null
                ]]
            );
        }
    }
    let path = directory.path().join("absolute-wal.duckdb");
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("CREATE TABLE t(k INTEGER PRIMARY KEY,d DECIMAL(38,2),u UHUGEINT,f FLOAT); INSERT INTO t VALUES (abs(-1),abs(-1.25::DECIMAL(38,2)),abs('340282366920938463463374607431768211455'::UHUGEINT),abs('-0.0'::FLOAT)); BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    let mut c = Database::open(&path)?.connect();
    assert_eq!(
        c.query("SELECT k,d,u,f::VARCHAR FROM t")?.rows,
        vec![vec![
            Value::Integer(1),
            decimal(125, 38, 2)?,
            Value::Unsigned(u128::MAX),
            Value::Varchar("0.0".into())
        ]]
    );
    c.execute("CHECKPOINT")?;
    Ok(())
}
