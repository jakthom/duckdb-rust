//! Reserved unqualified NULLIF has exactly two expression arguments in the
//! pinned PEG grammar. The resulting ordinary call still selects the catalog.
use sqlparser::{
    ast::{self, Expr},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn parse(parser: &mut Parser) -> Option<Result<Expr, ParserError>> {
    if !matches!(parser.peek_token().token,Token::Word(word) if word.quote_style.is_none() && word.keyword == Keyword::NULLIF)
        || parser.peek_nth_token(1).token != Token::LParen
    {
        return None;
    }
    // Compound-call parsing re-enters the dialect prefix hook after a dot.
    // The suffix is not an unqualified reserved call. Inspect token context
    // without consuming/rewinding parser state, including intervening comments.
    let mut previous = parser.index();
    while let Some(index) = previous.checked_sub(1) {
        match &parser.token_at(index).token {
            Token::Whitespace(_) => previous = index,
            Token::Period => return None,
            _ => break,
        }
    }
    Some((|| {
        let name = ast::ObjectName::from(vec![parser.parse_identifier()?]);
        parser.expect_token(&Token::LParen)?;
        let left = argument(parser)?;
        parser.expect_token(&Token::Comma)?;
        let right = argument(parser)?;
        parser.expect_token(&Token::RParen)?;
        Ok(Expr::Function(ast::Function {
            name,
            uses_odbc_syntax: false,
            parameters: ast::FunctionArguments::None,
            args: ast::FunctionArguments::List(ast::FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![left, right]
                    .into_iter()
                    .map(|expression| {
                        ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression))
                    })
                    .collect(),
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
        }))
    })())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn argument(parser: &mut Parser) -> Result<Expr, ParserError> {
    if matches!(parser.peek_token().token, Token::Word(_))
        && matches!(
            parser.peek_nth_token(1).token,
            Token::Assignment | Token::RArrow
        )
    {
        return Err(ParserError::ParserError(
            "named arguments are not allowed in reserved NULLIF syntax".into(),
        ));
    }
    parser.parse_expr()
}

#[cfg(test)]
mod tests {
    use crate::{
        Error,
        parser::{DuckDbParser, Parser},
    };

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn reserved_nullif_arity_and_modifiers_do_not_change_ordinary_function_names() {
        for sql in [
            "SELECT nullif()",
            "SELECT nullif(1)",
            "SELECT nullif(1,2,3)",
            "SELECT nullif(1,2,)",
            "SELECT nullif(DISTINCT 1,2)",
            "SELECT nullif(1,2 ORDER BY 1)",
            "SELECT nullif(a:=1,b:=2)",
            "SELECT nullif(1,2) FILTER(WHERE true)",
            "SELECT nullif(1,2) OVER()",
        ] {
            assert!(
                matches!(DuckDbParser.parse(sql), Err(Error::Parse(_))),
                "{sql}"
            );
        }
        for sql in [
            "SELECT nullif(1,2)::BIGINT",
            "SELECT NuLlIf /* gap */ ((SELECT 1),2)",
            "SELECT nullif FROM(VALUES(1))t(nullif)",
            "SELECT \"nullif\"()",
            "SELECT \"nullif\"(1,2,3)",
        ] {
            let result = DuckDbParser.parse(sql);
            assert!(result.is_ok(), "{sql}: {result:?}");
        }
        // Reserved-looking suffixes must retain ordinary qualified grammar.
        for sql in [
            "SELECT main.nullif()",
            "SELECT main.nullif(1,2,3)",
            "SELECT main /* gap */ . /* gap */ nullif()",
            "SELECT t.nullif FROM(VALUES(1))t(nullif)",
        ] {
            let baseline =
                sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect, sql);
            assert_eq!(DuckDbParser.parse(sql).is_ok(), baseline.is_ok(), "{sql}");
        }
        // These qualified keyword calls use prior rewrite grammar repairs,
        // so stock sqlparser is not their acceptance oracle.
        for sql in [
            "SELECT main.ceil(1.25)",
            "SELECT main.floor(1.25)",
            "SELECT main.date(1)",
        ] {
            assert!(DuckDbParser.parse(sql).is_ok(), "{sql}");
        }
    }
}
