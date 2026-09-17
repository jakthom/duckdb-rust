use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Error, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

#[test]
fn grapheme_text_functions_preserve_clusters_bounds_nuls_and_nulls() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(2)
            .build()?
            .connect();
        let graphemes = "e\u{301}👍🏽\u{200d}❤️\u{fe0f}x";
        assert_eq!(connection.query(&format!(
            "SELECT length_grapheme('{graphemes}'),substring_grapheme('{graphemes}',1,1),substring_grapheme('{graphemes}',2,1),substring_grapheme('{graphemes}',-1),substring_grapheme('{graphemes}',0,2),substring_grapheme('{graphemes}',3,-2),substring_grapheme('',1,1),length_grapheme(NULL),substring_grapheme(NULL,4294967296,1)"
        ))?.rows, vec![vec![
            Value::Integer(3), Value::Varchar("e\u{301}".into()), Value::Varchar("👍🏽\u{200d}❤️\u{fe0f}".into()), Value::Varchar("x".into()), Value::Varchar("e\u{301}".into()), Value::Varchar("e\u{301}".into()), Value::Varchar("".into()), Value::Null, Value::Null,
        ]]);
        connection.execute("CREATE TABLE graphemes(v VARCHAR, start BIGINT, count BIGINT)")?;
        let insert = connection.prepare("INSERT INTO graphemes VALUES ($1,$2,$3)")?;
        for row in [
            [
                Value::Varchar("e\u{301}\0x".into()),
                Value::Integer(1),
                Value::Integer(2),
            ],
            [
                Value::Varchar("👍🏽x".into()),
                Value::Integer(1),
                Value::Integer(1),
            ],
            [Value::Null, Value::Integer(1), Value::Integer(1)],
        ] {
            connection.execute_prepared(&insert, &row)?;
        }
        assert_eq!(connection.query("SELECT substring_grapheme(v,start,count),length_grapheme(v),sum(length_grapheme(v)) OVER () FROM graphemes")?.rows, vec![
            vec![Value::Varchar("e\u{301}\0".into()), Value::Integer(3), Value::Integer(5)],
            vec![Value::Varchar("👍🏽".into()), Value::Integer(2), Value::Integer(5)],
            vec![Value::Null, Value::Null, Value::Integer(5)],
        ]);
        let prepared =
            connection.prepare("SELECT substring_grapheme($1,$2,$3),length_grapheme($1)")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        Value::Varchar("e\u{301}x".into()),
                        Value::Integer(1),
                        Value::Integer(1)
                    ]
                )?
                .rows,
            vec![vec![Value::Varchar("e\u{301}".into()), Value::Integer(2)]]
        );
        assert!(matches!(
            connection.query("SELECT substring_grapheme('abc',4294967296,1)"),
            Err(Error::OutOfRange(_))
        ));
        assert!(matches!(
            connection.query("SELECT substring_grapheme('abc')"),
            Err(Error::Bind(_))
        ));
        assert!(matches!(
            connection.query("SELECT length_grapheme('abc',1)"),
            Err(Error::Bind(_))
        ));
    }
    Ok(())
}
