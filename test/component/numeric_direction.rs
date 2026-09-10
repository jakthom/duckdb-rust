use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    common::{
        cast::{CastFunction, CastSpec},
        vector::Vector,
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::FunctionRegistry,
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::InterruptHandle,
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_directions_preserve_decimal_widths_and_floating_boundaries() -> Result<()> {
    let functions = FunctionRegistry::builtins();
    let types = builtin_types();
    let query = QueryContext::background();
    for name in ["ceil", "ceiling", "floor"] {
        let function = functions.scalar(name)?;
        for width in 1..=38 {
            let limit = 10_i128.pow(u32::from(width));
            for scale in 0..=width {
                let power = 10_i128.pow(u32::from(scale));
                let source = DataType::Decimal { width, scale };
                let target = DataType::Decimal { width, scale: 0 };
                assert_eq!(
                    function.return_type(std::slice::from_ref(&source), &types)?,
                    target
                );
                let mut values = vec![Value::Null];
                for coefficient in [
                    -limit + 1,
                    -power - 1,
                    -power,
                    -power + 1,
                    -1,
                    0,
                    1,
                    power - 1,
                    power,
                    power + 1,
                    limit - 1,
                ] {
                    if coefficient <= -limit || coefficient >= limit {
                        continue;
                    }
                    values.push(decimal(coefficient, width, scale)?);
                }
                let flat = Vector::flat(source.clone(), values)?;
                for vector in [
                    flat.clone(),
                    flat.slice(1, flat.len() - 1)?,
                    Arc::new(flat.clone()).select((0..flat.len()).rev().collect())?,
                    Vector::constant(source, decimal(1, width, scale)?, 3)?,
                ] {
                    for value in vector.values() {
                        let actual = function.evaluate(std::slice::from_ref(value), &query)?;
                        let expected = match value {
                            Value::Null => Value::Null,
                            Value::Decimal { value, .. } => {
                                // Independent Euclidean quotient/remainder oracle.
                                let lower = value.div_euclid(power);
                                decimal(
                                    lower
                                        + i128::from(
                                            name != "floor" && value.rem_euclid(power) != 0,
                                        ),
                                    width,
                                    0,
                                )?
                            }
                            _ => unreachable!(),
                        };
                        assert_eq!(
                            actual, expected,
                            "{name}({value}) width={width}, scale={scale}"
                        );
                    }
                }
            }
        }
        for (input, ceiling, floor) in [
            (-1.25, -1.0, -2.0),
            (-1.0, -1.0, -1.0),
            (-0.25, -0.0, -1.0),
            (-0.0, -0.0, -0.0),
            (0.0, 0.0, 0.0),
            (0.25, 1.0, 0.0),
            (1.25, 2.0, 1.0),
            (f64::INFINITY, f64::INFINITY, f64::INFINITY),
            (f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
        ] {
            let expected = if name == "floor" { floor } else { ceiling };
            for value in [Value::Float(input as f32), Value::Double(input)] {
                let actual = function.evaluate(std::slice::from_ref(&value), &query)?;
                assert_eq!(actual.data_type(), value.data_type());
                assert_eq!(actual.as_f64()?.to_bits(), expected.to_bits());
            }
        }
        for value in [Value::Float(f32::NAN), Value::Double(f64::NAN)] {
            assert!(function.evaluate(&[value], &query)?.as_f64()?.is_nan());
        }
        assert!(matches!(
            function.evaluate(&[Value::Integer(1)], &query),
            Err(Error::Internal(_))
        ));
    }
    let sign = functions.scalar("sign")?;
    for (input, expected) in [
        (Value::Integer(i128::MIN), -1),
        (Value::Integer(i128::MAX), 1),
        (Value::Unsigned(u128::MAX), 1),
        (Value::Unsigned(0), 0),
        (Value::Float(-0.0), 0),
        (Value::Double(f64::NAN), 0),
        (Value::Float(f32::NAN), 0),
        (Value::Double(f64::NEG_INFINITY), -1),
        (Value::Float(f32::INFINITY), 1),
    ] {
        assert_eq!(sign.evaluate(&[input], &query)?, Value::Integer(expected));
    }
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
    interrupt.interrupt();
    for name in ["ceil", "ceiling", "floor", "sign"] {
        let function = functions.scalar(name)?;
        assert!(matches!(
            function.evaluate(&[Value::Null], &cancelled),
            Err(Error::Interrupted)
        ));
        assert!(matches!(
            function.evaluate(&[], &query),
            Err(Error::Internal(_))
        ));
        assert!(matches!(
            function.argument_types(&[], &types),
            Err(Error::Bind(_))
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedDirectionCast;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedDirectionCast {
    fn name(&self) -> &'static str {
        "selected-direction-double"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer && spec.target == DataType::Double
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if *value == Value::Integer(999) {
            return Err(Error::Resource("selected direction failure".into()));
        }
        Ok(Value::Double(7.25))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_directions_bind_exact_types_and_selected_casts() -> Result<()> {
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
            assert_eq!(c.query("SELECT ceil(1.25::DECIMAL(38,2)),floor(-1.25::DECIMAL(38,2)),sign(-1.25::DECIMAL(38,2)),ceil(1::INTEGER),floor(1::FLOAT),sign('340282366920938463463374607431768211455'::UHUGEINT),sign(NULL),ceil(NULL),floor(NULL)")?.rows,vec![vec![decimal(2,38,0)?,decimal(-2,38,0)?,Value::Integer(-1),Value::Double(1.0),Value::Float(1.0),Value::Integer(1),Value::Null,Value::Null,Value::Null]]);
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
                "BIGNUM",
            ] {
                assert_eq!(
                    c.query(&format!(
                        "SELECT typeof(ceil(1::{ty})),typeof(floor(1::{ty})),typeof(sign(1::{ty}))"
                    ))?
                    .rows,
                    vec![vec![
                        Value::Varchar("DOUBLE".into()),
                        Value::Varchar("DOUBLE".into()),
                        Value::Varchar("TINYINT".into())
                    ]]
                );
            }
            assert_eq!(c.query("SELECT typeof(ceil(NULL)),typeof(sign(NULL)),ceil('-0.0'::FLOAT)::VARCHAR,floor('-0.0'::DOUBLE)::VARCHAR,concat(ceil(1.25::FLOAT)),[floor(-1.25::DOUBLE),NULL]::VARCHAR")?.rows,vec![vec![Value::Varchar("DOUBLE".into()),Value::Varchar("TINYINT".into()),Value::Varchar("-0.0".into()),Value::Varchar("-0.0".into()),Value::Varchar("2.0".into()),Value::Varchar("[-2.0, NULL]".into())]]);
            for function in ["ceil", "ceiling", "floor", "sign"] {
                for argument in [
                    "'1.25'",
                    "'1.25'::VARCHAR",
                    "TRUE",
                    "'1.25'::ENUM('1.25')",
                    "[1]",
                    "DATE '2024-01-01'",
                    "1,2",
                    "",
                ] {
                    assert!(
                        matches!(
                            c.query(&format!("SELECT {function}({argument})")),
                            Err(Error::Bind(_))
                        ),
                        "{function}({argument})"
                    );
                }
            }
            assert_eq!(
                c.execute_params(
                    "SELECT ceil($1),floor($1),sign($1)",
                    &[decimal(-125, 5, 2)?]
                )?[0]
                    .rows,
                vec![vec![
                    decimal(-1, 5, 0)?,
                    decimal(-2, 5, 0)?,
                    Value::Integer(-1)
                ]]
            );
            assert!(matches!(
                c.execute_params("SELECT ceil($1)", &[Value::Varchar("1.25".into())]),
                Err(Error::Bind(_))
            ));
            let mut casts = CastRegistry::builtins();
            for mode in [CastMode::Implicit, CastMode::Explicit] {
                casts.replace(
                    CastSpec {
                        source: DataType::Integer,
                        target: DataType::Double,
                        mode,
                    },
                    Arc::new(SelectedDirectionCast),
                )?;
            }
            let mut selected = DatabaseBuilder::new()
                .casts(casts)
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            assert_eq!(
                selected
                    .query("SELECT ceil(1::INTEGER),floor(1),ceil(1.1::DOUBLE),sign(1::INTEGER)")?
                    .rows,
                vec![vec![
                    Value::Double(8.0),
                    Value::Double(7.0),
                    Value::Double(2.0),
                    Value::Integer(1)
                ]]
            );
            assert!(matches!(
                selected.query("SELECT TRY_CAST(ceil(999::INTEGER) AS VARCHAR)"),
                Err(Error::Resource(_))
            ));
            assert_eq!(
                selected
                    .query("SELECT CASE WHEN false THEN ceil(999::INTEGER) ELSE floor(1) END")?
                    .rows,
                vec![vec![Value::Double(7.0)]]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_directions_cross_relations_mutation_rollback_and_reopen() -> Result<()> {
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
                .join(format!("direction-{index}-{}.db", expressions.name()));
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
                c.execute("CREATE TABLE t(k DECIMAL(8,0) PRIMARY KEY,d DECIMAL(8,2),s TINYINT,n DECIMAL(8,0)[]); INSERT INTO t SELECT floor(d),d,sign(d),[ceil(d),floor(d),NULL] FROM (VALUES (-1.25::DECIMAL(8,2)),(0.00),(1.25)) v(d); BEGIN; DELETE FROM t; ROLLBACK")?;
                assert_eq!(c.query("SELECT a.k,count(*),sum(sign(a.d)) FROM t a JOIN t b ON a.k=floor(b.d) GROUP BY a.k ORDER BY a.k")?.rows,vec![vec![decimal(-2,8,0)?,Value::Integer(1),Value::Integer(-1)],vec![decimal(0,8,0)?,Value::Integer(1),Value::Integer(0)],vec![decimal(1,8,0)?,Value::Integer(1),Value::Integer(1)]]);
                assert_eq!(c.query("SELECT ceil(d),sum(sign(d)) OVER(ORDER BY d ROWS UNBOUNDED PRECEDING) FROM t ORDER BY d")?.rows,vec![vec![decimal(-1,8,0)?,Value::Integer(-1)],vec![decimal(0,8,0)?,Value::Integer(-1)],vec![decimal(2,8,0)?,Value::Integer(0)]]);
                assert!(c.execute("UPDATE t SET k=floor(0::DECIMAL(8,2))").is_err());
                assert!(matches!(
                    c.execute("UPDATE t SET s=ceil(999::DOUBLE)"),
                    Err(Error::Conversion(_))
                ));
                c.execute("BEGIN; UPDATE t SET d=ceil(d),s=sign(d); ROLLBACK; CHECKPOINT")?;
            }
            let mut c = open()?.connect();
            let p = c.prepare("SELECT k,d,s,n[1],n[2],n[3] FROM t WHERE k=floor($1)")?;
            assert_eq!(
                c.execute_prepared(&p, &[decimal(125, 8, 2)?])?.rows,
                vec![vec![
                    decimal(1, 8, 0)?,
                    decimal(125, 8, 2)?,
                    Value::Integer(1),
                    decimal(2, 8, 0)?,
                    decimal(1, 8, 0)?,
                    Value::Null
                ]]
            );
        }
    }
    let path = directory.path().join("direction-wal.duckdb");
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("CREATE TABLE t(k INTEGER PRIMARY KEY,d DECIMAL(38,3),f FLOAT,s TINYINT); INSERT INTO t VALUES (1,ceil(1.125::DECIMAL(38,3)),floor(-1.25::FLOAT),sign(-1::HUGEINT)); BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    let mut c = Database::open(&path)?.connect();
    assert_eq!(
        c.query("SELECT d,f,s,ceil(d),floor(f),sign(d) FROM t")?
            .rows,
        vec![vec![
            decimal(2000, 38, 3)?,
            Value::Float(-2.0),
            Value::Integer(-1),
            decimal(2, 38, 0)?,
            Value::Float(-2.0),
            Value::Integer(1)
        ]]
    );
    c.execute("CHECKPOINT")?;
    Ok(())
}
