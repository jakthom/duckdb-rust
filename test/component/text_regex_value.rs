use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

#[test]
fn regex_value_functions_cover_options_groups_nulls_and_nuls() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(connection.query(
            "SELECT regexp_replace('abc123abc', '[a-z]+', 'X'), regexp_replace('abc123abc', '[a-z]+', 'X', 'g'), regexp_replace('ab', '(a)(b)', '\\2\\1'), regexp_extract('abc-123', '([a-z]+)-([0-9]+)', 2), regexp_extract('abc', 'z', 'k'), regexp_extract('abc', 'z'), regexp_extract(NULL, 'x'), regexp_escape('a.b\\c')"
        )?.rows, vec![vec![
            Value::Varchar("X123abc".into()), Value::Varchar("X123X".into()),
            Value::Varchar("ba".into()), Value::Varchar("123".into()),
            Value::Varchar("abc".into()), Value::Varchar(String::new()), Value::Null,
            Value::Varchar("a\\.b\\\\c".into()),
        ]]);
        assert_eq!(
            connection.query("SELECT regexp_extract('foobarbaz', 'b..', NULL), regexp_extract('foobarbaz', 'b..', 1), regexp_escape('https://duckdb.org'), regexp_escape('a b@c-'), regexp_replace('x', 'x', '$')")?.rows,
            vec![vec![
                Value::Varchar(String::new()),
                Value::Varchar(String::new()),
                Value::Varchar(r"https\:\/\/duckdb\.org".into()),
                Value::Varchar("a\\ b\\@c\\-".into()),
                Value::Varchar("$".into()),
            ]]
        );
        connection.execute("CREATE TABLE regex_value_rows(s VARCHAR, p VARCHAR)")?;
        connection
            .execute("INSERT INTO regex_value_rows VALUES ('a\0b', '\0'), ('abc', '[a-z]+')")?;
        assert_eq!(connection.query("SELECT regexp_replace(s, p, 'X', 'g'), regexp_extract(s, p) FROM regex_value_rows ORDER BY s")?.rows,
            vec![vec![Value::Varchar("aXb".into()), Value::Varchar("\0".into())], vec![Value::Varchar("X".into()), Value::Varchar("abc".into())]]);
        assert_eq!(
            connection
                .query("SELECT regexp_extract('x', '(x)', 2)")?
                .rows,
            vec![vec![Value::Varchar(String::new())]]
        );
        assert!(
            connection
                .query("SELECT regexp_replace('x', 'x', 'x', 'k')")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_replace('x', '(x)', '\\2')")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_replace('x', '(x)', '\\x')")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_extract('x', '(x)', -1)")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_extract('x', '(x)', 42)")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_extract('abcdefg', 'A..', 'i', 'c')")
                .unwrap_err()
                .to_string()
                .contains("Could not choose a best candidate function")
        );
        assert!(
            connection
                .query("SELECT regexp_matches('', '\\X')")
                .unwrap_err()
                .to_string()
                .contains("invalid escape sequence")
        );
    }
    Ok(())
}
