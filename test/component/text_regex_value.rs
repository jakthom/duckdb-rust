use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Result, Value,
    common::{DataType, NestedPayload, NestedType, NestedValue},
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
        let prepared = connection
            .prepare("SELECT regexp_replace($1, $2, $3, 'g'), regexp_extract($1, $2, 1)")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        Value::Varchar("a1a".into()),
                        Value::Varchar("(a)".into()),
                        Value::Varchar("\\1x".into()),
                    ],
                )?
                .rows,
            vec![vec![
                Value::Varchar("ax1ax".into()),
                Value::Varchar("a".into()),
            ]]
        );
        assert!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        Value::Varchar("a".into()),
                        Value::Varchar("(a)".into()),
                        Value::Varchar("\\".into()),
                    ],
                )
                .is_err()
        );
    }
    Ok(())
}

fn strings(values: &[Option<&str>]) -> Result<Value> {
    NestedValue::value(
        NestedType::List(DataType::Varchar).data_type(),
        NestedPayload::Sequence(
            values
                .iter()
                .map(|value| value.map_or(Value::Null, |value| Value::Varchar(value.into())))
                .collect(),
        ),
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn regexp_extract_all_scalar_groups_cover_matches_and_boundaries() -> Result<()> {
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
            connection.query("SELECT regexp_extract_all('a1a2', '(a)([0-9])', 1), regexp_extract_all('a1a2', '(a)([0-9])', 2), regexp_extract_all('abc', 'z'), regexp_extract_all('', '')")?.rows,
            vec![vec![strings(&[Some("a"), Some("a")])?, strings(&[Some("1"), Some("2")])?, strings(&[])?, strings(&[Some("")])?]]
        );
        connection.execute("CREATE TABLE regex_all(s VARCHAR, p VARCHAR, g BIGINT)")?;
        connection.execute("INSERT INTO regex_all VALUES ('a\0a', 'a', 0), ('éé', '.', 0), ('x', '(a)?', 1), (NULL, 'x', 0)")?;
        assert_eq!(
            connection
                .query("SELECT regexp_extract_all(s, p, g) FROM regex_all ORDER BY s NULLS LAST")?
                .rows,
            vec![
                vec![strings(&[Some("a"), Some("a")])?],
                vec![strings(&[None, None])?],
                vec![strings(&[Some("é"), Some("é")])?],
                vec![Value::Null],
            ]
        );
        assert!(
            connection
                .query("SELECT regexp_extract_all('x', '(x)', 2)")
                .is_err()
        );
        assert_eq!(
            connection
                .query("SELECT regexp_extract_all('x', '(x)', -1)")?
                .rows,
            vec![vec![strings(&[])?]]
        );
        assert!(
            connection
                .query("SELECT regexp_extract_all('x', '(', 0)")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT regexp_extract_all('x', 'x', 0, $1)")
                .is_err()
        );
        let prepared = connection.prepare("SELECT regexp_extract_all($1, $2, $3)")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        Value::Varchar("b1b2".into()),
                        Value::Varchar("(b)([0-9])".into()),
                        Value::Integer(2)
                    ]
                )?
                .rows,
            vec![vec![strings(&[Some("1"), Some("2")])?]]
        );
    }
    Ok(())
}
