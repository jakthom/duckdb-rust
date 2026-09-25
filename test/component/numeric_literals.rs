use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    common::BignumValue,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bare_numeric_literals_preserve_signed_unsigned_and_unbounded_integer_domains() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            for (text, ty) in [
                ("2147483647", DataType::Integer),
                ("2147483648", DataType::BigInt),
                ("-2147483648", DataType::Integer),
                ("-2147483649", DataType::BigInt),
                ("9223372036854775807", DataType::BigInt),
                ("9223372036854775808", DataType::HugeInt),
                ("-9223372036854775808", DataType::BigInt),
                ("-9223372036854775809", DataType::HugeInt),
                ("170141183460469231731687303715884105727", DataType::HugeInt),
                (
                    "170141183460469231731687303715884105728",
                    DataType::UHugeInt,
                ),
                (
                    "-170141183460469231731687303715884105728",
                    DataType::HugeInt,
                ),
                ("-170141183460469231731687303715884105729", DataType::Bignum),
                (
                    "340282366920938463463374607431768211455",
                    DataType::UHugeInt,
                ),
                ("340282366920938463463374607431768211456", DataType::Bignum),
                ("-340282366920938463463374607431768211455", DataType::Bignum),
            ] {
                let result = c.query(&format!("SELECT {text},typeof({text}),({text})::VARCHAR"))?;
                assert_eq!(result.columns[0].data_type, ty, "{text}");
                let expected = match ty {
                    DataType::Bignum => BignumValue::parse(text, || Ok(()))?.value(),
                    DataType::UHugeInt => Value::Unsigned(text.parse().unwrap()),
                    _ => Value::Integer(text.parse().unwrap()),
                };
                assert_eq!(
                    result.rows,
                    vec![vec![
                        expected,
                        Value::Varchar(ty.to_string()),
                        Value::Varchar(text.into())
                    ]],
                    "{text}"
                );
            }
            assert_eq!(c.query("SELECT trunc(340282366920938463463374607431768211455::UHUGEINT,-38),abs(340282366920938463463374607431768211455),340282366920938463463374607431768211456+1::BIGNUM")?.rows,
                vec![vec![Value::Unsigned(300000000000000000000000000000000000000),Value::Unsigned(u128::MAX),BignumValue::parse("340282366920938463463374607431768211457",||Ok(()))?.value()]]);
            assert_eq!(c.query("SELECT (CASE WHEN true THEN 340282366920938463463374607431768211455 ELSE 0::UHUGEINT END)::VARCHAR,[340282366920938463463374607431768211455,NULL]::VARCHAR")?.rows,
                vec![vec![Value::Varchar(u128::MAX.to_string()),Value::Varchar(format!("[{}, NULL]",u128::MAX))]]);
            let p = c.prepare("SELECT 340282366920938463463374607431768211455=$1,340282366920938463463374607431768211456=$2")?;
            assert_eq!(
                c.execute_prepared(
                    &p,
                    &[
                        Value::Unsigned(u128::MAX),
                        BignumValue::parse("340282366920938463463374607431768211456", || Ok(()))?
                            .value()
                    ]
                )?
                .rows,
                vec![vec![Value::Boolean(true), Value::Boolean(true)]]
            );
            let digits = "1234567890".repeat(100);
            assert_eq!(
                c.query(&format!("SELECT ({digits})::VARCHAR,(-{digits})::VARCHAR"))?
                    .rows,
                vec![vec![
                    Value::Varchar(digits.clone()),
                    Value::Varchar(format!("-{digits}"))
                ]]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn full_width_literal_keys_and_values_survive_atomic_mutations_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (index, format) in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory
            .path()
            .join(format!("numeric-literals-{index}.db"));
        let open = || {
            DatabaseBuilder::new()
                .durability(Arc::new(FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    format.clone(),
                )?))
                .build()
        };
        {
            let mut c = open()?.connect();
            c.execute("CREATE TABLE t(u UHUGEINT PRIMARY KEY,b BIGNUM); INSERT INTO t VALUES (340282366920938463463374607431768211455,340282366920938463463374607431768211456),(170141183460469231731687303715884105728,-170141183460469231731687303715884105729)")?;
            assert!(
                c.execute("UPDATE t SET u=340282366920938463463374607431768211455")
                    .is_err()
            );
            c.execute("BEGIN; UPDATE t SET u=u-1::UHUGEINT,b=b+1::BIGNUM; ROLLBACK; CHECKPOINT")?;
        }
        let mut c = open()?.connect();
        let p = c.prepare("SELECT u::VARCHAR,b::VARCHAR FROM t WHERE u=$1")?;
        assert_eq!(
            c.execute_prepared(&p, &[Value::Unsigned(u128::MAX)])?.rows,
            vec![vec![
                Value::Varchar(u128::MAX.to_string()),
                Value::Varchar("340282366920938463463374607431768211456".into())
            ]]
        );
        assert_eq!(
            c.query("SELECT a.b::VARCHAR FROM t a JOIN t b USING(u) ORDER BY a.u")?
                .rows,
            vec![
                vec![Value::Varchar(
                    "-170141183460469231731687303715884105729".into()
                )],
                vec![Value::Varchar(
                    "340282366920938463463374607431768211456".into()
                )]
            ]
        );
    }
    let path = directory.path().join("numeric-literal-wal.duckdb");
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("CREATE TABLE t(u UHUGEINT PRIMARY KEY,b BIGNUM); INSERT INTO t VALUES (340282366920938463463374607431768211455,-170141183460469231731687303715884105729); BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    let mut c = Database::open(&path)?.connect();
    assert_eq!(
        c.query("SELECT u::VARCHAR,b::VARCHAR FROM t")?.rows,
        vec![vec![
            Value::Varchar(u128::MAX.to_string()),
            Value::Varchar("-170141183460469231731687303715884105729".into())
        ]]
    );
    c.execute("CHECKPOINT")?;
    Ok(())
}
