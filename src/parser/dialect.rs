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
    fn parse_prefix(&self, parser: &mut Parser) -> Option<Result<Expr, ParserError>> {
        if !parser.parse_keywords(&[Keyword::GROUPING, Keyword::SETS]) {
            return DuckDbDialect.parse_prefix(parser);
        }
        Some(grouping_items(parser).map(Expr::GroupingSets))
    }
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
