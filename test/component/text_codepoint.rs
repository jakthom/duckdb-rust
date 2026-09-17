use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Error, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

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
        assert!(connection.query("SELECT chr('x')").is_err());
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
