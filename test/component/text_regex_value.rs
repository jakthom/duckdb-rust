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
        assert!(matches!(
            connection.query("SELECT regexp_replace('abc', '(b)', '\\3Y')"),
            Err(duckdb_rust::Error::InvalidInput(message))
                if message == "Invalid replacement string for regexp_replace"
        ));
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
        assert_eq!(
            connection.query("SELECT regexp_extract_all('1-a 22-b 333', '([0-9]+)', 1), regexp_extract_all('1-a 22-b 333', '([0-9]+)-[a-z]+', 1)")?.rows,
            vec![vec![
                strings(&[Some("1"), Some("22"), Some("333")])?,
                strings(&[Some("1"), Some("22")])?
            ]]
        );
        assert_eq!(
            connection
                .query("SELECT regexp_extract_all('aabca', 'a*')")?
                .rows,
            vec![vec![strings(&[
                Some("aa"),
                Some(""),
                Some(""),
                Some("a"),
                Some("")
            ])?]]
        );
        assert_eq!(
            connection
                .query(r"SELECT regexp_extract_all('\001\002\003', '\002?')")?
                .rows,
            vec![vec![strings(&[Some(""); 13])?]]
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
        connection.execute(
            "CREATE TABLE regex_positions(id INTEGER, s VARCHAR, p VARCHAR, g BIGINT, n VARCHAR)",
        )?;
        connection.execute("INSERT INTO regex_positions VALUES (1, '1-a 22-b', '([0-9]+)', 1, '22'), (2, 'x', '(a)?', 1, NULL), (3, NULL, 'x', 0, 'x')")?;
        assert_eq!(
            connection
                .query("SELECT list_position(regexp_extract_all(s, p, g), n) FROM regex_positions ORDER BY id")?
                .rows,
            vec![
                vec![Value::Integer(2)],
                vec![Value::Integer(1)],
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
        assert!(matches!(
            connection.query("SELECT regexp_extract_all('abb', 'ab++')"),
            Err(duckdb_rust::Error::InvalidInput(message))
                if message == "bad repetition operator: ++"
        ));
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn regexp_named_struct_extract_metadata_values_and_bind_rejections() -> Result<()> {
    let struct_type = NestedType::Struct(vec![
        ("name".into(), DataType::Varchar),
        ("number".into(), DataType::Varchar),
    ])
    .data_type();
    let record = |name: Option<&str>, number: Option<&str>| {
        NestedValue::value(
            struct_type.clone(),
            NestedPayload::Struct(vec![
                name.map_or(Value::Null, |value| Value::Varchar(value.into())),
                number.map_or(Value::Null, |value| Value::Varchar(value.into())),
            ]),
        )
    };
    let scalar_empty = NestedValue::value(
        struct_type.clone(),
        NestedPayload::Struct(vec![
            Value::Varchar(String::new()),
            Value::Varchar(String::new()),
        ]),
    )?;
    let list_type = NestedType::List(struct_type.clone()).data_type();
    let list = |values| NestedValue::value(list_type.clone(), NestedPayload::Sequence(values));
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
            connection.query("SELECT regexp_extract('alpha:42', '(\\w+):(\\d+)', ['name', 'number']), regexp_extract('miss', '(\\w+):(\\d+)', ['name', 'number']), regexp_extract(NULL, '(\\w+):(\\d+)', ['name', 'number'])")?.rows,
            vec![vec![record(Some("alpha"), Some("42"))?, scalar_empty.clone(), Value::Null]],
        );
        assert_eq!(
            connection.query("SELECT regexp_extract_all('a:1 b:2', '(\\w+):(\\d+)', ['name', 'number']), regexp_extract_all('miss', '(\\w+):(\\d+)', ['name', 'number']), regexp_extract_all('a:1', '(\\w+):(\\d+)?', ['name', 'number'])")?.rows,
            vec![vec![
                list(vec![record(Some("a"), Some("1"))?, record(Some("b"), Some("2"))?])?,
                list(vec![])?,
                list(vec![record(Some("a"), Some("1"))?])?,
            ]],
        );
        let result = connection.query("SELECT regexp_extract('a:', '([^:]+):([0-9]+)?', ['name','number']) AS one, regexp_extract_all('a:', '([^:]+):([0-9]+)?', ['name','number']) AS many, regexp_extract_all(NULL, '(x)', ['name']) AS missing")?;
        assert_eq!(result.columns[0].data_type, struct_type);
        assert_eq!(result.columns[1].data_type, list_type);
        assert_eq!(
            result.rows,
            vec![vec![
                record(Some("a"), Some(""))?,
                list(vec![record(Some("a"), None)?])?,
                Value::Null
            ]]
        );
        let unicode = connection.query("SELECT regexp_extract('é:3', '([^:]+):([0-9]+)', ['name','number']), regexp_extract_all('a:1 B:2', '([ab]):([0-9]+)', ['name','number'], 'i')")?;
        assert_eq!(
            unicode.rows,
            vec![vec![
                record(Some("é"), Some("3"))?,
                list(vec![
                    record(Some("a"), Some("1"))?,
                    record(Some("B"), Some("2"))?
                ])?
            ]]
        );
        let nul = connection.query("SELECT regexp_extract('a\0b', '(a)(\0b)', ['name','number']), regexp_extract_all('a\0b', '(a)(\0b)', ['name','number'])")?;
        assert_eq!(
            nul.rows,
            vec![vec![
                record(Some("a"), Some("\0b"))?,
                list(vec![record(Some("a"), Some("\0b"))?])?
            ]]
        );
        let prepared = connection.prepare("SELECT regexp_extract($1, '(\\w+):(\\d+)', ['name', 'number']), regexp_extract_all($1, '(\\w+):(\\d+)', ['name', 'number'])")?;
        assert_eq!(
            connection
                .execute_prepared(&prepared, &[Value::Varchar("again:7".into())])?
                .rows,
            vec![vec![
                record(Some("again"), Some("7"))?,
                list(vec![record(Some("again"), Some("7"))?])?
            ]],
        );
        assert_eq!(
            connection
                .execute_prepared(&prepared, &[Value::Varchar("none".into())])?
                .rows,
            vec![vec![scalar_empty.clone(), list(vec![])?]],
        );
        connection
            .execute("CREATE TABLE regexp_named_binding(s VARCHAR, p VARCHAR, names VARCHAR[])")?;
        connection.execute(
            "INSERT INTO regexp_named_binding VALUES ('a:1', '(\\w+):(\\d+)', ['name', 'number'])",
        )?;
        for function in ["regexp_extract", "regexp_extract_all"] {
            for (arguments, diagnostic) in [
                ("'a', '(a)', []", "name list must be non-empty"),
                ("'a', '(a)', ['x', NULL]", "NULL group name"),
                ("'a', '(a)(b)?', ['x','X']", "Duplicate group name"),
                ("'a', '(a)', ['x','y']", "Not enough capturing groups"),
                ("'a', NULL, ['x']", "constant pattern"),
                ("'a', '(a)', NULL::VARCHAR[]", "non-NULL LIST"),
            ] {
                let sql = format!("SELECT {function}({arguments})");
                let error = connection.query(&sql).expect_err(&sql);
                assert!(error.to_string().contains(diagnostic), "{sql}: {error}");
            }
        }
        for sql in [
            "SELECT regexp_extract('a:1', '(\\w+):(\\d+)', [])",
            "SELECT regexp_extract('a:1', '(\\w+):(\\d+)', ['name', 'NAME'])",
            "SELECT regexp_extract('a:1', '(\\w+):(\\d+)', ['name', NULL])",
            "SELECT regexp_extract('a:1', '(\\w+):(\\d+)', ['only', 'too', 'many'])",
            "SELECT regexp_extract(s, p, ['name', 'number']) FROM regexp_named_binding",
            "SELECT regexp_extract('a:1', '(\\w+):(\\d+)', names) FROM regexp_named_binding",
            "SELECT regexp_extract_all('a:1', '(\\w+):(\\d+)', names) FROM regexp_named_binding",
        ] {
            assert!(connection.query(sql).is_err(), "{sql}");
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn regexp_named_empty_fields_keep_development_struct_metadata_and_access() -> Result<()> {
    let mut connection = DatabaseBuilder::new().build()?.connect();
    for (names, fields, rendered, type_text, key, extracted) in [
        ("['']", vec![""], "(a,)", "STRUCT(VARCHAR)", None, "a"),
        (
            "['','x']",
            vec!["", "x"],
            "(a, b)",
            "STRUCT(VARCHAR, VARCHAR)",
            Some("x"),
            "b",
        ),
        (
            "['x','']",
            vec!["x", ""],
            "{'x': a, '': b}",
            "STRUCT(x VARCHAR,  VARCHAR)",
            Some("x"),
            "a",
        ),
    ] {
        let sql = format!("regexp_extract('ab', '(a)(b)', {names})");
        let result = connection.query(&format!("SELECT {sql}"))?;
        let expected_type = NestedType::Struct(
            fields
                .into_iter()
                .map(|name| (name.into(), DataType::Varchar))
                .collect(),
        )
        .data_type();
        assert_eq!(result.columns[0].data_type, expected_type);
        assert_eq!(result.rows[0][0].to_string(), rendered);
        assert_eq!(
            connection
                .query(&format!("SELECT CAST({sql} AS VARCHAR)"))?
                .rows,
            vec![vec![Value::Varchar(rendered.into())]]
        );
        assert_eq!(
            connection.query(&format!("SELECT typeof({sql})"))?.rows,
            vec![vec![Value::Varchar(type_text.into())]]
        );
        if let Some(key) = key {
            assert_eq!(
                connection
                    .query(&format!("SELECT struct_extract({sql}, '{key}')"))?
                    .rows,
                vec![vec![Value::Varchar(extracted.into())]]
            );
        }
        assert!(
            connection
                .query(&format!("SELECT struct_extract({sql}, '')"))
                .is_err()
        );
        assert_eq!(
            connection
                .query(&format!("SELECT struct_extract_at({sql},1)"))?
                .rows,
            vec![vec![Value::Varchar("a".into())]]
        );
        let all = connection.query(&format!(
            "SELECT regexp_extract_all('ab', '(a)(b)', {names})"
        ))?;
        assert_eq!(all.rows[0][0].to_string(), format!("[{rendered}]"));
        assert_eq!(
            connection
                .query(&format!(
                    "SELECT CAST(regexp_extract_all('ab', '(a)(b)', {names}) AS VARCHAR)"
                ))?
                .rows,
            vec![vec![Value::Varchar(format!("[{rendered}]"))]]
        );
    }
    assert!(
        connection
            .query("SELECT regexp_extract('ab', '(a)(b)', ['',''])")
            .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn regexp_named_empty_fields_survive_snapshot_and_native_wal_reopen() -> Result<()> {
    for native in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("empty-fields.db");
        let open = || {
            if native {
                duckdb_rust::Database::open_logged(&path)
            } else {
                duckdb_rust::Database::open_snapshot(&path)
            }
        };
        {
            let database = open()?;
            let mut connection = database.connect();
            connection.execute("CREATE TABLE empty_fields AS SELECT regexp_extract('ab', '(a)(b)', ['','x']) AS first_empty, regexp_extract('ab', '(a)(b)', ['x','']) AS last_empty")?;
        }
        let database = open()?;
        let mut connection = database.connect();
        assert_eq!(connection.query("SELECT struct_extract(first_empty,'x'), struct_extract_at(first_empty,1), struct_extract(last_empty,'x'), struct_extract_at(last_empty,2) FROM empty_fields")?.rows, vec![vec![Value::Varchar("b".into()), Value::Varchar("a".into()), Value::Varchar("a".into()), Value::Varchar("b".into())]], "native={native}");
    }
    Ok(())
}
