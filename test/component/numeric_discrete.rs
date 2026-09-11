use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn discrete_integer_functions_match_types_aliases_nulls_and_boundaries() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut connection = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(
                connection
                    .query(
                        "SELECT
                       gcd(42,57), greatest_common_divisor(-42,0),
                       lcm(-42,57), least_common_multiple(21,6),
                       factorial(0), factorial(4),
                       typeof(gcd(42,57)),
                       typeof(gcd(42::HUGEINT,57)),
                       typeof(lcm(NULL,NULL)), typeof(factorial(NULL)),
                       gcd(NULL,1), lcm(0,NULL), factorial(NULL)"
                    )?
                    .rows,
                vec![vec![
                    Value::Integer(3),
                    Value::Integer(42),
                    Value::Integer(798),
                    Value::Integer(42),
                    Value::Integer(1),
                    Value::Integer(24),
                    Value::Varchar("BIGINT".into()),
                    Value::Varchar("HUGEINT".into()),
                    Value::Varchar("BIGINT".into()),
                    Value::Varchar("HUGEINT".into()),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ]]
            );
            assert!(matches!(
                connection.query("SELECT factorial(-1)"),
                Err(Error::OutOfRange(message)) if message.contains("negative number")
            ));
            assert!(matches!(
                connection.query("SELECT factorial(34)"),
                Err(Error::OutOfRange(message)) if message.contains("Value out of range")
            ));
            assert_eq!(
                connection.query("SELECT factorial(33)")?.rows,
                vec![vec![Value::Integer(
                    8_683_317_618_811_886_495_518_194_401_280_000_000
                )]]
            );
            assert_eq!(
                connection
                    .query(
                        "SELECT gcd('-9223372036854775808'::BIGINT,-1::BIGINT),
                            gcd('-170141183460469231731687303715884105728'::HUGEINT,-1::HUGEINT),
                            lcm(0::BIGINT,'-9223372036854775808'::BIGINT)"
                    )?
                    .rows,
                vec![vec![
                    Value::Integer(1),
                    Value::Integer(1),
                    Value::Integer(0),
                ]]
            );
            for sql in [
                "SELECT gcd('-9223372036854775808'::BIGINT,0::BIGINT)",
                "SELECT gcd('-170141183460469231731687303715884105728'::HUGEINT,0::HUGEINT)",
                "SELECT lcm('-9223372036854775808'::BIGINT,1::BIGINT)",
                "SELECT lcm('-170141183460469231731687303715884105728'::HUGEINT,1::HUGEINT)",
            ] {
                assert!(
                    matches!(connection.query(sql), Err(Error::OutOfRange(message)) if message.contains("Overflow on abs")),
                    "{sql}"
                );
            }
            assert!(matches!(
                connection.query("SELECT lcm(9223372036854775807::BIGINT,2::BIGINT)"),
                Err(Error::OutOfRange(message)) if message.contains("lcm value is out of range")
            ));

            let prepared = connection.prepare(
                "SELECT gcd($1,$2),lcm($1,$2),factorial($3),
                        typeof(gcd($1,$2)),typeof(factorial($3))",
            )?;
            assert_eq!(
                connection
                    .execute_prepared(
                        &prepared,
                        &[Value::Integer(42), Value::Integer(57), Value::Integer(5),],
                    )?
                    .rows,
                vec![vec![
                    Value::Integer(3),
                    Value::Integer(798),
                    Value::Integer(120),
                    Value::Varchar("BIGINT".into()),
                    Value::Varchar("HUGEINT".into()),
                ]]
            );

            connection.execute(
                "CREATE TABLE inputs(a BIGINT,b BIGINT);
                 INSERT INTO inputs VALUES (42,57),(-42,0),(NULL,3);
                 UPDATE inputs SET b=gcd(a,b) WHERE a IS NOT NULL",
            )?;
            assert_eq!(
                connection
                    .query("SELECT a,b,lcm(a,b) FROM inputs ORDER BY a NULLS LAST")?
                    .rows,
                vec![
                    vec![Value::Integer(-42), Value::Integer(42), Value::Integer(42)],
                    vec![Value::Integer(42), Value::Integer(3), Value::Integer(42)],
                    vec![Value::Null, Value::Integer(3), Value::Null],
                ]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn discrete_integer_results_survive_logged_mutation_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("discrete.duckdb");
    {
        let mut connection = Database::open_logged(&path)?.connect();
        connection.execute(
            "CREATE TABLE results(id INTEGER PRIMARY KEY,g BIGINT,l HUGEINT,f HUGEINT);
             INSERT INTO results VALUES
               (1,gcd(42,57),lcm(42::HUGEINT,57),factorial(4));
             UPDATE results SET g=greatest_common_divisor(84,30),
                                l=least_common_multiple(21::HUGEINT,6),
                                f=factorial(5)
             WHERE id=1",
        )?;
    }
    assert_eq!(
        Database::open_logged(&path)?
            .connect()
            .query("SELECT * FROM results")?
            .rows,
        vec![vec![
            Value::Integer(1),
            Value::Integer(6),
            Value::Integer(42),
            Value::Integer(120),
        ]]
    );
    Ok(())
}
