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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn unicode_normalization_functions_cover_scalar_null_prepared_and_batched_rows() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(evaluator)
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(
            connection.query("SELECT strip_accents('hello'), strip_accents('hännës mühlëïsën'), strip_accents('ôâêóáëòõç'), strip_accents('øßŁ'), strip_accents('a\0é'), strip_accents(''), strip_accents(NULL)")?.rows,
            vec![vec![
                Value::Varchar("hello".into()),
                Value::Varchar("hannes muhleisen".into()),
                Value::Varchar("oaeoaeooc".into()),
                Value::Varchar("øßŁ".into()),
                Value::Varchar("a\0e".into()),
                Value::Varchar("".into()),
                Value::Null,
            ]]
        );
        assert_eq!(
            connection.query("SELECT nfc_normalize('é'), nfc_normalize('A\0̊'), nfc_normalize('ascii'), nfc_normalize(NULL)")?.rows,
            vec![vec![
                Value::Varchar("é".into()),
                Value::Varchar("A\0̊".into()),
                Value::Varchar("ascii".into()),
                Value::Null,
            ]]
        );
        assert_eq!(
            connection
                .query("SELECT strip_accents('각'), nfc_normalize('각'), strip_accents('\u{0378}'), nfc_normalize('\u{0378}')")?
                .rows,
            vec![vec![
                Value::Varchar("각".into()),
                Value::Varchar("각".into()),
                Value::Varchar("\u{0378}".into()),
                Value::Varchar("\u{0378}".into()),
            ]]
        );
        assert!(matches!(
            connection.query("SELECT strip_accents(42)"),
            Err(Error::Bind(message)) if message.contains("No function matches")
        ));
        assert!(matches!(
            connection.query("SELECT nfc_normalize()"),
            Err(Error::Bind(message)) if message.contains("No function matches")
        ));
        let prepared = connection.prepare("SELECT strip_accents($1), nfc_normalize($2)")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        Value::Varchar("Crème brûlée".into()),
                        Value::Varchar("ô".into())
                    ],
                )?
                .rows,
            vec![vec![
                Value::Varchar("Creme brulee".into()),
                Value::Varchar("ô".into())
            ]]
        );
        connection.execute("CREATE TABLE normalized_values(value VARCHAR)")?;
        connection.execute("INSERT INTO normalized_values VALUES ('é'), ('é'), ('\0ö'), (NULL)")?;
        assert_eq!(
            connection.query("SELECT strip_accents(value), nfc_normalize(value) FROM normalized_values ORDER BY value NULLS LAST")?.rows,
            vec![
                vec![Value::Varchar("\0o".into()), Value::Varchar("\0ö".into())],
                vec![Value::Varchar("e".into()), Value::Varchar("é".into())],
                vec![Value::Varchar("e".into()), Value::Varchar("é".into())],
                vec![Value::Null, Value::Null],
            ]
        );
    }
    Ok(())
}
