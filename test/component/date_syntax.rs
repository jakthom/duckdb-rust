use super::*;
use duckdb_rust::{
    execution::expression_executor::{BatchedEvaluator, ScalarEvaluator},
    optimizer::IdentityOptimizer,
    parser::{DuckDbParser, Parser, Statement, ast},
};

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn date_call_is_cast_syntax_with_reference_modifier_and_window_boundaries() -> Result<()> {
    let calls = [
        "DATE('2000-01-01')",
        "date('2000-01-01 24:00:00')",
        "main.date('2000-01-01')",
        "nonexistent.schema.\"DATE\"('2000-01-01')",
        "DATE(DISTINCT '2000-01-01')",
        "DATE(ALL '2000-01-01')",
        "DATE('2000-01-01' ORDER BY nonexistent)",
        "DATE('2000-01-01') FILTER (WHERE nonexistent)",
        "DATE('2000-01-01') WITHIN GROUP (ORDER BY nonexistent)",
        "DATE('2000-01-01' IGNORE NULLS)",
        "DATE('2000-01-01' RESPECT NULLS)",
        "DATE(x := '2000-01-01')",
        "DATE(x => '2000-01-01')",
        "DATE(DATE('2000-01-01'))",
        "DATE((SELECT '2000-01-01'))",
    ];
    for batched in [false, true] {
        let mut c = DatabaseBuilder::new()
            .optimizer(Arc::new(IdentityOptimizer))
            .batch_size(2)
            .expressions(if batched {
                Arc::new(BatchedEvaluator)
            } else {
                Arc::new(ScalarEvaluator)
            })
            .build()?
            .connect();
        for expression in calls {
            let result = c.query(&format!("SELECT {expression}"))?;
            assert_eq!(result.rows, vec![vec![date("2000-01-01")]], "{expression}");
            assert_eq!(result.columns[0].data_type, DataType::Date);
            assert!(result.columns[0].name.starts_with("CAST("));
        }
        assert_eq!(
            c.query("SELECT DATE(NULL),DATE('infinity'),DATE('epoch'),DATE(TIMESTAMP_NS '1969-12-31 23:59:59.999999999')")?.rows,
            vec![vec![Value::Null,date("infinity"),date("epoch"),date("1970-01-01")]]
        );
        c.execute("CREATE TABLE t(s VARCHAR); INSERT INTO t VALUES ('2000-01-01 24:00:00'),('2000-01-02'),(NULL)")?;
        assert_eq!(
            c.query("SELECT DATE(s),min(DATE(s)) OVER(ORDER BY s) FROM t ORDER BY s")?
                .rows,
            vec![
                vec![date("2000-01-01"), date("2000-01-01")],
                vec![date("2000-01-02"), date("2000-01-01")],
                vec![Value::Null, date("2000-01-01")]
            ]
        );
        let statement = c.prepare("SELECT DATE($1),DATE($2::VARIANT)")?;
        assert_eq!(
            c.execute_prepared(
                &statement,
                &[
                    Value::Varchar("2000-01-01 24:00:00".into()),
                    Value::Varchar("01-1-1".into())
                ]
            )?
            .rows,
            vec![vec![date("2000-01-01"), date("0001-01-01")]]
        );
        assert!(matches!(
            c.execute_prepared(
                &statement,
                &[
                    Value::Varchar("2000-01-01".into()),
                    Value::Varchar("1-1-1".into())
                ]
            ),
            Err(Error::Conversion(_))
        ));
        for expression in ["DATE()", "DATE(1,2)", "DATE(*)", "DATE(ALL *)"] {
            assert!(
                matches!(c.query(&format!("SELECT {expression}")),Err(Error::Parse(message)) if message.contains("Wrong number of arguments provided to DATE function")),
                "{expression}"
            );
        }
        for expression in ["DATE(DISTINCT *)", "DATE(* ORDER BY nonexistent)"] {
            assert!(
                matches!(c.query(&format!("SELECT {expression}")),Err(Error::Bind(message)) if message.contains("STAR expression is only allowed as the root")),
                "{expression}"
            );
        }
        for expression in [
            "DATE('2000-01-01') OVER ()",
            "nonexistent.DATE('2000-01-01') OVER ()",
        ] {
            assert!(
                matches!(
                    c.query(&format!("SELECT {expression}")),
                    Err(Error::Catalog(_))
                ),
                "{expression}"
            );
        }
    }
    // OVER remains a function AST, while ordinary calls use the standard CAST
    // AST before grouping/window/dependency collection and adapter selection.
    for (sql, window) in [
        ("SELECT missing.date('epoch')", false),
        ("SELECT date('epoch') OVER ()", true),
    ] {
        let statements = DuckDbParser.parse(sql)?;
        let Statement::Sql(statement) = &statements[0] else {
            panic!("SQL statement")
        };
        let ast::Statement::Query(query) = &**statement else {
            panic!("query")
        };
        let ast::SetExpr::Select(select) = &*query.body else {
            panic!("select")
        };
        let ast::SelectItem::UnnamedExpr(expression) = &select.projection[0] else {
            panic!("expression")
        };
        assert_eq!(matches!(expression, ast::Expr::Function(_)), window);
        assert_eq!(matches!(expression, ast::Expr::Cast { .. }), !window);
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn invalid_infix_colon_does_not_change_dictionary_named_argument_or_slice_grammar() -> Result<()> {
    for sql in [
        "SELECT '-291000-01-01'::DATE:VARCHAR",
        "SELECT '291000-01-01 (BC)'::DATE:VARCHAR",
        "SELECT x:y FROM t",
        "SELECT {'d':1}:d",
    ] {
        assert!(
            matches!(DuckDbParser.parse(sql),Err(Error::Parse(message)) if message.contains("syntax error at or near \":\"")),
            "{sql}"
        );
    }
    // Slices are a distinct grammar production; parsing success does not claim
    // execution support for the still-unimplemented nested slice operator.
    for sql in [
        "SELECT [1,2,3][1:2]",
        "SELECT [1,2,3][:2]",
        "SELECT [1,2,3][1:3:2]",
        "SELECT {'d':DATE('2000-01-01')}",
        "SELECT MAP {'x':1}",
        "SELECT struct_pack(d := DATE('2000-01-01'))",
    ] {
        DuckDbParser.parse(sql)?;
    }
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT {'d':DATE('2000-01-01')}::VARCHAR,struct_pack(d := DATE('2000-01-01'))::VARCHAR,DATE('2000-01-01')::VARCHAR")?.rows,vec![vec![Value::Varchar("{'d': 2000-01-01}".into()),Value::Varchar("{'d': 2000-01-01}".into()),Value::Varchar("2000-01-01".into())]]);
    Ok(())
}
