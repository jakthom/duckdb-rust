use crate::common::{Error, Result};

/// Shared SQL syntax interchange; only the SQL binder consumes this AST.
/// Other frontends can submit logical plans through the runtime's plan API.
pub use sqlparser::ast;

/// SQL syntax plus database maintenance statements missing from the upstream
/// grammar. Frontends and binders share this interchange, not SQL text rewrites.
#[derive(Clone, Debug)]
pub enum Statement {
    Sql(Box<ast::Statement>),
    Checkpoint,
}

pub trait Parser: Send + Sync {
    fn name(&self) -> &'static str;
    fn parse(&self, sql: &str) -> Result<Vec<Statement>>;
}

#[derive(Default)]
pub struct DuckDbParser;

impl Parser for DuckDbParser {
    fn name(&self) -> &'static str {
        "sqlparser-duckdb"
    }
    fn parse(&self, sql: &str) -> Result<Vec<Statement>> {
        use sqlparser::tokenizer::Token;
        let mut parser = sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect {})
            .with_recursion_limit(128)
            .try_with_sql(sql)
            .map_err(|e| Error::Parse(e.to_string()))?;
        let mut statements = Vec::new();
        loop {
            while parser.consume_token(&Token::SemiColon) {}
            if parser.peek_token().token == Token::EOF {
                break;
            }
            let checkpoint = matches!(parser.peek_token().token, Token::Word(word) if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("checkpoint"));
            let statement = if checkpoint {
                parser.next_token();
                Statement::Checkpoint
            } else {
                Statement::Sql(Box::new(
                    parser
                        .parse_statement()
                        .map_err(|e| Error::Parse(e.to_string()))?,
                ))
            };
            statements.push(statement);
            if parser.peek_token().token != Token::EOF && !parser.consume_token(&Token::SemiColon) {
                return Err(Error::Parse(format!(
                    "expected statement delimiter, found {}",
                    parser.peek_token()
                )));
            }
        }
        Ok(statements)
    }
}
