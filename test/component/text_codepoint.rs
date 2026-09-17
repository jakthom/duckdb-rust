use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Error, Result, Value,
    common::{
        DataType,
        vector::{DataChunk, Vector},
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
    planner::{BoundExpr, ExprKind},
};

#[derive(Debug)]
struct UnrelatedContains;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for UnrelatedContains {
    fn name(&self) -> &str {
        "contains"
    }

    fn return_type(
        &self,
        _: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        Ok(DataType::Boolean)
    }

    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }

    fn evaluate(&self, _: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        Ok(Value::Boolean(true))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn batched_contains_selection_uses_builtin_identity_and_exact_offsets() -> Result<()> {
    let query = QueryContext::background();
    let input = DataChunk::new(
        vec![
            Vector::flat(
                DataType::Varchar,
                vec![
                    Value::Varchar("a\0é".into()),
                    Value::Varchar("abc".into()),
                    Value::Null,
                    Value::Varchar("".into()),
                    Value::Varchar("é".into()),
                ],
            )?,
            Vector::flat(
                DataType::Varchar,
                vec![
                    Value::Varchar("\0é".into()),
                    Value::Varchar("z".into()),
                    Value::Varchar("".into()),
                    Value::Varchar("".into()),
                    Value::Varchar("é".into()),
                ],
            )?,
        ],
        5,
    )?;
    let arguments = vec![
        BoundExpr::column(0, DataType::Varchar),
        BoundExpr::column(1, DataType::Varchar),
    ];
    let builtin = BoundExpr {
        kind: ExprKind::Scalar(
            FunctionRegistry::builtins().scalar("contains")?,
            arguments.clone(),
        ),
        data_type: DataType::Boolean,
    };
    assert_eq!(
        BatchedEvaluator.select_batch(&builtin, &input, &query)?,
        vec![0, 3, 4]
    );

    // Matching SQL spelling is not a built-in identity. An external adapter
    // must retain its own callback semantics in predicate mode.
    let unrelated = BoundExpr {
        kind: ExprKind::Scalar(Arc::new(UnrelatedContains), arguments),
        data_type: DataType::Boolean,
    };
    assert_eq!(
        BatchedEvaluator.select_batch(&unrelated, &input, &query)?,
        vec![0, 1, 2, 3, 4]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn codepoint_varchar_functions_are_nul_safe_and_prepared() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(evaluator)
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(connection.query("SELECT chr(0),chr(128169),ascii(''),ascii('\0'),ascii('é'),contains('a\0é','\0é'),contains('abc',''),contains(NULL,'x')")?.rows,
            vec![vec![Value::Varchar("\0".into()), Value::Varchar("💩".into()), Value::Integer(0), Value::Integer(0), Value::Integer(233), Value::Boolean(true), Value::Boolean(true), Value::Null]]);
        for value in ["-1", "55296", "1114112"] {
            assert!(
                matches!(connection.query(&format!("SELECT chr({value})")), Err(Error::InvalidInput(message)) if message.contains("Invalid UTF8 Codepoint"))
            );
        }
        assert!(matches!(
            connection.query("SELECT chr('x')"),
            Err(Error::Bind(message)) if message.contains("No function matches")
        ));
        assert!(matches!(
            connection.query("SELECT chr()"),
            Err(Error::Bind(message)) if message.contains("No function matches")
        ));
        assert!(connection.query("SELECT contains(['x'],'x')").is_err());
        assert!(matches!(
            connection.query("SELECT contains(NULL,NULL)"),
            Err(Error::Bind(message)) if message.contains("Could not choose a best candidate function")
        ));
        let prepared = connection
            .prepare("SELECT chr($1), ascii(chr($1)), contains(concat('a',chr($1),'b'),chr($1))")?;
        assert_eq!(
            connection
                .execute_prepared(&prepared, &[Value::Integer(0)])?
                .rows,
            vec![vec![
                Value::Varchar("\0".into()),
                Value::Integer(0),
                Value::Boolean(true)
            ]]
        );
        connection.execute("CREATE TABLE codepoint_values(value VARCHAR)")?;
        connection.execute("INSERT INTO codepoint_values VALUES ('x'), ('\0'), ('é'), (NULL)")?;
        assert_eq!(connection.query("SELECT sum(ascii(value)), count(*) FILTER (contains(value,'\0')) FROM codepoint_values")?.rows,
            vec![vec![Value::Integer(353), Value::Integer(1)]]);
    }
    Ok(())
}
