use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Error, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn substring_and_substr_match_character_indexed_reference_boundaries() -> Result<()> {
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
            connection.query("SELECT length('é🦆x'),char_length('é🦆x'),character_length('é🦆x'),len('é🦆x'),substring('abcdef',2,3),substr('abcdef',2,3),substring('abcdef',2),substring('abcdef',-2),substring('abcdef',0,3),substring('abcdef',3,-2),substring('é🦆x',2,1),substring('',1,1),substring(NULL,2,3)")?.rows,
            vec![vec![
                Value::Integer(3), Value::Integer(3), Value::Integer(3), Value::Integer(3),
                Value::Varchar("bcd".into()), Value::Varchar("bcd".into()),
                Value::Varchar("bcdef".into()), Value::Varchar("ef".into()),
                Value::Varchar("ab".into()), Value::Varchar("ab".into()),
                Value::Varchar("🦆".into()), Value::Varchar("".into()), Value::Null,
            ]]
        );

        connection.execute("CREATE TABLE text_slice(v VARCHAR, start BIGINT, count BIGINT)")?;
        let insert = connection.prepare("INSERT INTO text_slice VALUES ($1,$2,$3)")?;
        for values in [
            [
                Value::Varchar("A\0é🦆".into()),
                Value::Integer(2),
                Value::Integer(3),
            ],
            [
                Value::Varchar("abcdef".into()),
                Value::Integer(-9),
                Value::Integer(2),
            ],
            [
                Value::Varchar("abcdef".into()),
                Value::Integer(2),
                Value::Integer(-1),
            ],
            [Value::Null, Value::Integer(2), Value::Integer(3)],
        ] {
            connection.execute_prepared(&insert, &values)?;
        }
        let expected = vec![
            vec![
                Value::Varchar("\0é🦆".into()),
                Value::Varchar("A\0é🦆".into()),
            ],
            vec![Value::Varchar("".into()), Value::Varchar("abcdef".into())],
            vec![Value::Varchar("a".into()), Value::Varchar("bcdef".into())],
            vec![Value::Null, Value::Null],
        ];
        assert_eq!(
            connection
                .query("SELECT substring(v,start,count),substring(v,start) FROM text_slice")?
                .rows,
            expected
        );
        let prepared = connection.prepare("SELECT substr($1,$2,$3)")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        Value::Varchar("é🦆x".into()),
                        Value::Integer(2),
                        Value::Integer(1)
                    ]
                )?
                .rows,
            vec![vec![Value::Varchar("🦆".into())]]
        );
        assert!(matches!(
            connection.query("SELECT substring('abc',4294967296,1)"),
            Err(Error::OutOfRange(message)) if message == "Substring offset outside of supported range (> 4294967295)"
        ));
    }
    Ok(())
}
