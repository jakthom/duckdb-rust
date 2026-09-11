use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    common::{cast::numeric::ExactNumericCast, vector::Vector},
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn expected(value: f64, target: &DataType) -> Option<Value> {
    if !value.is_finite() {
        return None;
    }
    // Independent half-interval oracle; do not use the production rounding
    // intrinsic in the expected-result calculation.
    let lower = value.floor();
    let fraction = value - lower;
    let rounded = if fraction > 0.5 || (fraction == 0.5 && lower % 2.0 != 0.0) {
        lower + 1.0
    } else {
        lower
    };
    if let Some(width) = target.integer_bits() {
        let limit = 2_f64.powi(i32::from(width) - 1);
        if rounded < -limit
            || rounded >= limit
            || (*target == DataType::HugeInt && rounded == -limit)
        {
            None
        } else {
            Some(Value::Integer(rounded as i128))
        }
    } else {
        let limit = 2_f64.powi(i32::from(target.unsigned_bits().unwrap()));
        if rounded < 0.0 || rounded >= limit {
            None
        } else {
            Some(Value::Unsigned(rounded as u128))
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn floating_integral_casts_round_ties_even_before_checking_every_width() -> Result<()> {
    let types = builtin_types();
    let query = QueryContext::background();
    for target in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
        DataType::UTinyInt,
        DataType::USmallInt,
        DataType::UInteger,
        DataType::UBigInt,
        DataType::UHugeInt,
    ] {
        for source in [DataType::Float, DataType::Double] {
            // Also select the exact-numeric adapter for signed targets to
            // verify that both registered implementations share the contract.
            for replace in [false, true] {
                let mut casts = CastRegistry::builtins();
                if replace {
                    casts.replace(
                        duckdb_rust::common::cast::CastSpec {
                            source: source.clone(),
                            target: target.clone(),
                            mode: CastMode::Explicit,
                        },
                        Arc::new(ExactNumericCast),
                    )?;
                }
                let bound = casts.bind(&source, &target, CastMode::Explicit, &types)?;
                let width = target
                    .integer_bits()
                    .or_else(|| target.unsigned_bits())
                    .unwrap();
                let limit = 2_f64.powi(i32::from(width) - i32::from(target.is_signed_integer()));
                let adjacent = if source == DataType::Float {
                    f64::from((limit as f32).next_down())
                } else {
                    limit.next_down()
                };
                let inputs = [
                    -3.5,
                    -2.5,
                    -1.5,
                    -0.5,
                    -0.0,
                    0.0,
                    0.5,
                    1.5,
                    2.5,
                    3.5,
                    60.5,
                    61.5,
                    127.4,
                    127.5,
                    127.6,
                    -128.4,
                    -128.5,
                    -128.6,
                    255.4,
                    255.5,
                    255.6,
                    -limit,
                    -adjacent,
                    adjacent,
                    limit,
                    f64::NAN,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                ];
                let mut passing = vec![Value::Null];
                for value in inputs {
                    let input = if source == DataType::Float {
                        Value::Float(value as f32)
                    } else {
                        Value::Double(value)
                    };
                    match expected(input.as_f64()?, &target) {
                        Some(result) => {
                            assert_eq!(
                                bound.apply(&input, &query)?,
                                result,
                                "{source} {input} -> {target}, alternate={replace}"
                            );
                            passing.push(input);
                        }
                        None => assert!(
                            matches!(bound.apply(&input, &query), Err(Error::Conversion(_))),
                            "{source} {input} -> {target}, alternate={replace}"
                        ),
                    }
                }
                let flat = Vector::flat(source.clone(), passing.clone())?;
                for vector in [
                    flat.clone(),
                    Arc::new(flat).select((0..passing.len()).rev().collect())?,
                    Vector::constant(source.clone(), passing[1].clone(), 33)?,
                ] {
                    assert_eq!(
                        bound
                            .apply_batch(&vector, &query)?
                            .values()
                            .cloned()
                            .collect::<Vec<_>>(),
                        vector
                            .values()
                            .map(|value| bound.apply(value, &query))
                            .collect::<Result<Vec<_>>>()?
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn floating_rounding_flows_through_parameters_keys_mutations_windows_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
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
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(c.query("SELECT ('60.5'::DOUBLE)::INTEGER,('61.5'::DOUBLE)::INTEGER,('-2.5'::FLOAT)::BIGINT,('2.5'::DOUBLE)::HUGEINT,('-0.5'::DOUBLE)::UHUGEINT,('2.5'::DOUBLE)::DECIMAL(2,0),2.5::INTEGER,round('2.5'::DOUBLE)")?.rows,
                vec![vec![Value::Integer(60),Value::Integer(62),Value::Integer(-2),Value::Integer(2),Value::Unsigned(0),decimal(3,2,0)?,Value::Integer(3),Value::Double(3.0)]]);
            c.execute(
                "CREATE TABLE f(x DOUBLE); INSERT INTO f VALUES (0.5),(2.5),(3.5),(-2.5),(NULL)",
            )?;
            assert_eq!(c.query("SELECT x::INTEGER,sum(x::INTEGER) OVER(ORDER BY x ROWS UNBOUNDED PRECEDING) FROM f ORDER BY x")?.rows,
                vec![vec![Value::Integer(-2),Value::Integer(-2)],vec![Value::Integer(0),Value::Integer(-2)],vec![Value::Integer(2),Value::Integer(0)],vec![Value::Integer(4),Value::Integer(4)],vec![Value::Null,Value::Integer(4)]]);
            assert_eq!(
                c.execute_params(
                    "SELECT $1::INTEGER,$2::UTINYINT",
                    &[Value::Double(60.5), Value::Float(-0.5)]
                )?[0]
                    .rows,
                vec![vec![Value::Integer(60), Value::Unsigned(0)]]
            );
            assert!(
                c.query("SELECT interval '99999999999999' years")
                    .unwrap_err()
                    .to_string()
                    .contains("out of range for the destination type")
            );
        }
    }
    let path = directory.path().join("float-rounding.duckdb");
    {
        let mut c = Database::open_logged(&path)?.connect();
        c.execute(
            "CREATE TABLE t(k INTEGER PRIMARY KEY,u UTINYINT,s BIGINT,h HUGEINT,uh UHUGEINT)",
        )?;
        c.execute_params(
            "INSERT INTO t VALUES ($1,$2,$3,$4,$5)",
            &[
                Value::Double(0.5),
                Value::Double(-0.5),
                Value::Float(60.5),
                Value::Double(2.5),
                Value::Double(3.5),
            ],
        )?;
        assert!(matches!(
            c.execute_params("INSERT INTO t(k) VALUES ($1)", &[Value::Double(-0.5)]),
            Err(Error::Constraint(_))
        ));
        c.execute("BEGIN; UPDATE t SET u='255.6'::DOUBLE; ROLLBACK")
            .expect_err("overflow must invalidate update, not publish narrowed bytes");
        c.execute("ROLLBACK")?;
        c.execute("BEGIN; UPDATE t SET s='3.5'::DOUBLE; ROLLBACK; UPDATE t SET h='60.5'::DOUBLE")?;
    }
    for checkpoint in [false, true] {
        let mut c = Database::open_logged(&path)?.connect();
        assert_eq!(
            c.query("SELECT k,u,s,h,uh FROM t WHERE k=0")?.rows,
            vec![vec![
                Value::Integer(0),
                Value::Unsigned(0),
                Value::Integer(60),
                Value::Integer(60),
                Value::Unsigned(4)
            ]]
        );
        if checkpoint {
            c.execute("CHECKPOINT")?;
        }
    }
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT h FROM t")?
            .rows,
        vec![vec![Value::Integer(60)]]
    );
    Ok(())
}
