//! Extend the selected SQL grammar through its dialect interface. Nested
//! grouping constructs produce AST nodes; no SQL text is rewritten.
use sqlparser::{
    ast::Expr,
    dialect::{Dialect, DuckDbDialect},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

#[derive(Debug)]
pub(super) struct RewriteDialect;

macro_rules! delegate_flags {
    ($($name:ident),* $(,)?) => { $(
        #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
        fn $name(&self) -> bool { DuckDbDialect.$name() }
    )* };
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Dialect for RewriteDialect {
    fn dialect(&self) -> std::any::TypeId {
        DuckDbDialect.dialect()
    }
    fn is_identifier_start(&self, ch: char) -> bool {
        DuckDbDialect.is_identifier_start(ch)
    }
    fn is_identifier_part(&self, ch: char) -> bool {
        DuckDbDialect.is_identifier_part(ch)
    }
    fn supports_window_function_null_treatment_arg(&self) -> bool {
        true
    }
    delegate_flags!(
        supports_trailing_commas,
        supports_filter_during_aggregation,
        supports_group_by_expr,
        supports_bitwise_shift_operators,
        supports_named_fn_args_with_eq_operator,
        supports_named_fn_args_with_assignment_operator,
        supports_dictionary_syntax,
        support_map_literal_syntax,
        supports_lambda_functions,
        allow_extract_single_quotes,
        supports_explain_with_utility_options,
        supports_load_extension,
        supports_array_typedef_with_brackets,
        supports_from_first_select,
        supports_order_by_all,
        supports_select_wildcard_exclude,
        supports_notnull_operator,
        supports_install,
        supports_detach,
        supports_select_wildcard_replace,
        supports_comma_separated_trim,
    );
    fn parse_infix(
        &self,
        parser: &mut Parser,
        expr: &Expr,
        precedence: u8,
    ) -> Option<Result<Expr, ParserError>> {
        if parser.peek_token().token == Token::Colon {
            return Some(Err(ParserError::ParserError(
                "syntax error at or near \":\"".into(),
            )));
        }
        DuckDbDialect.parse_infix(parser, expr, precedence)
    }
    fn parse_prefix(&self, parser: &mut Parser) -> Option<Result<Expr, ParserError>> {
        if let Some(expression) = super::date_call::parse(parser) {
            return Some(expression);
        }
        if let Token::Word(word) = parser.peek_token().token
            && word.quote_style.is_none()
            && matches!(word.keyword, Keyword::CEIL | Keyword::FLOOR)
        {
            // DuckDB uses ordinary catalog functions, not sqlparser's special
            // numeric-scale / datetime-TO grammar. Preserve the normal AST and
            // selected binding path, including invalid-arity diagnostics.
            return Some((|| {
                let mut parts = vec![parser.parse_identifier()?];
                while parser.consume_token(&Token::Period) {
                    parts.push(parser.parse_identifier()?);
                }
                if parser.peek_token().token == Token::LParen {
                    parser.parse_function(sqlparser::ast::ObjectName::from(parts))
                } else if parts.len() == 1 {
                    Ok(Expr::Identifier(parts.remove(0)))
                } else {
                    Ok(Expr::CompoundIdentifier(parts))
                }
            })());
        }
        if parser.parse_keyword(Keyword::INTERVAL) {
            return Some(interval_literal(parser));
        }
        if let Token::Word(word) = parser.peek_token().token
            && word.quote_style.is_none()
            && matches!(
                word.value.to_ascii_lowercase().as_str(),
                "time_ns"
                    | "timestamp_s"
                    | "timestamp_ms"
                    | "timestamp_us"
                    | "timestamp_ns"
                    | "timestamptz_ns"
            )
            && matches!(parser.peek_nth_token(1).token, Token::SingleQuotedString(_))
        {
            return Some((|| {
                let data_type = parser.parse_data_type()?;
                let value =
                    sqlparser::ast::Value::SingleQuotedString(parser.parse_literal_string()?)
                        .into();
                Ok(Expr::TypedString(sqlparser::ast::TypedString {
                    data_type,
                    value,
                    uses_odbc_syntax: false,
                }))
            })());
        }
        if !parser.parse_keywords(&[Keyword::GROUPING, Keyword::SETS]) {
            return DuckDbDialect.parse_prefix(parser);
        }
        Some(grouping_items(parser).map(Expr::GroupingSets))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn interval_literal(parser: &mut Parser) -> Result<Expr, ParserError> {
    let mut expression = parser.parse_interval()?;
    if let Expr::Interval(interval) = &mut expression
        && interval.leading_field.is_none()
        && let Token::Word(word) = parser.peek_token().token
        && word.quote_style.is_none()
        && matches!(
            word.value.to_ascii_lowercase().as_str(),
            "quarters" | "decades" | "centuries" | "millennia"
        )
    {
        interval.leading_field = Some(sqlparser::ast::DateTimeField::Custom(
            parser.parse_identifier()?,
        ));
        if parser.parse_keyword(Keyword::TO) {
            return Err(ParserError::ParserError(
                "INTERVAL TO qualifiers are not supported".into(),
            ));
        }
    }
    Ok(expression)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn grouping_items(parser: &mut Parser) -> Result<Vec<Vec<Expr>>, ParserError> {
    parser.expect_token(&Token::LParen)?;
    let items = parser.parse_comma_separated(|parser| {
        if parser.consume_token(&Token::LParen) {
            if parser.consume_token(&Token::RParen) {
                return Ok(Vec::new());
            }
            let items = parser.parse_comma_separated(Parser::parse_expr)?;
            parser.expect_token(&Token::RParen)?;
            Ok(items)
        } else {
            Ok(vec![parser.parse_expr()?])
        }
    })?;
    parser.expect_token(&Token::RParen)?;
    Ok(items)
}

#[cfg(test)]
mod tests {
    use crate::parser::{DuckDbParser, Parser, Statement};
    use sqlparser::ast::{self, Expr, FunctionArguments, SelectItem, SetExpr};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn numeric_direction_syntax_retains_ordinary_function_arguments() {
        for (call, arity) in [
            ("ceil()", 0),
            ("CeIl(1,-1)", 2),
            ("floor(1,'s')", 2),
            ("floor(1,$1)", 2),
            ("ceiling(1)", 1),
        ] {
            let statements = DuckDbParser.parse(&format!("SELECT {call}")).unwrap();
            let Statement::Sql(statement) = &statements[0] else {
                panic!("SQL statement");
            };
            let ast::Statement::Query(query) = statement.as_ref() else {
                panic!("query");
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                panic!("select");
            };
            let SelectItem::UnnamedExpr(Expr::Function(function)) = &select.projection[0] else {
                panic!("ordinary function: {call}");
            };
            let FunctionArguments::List(arguments) = &function.args else {
                panic!("arguments");
            };
            assert_eq!(arguments.args.len(), arity);
        }
        assert!(
            DuckDbParser
                .parse("SELECT floor(TIMESTAMP '2024-01-01' TO DAY)")
                .is_err()
        );
    }
}
