use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn varchar_search_aliases_unicode_nuls_nulls_and_prepared_execution() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(
            connection.query("SELECT instr('two ñ three ₡ four 🦆 end','ñ'),strpos('two ñ three ₡ four 🦆 end','₡ four 🦆 e'),position('two ñ three ₡ four 🦆 end','🦆 end'),POSITION('🦆 end' IN 'two ñ three ₡ four 🦆 end'),instr('abc',''),instr('',''),instr('','x'),instr('abc','z'),instr('a\\0é🦆','\\0'),instr(NULL,'x'),instr('x',NULL)")?.rows,
            vec![vec![
                Value::Integer(5), Value::Integer(13), Value::Integer(20), Value::Integer(20),
                Value::Integer(1), Value::Integer(1), Value::Integer(0),
                Value::Integer(0), Value::Integer(2), Value::Null, Value::Null,
            ]]
        );

        connection.execute("CREATE TABLE text_search(haystack VARCHAR, needle VARCHAR)")?;
        let insert = connection.prepare("INSERT INTO text_search VALUES ($1,$2)")?;
        for values in [
            [Value::Varchar("hello".into()), Value::Varchar("l".into())],
            [Value::Varchar("é🦆x".into()), Value::Varchar("🦆".into())],
            [Value::Varchar("a\0é".into()), Value::Varchar("\0é".into())],
            [Value::Varchar("abc".into()), Value::Varchar("z".into())],
            [Value::Null, Value::Varchar("x".into())],
        ] {
            connection.execute_prepared(&insert, &values)?;
        }
        let expected = vec![
            vec![Value::Integer(3), Value::Integer(3), Value::Integer(3)],
            vec![Value::Integer(2), Value::Integer(2), Value::Integer(2)],
            vec![Value::Integer(2), Value::Integer(2), Value::Integer(2)],
            vec![Value::Integer(0), Value::Integer(0), Value::Integer(0)],
            vec![Value::Null, Value::Null, Value::Null],
        ];
        assert_eq!(
            connection.query("SELECT instr(haystack,needle),strpos(haystack,needle),position(haystack,needle) FROM text_search")?.rows,
            expected
        );
        assert_eq!(
            connection
                .query("SELECT sum(instr(haystack,needle)) FROM text_search")?
                .rows,
            vec![vec![Value::Integer(7)]]
        );
        let prepared = connection
            .prepare("SELECT instr($1,$2),strpos($1,$2),position($1,$2),POSITION($2 IN $1)")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[Value::Varchar("é🦆x".into()), Value::Varchar("🦆".into())]
                )?
                .rows,
            vec![vec![
                Value::Integer(2),
                Value::Integer(2),
                Value::Integer(2),
                Value::Integer(2)
            ]]
        );
        assert!(connection.query("SELECT instr('x')").is_err());
        assert!(connection.query("SELECT instr('x', 1)").is_err());
        assert!(connection.query("SELECT POSITION(1 IN 'x')").is_err());
    }
    Ok(())
}
