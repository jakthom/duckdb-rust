use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn regexp_predicates_cover_constants_options_nuls_and_prepared_rows() -> Result<()> {
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
            connection
                .query("SELECT regexp_matches('asdf', 'sd'), regexp_full_match('asdf', '.sd.'), regexp_matches(NULL, '.*'), regexp_matches('x', NULL), regexp_matches('as^/$df', '^/$', 'l'), regexp_matches('ASDF', '.*sd.*', ' i \t'), regexp_matches('alpha', 'ALPHA$', 'i'), regexp_matches('ASDF', 'sd', 'c'), regexp_matches('hello\nworld', '.*', 's'), regexp_full_match('hello\nworld', '.*', 'n'), regexp_full_match('hello\nworld', '.*', 'p'), regexp_full_match('🦆', '\\x{1F986}')")?
                .rows,
            vec![vec![
                Value::Boolean(true), Value::Boolean(true), Value::Null, Value::Null,
                Value::Boolean(true), Value::Boolean(true), Value::Boolean(true),
                Value::Boolean(false), Value::Boolean(true), Value::Boolean(false),
                Value::Boolean(false), Value::Boolean(true),
            ]]
        );

        connection.execute("CREATE TABLE regex_values(s VARCHAR, p VARCHAR)")?;
        let insert = connection.prepare("INSERT INTO regex_values VALUES ($1, $2)")?;
        for values in [
            [
                Value::Varchar("a\0goose".into()),
                Value::Varchar("\0go".into()),
            ],
            [Value::Varchar("asdf".into()), Value::Varchar("sd".into())],
            [Value::Null, Value::Varchar(".*".into())],
            [Value::Varchar("asdf".into()), Value::Null],
        ] {
            connection.execute_prepared(&insert, &values)?;
        }
        assert_eq!(
            connection
                .query("SELECT regexp_matches(s, p), regexp_full_match(s, p) FROM regex_values ORDER BY s NULLS LAST")?
                .rows,
            vec![
                vec![Value::Boolean(true), Value::Boolean(false)],
                vec![Value::Boolean(true), Value::Boolean(false)],
                vec![Value::Null, Value::Null],
                vec![Value::Null, Value::Null],
            ]
        );
        assert_eq!(
            connection
                .query("SELECT count(*) FROM regex_values WHERE regexp_matches(s, p) AND NOT regexp_full_match(s, p)")?
                .rows,
            vec![vec![Value::Integer(2)]]
        );
        assert_eq!(
            connection
                .query(
                    "SELECT count(*) FILTER (WHERE regexp_matches(s, 'DF$', 'i')) FROM regex_values"
                )?
                .rows,
            vec![vec![Value::Integer(2)]]
        );
        let prepared = connection.prepare("SELECT regexp_matches($1, $2)")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[Value::Varchar("é🦆".into()), Value::Varchar("🦆".into())]
                )?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert!(
            connection
                .query("SELECT regexp_matches('', '\\X')")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_matches('x', 'x', NULL)")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_matches('x', 'x', 'q')")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_matches('x', 'x', 'g')")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_matches(s, p) FROM (VALUES ('x', '(')) t(s, p)")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_matches(s, 'x', s) FROM (VALUES ('x')) t(s)")
                .is_err()
        );
    }
    Ok(())
}
