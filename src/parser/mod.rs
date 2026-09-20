use crate::common::{Error, Result};
mod date_call;
mod dialect;
mod nullif_call;
mod parameters;
mod pragma;

/// Shared SQL syntax interchange; only the SQL binder consumes this AST.
/// Other frontends can submit logical plans through the runtime's plan API.
pub use sqlparser::ast;

/// SQL syntax plus database maintenance statements missing from the upstream
/// grammar. Frontends and binders share this interchange, not SQL text rewrites.
#[derive(Clone, Debug)]
pub enum Statement {
    Sql(Box<ast::Statement>),
    Checkpoint,
    ResetSetting {
        name: ast::ObjectName,
        scope: Option<ast::ContextModifier>,
    },
}

#[cfg(feature = "dev")]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Statement {
    /// Parser-rendered SQL gives each executable statement a stable content key,
    /// including statements in a batch. The request trace retains original text.
    pub(crate) fn dev_sql(&self) -> String {
        match self {
            Self::Sql(statement) => statement.to_string(),
            Self::Checkpoint => "CHECKPOINT".into(),
            Self::ResetSetting { name, scope } => {
                let scope = scope
                    .as_ref()
                    .map(|scope| format!("{scope} "))
                    .unwrap_or_default();
                format!("RESET {scope}{name}")
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait Parser: Send + Sync {
    fn name(&self) -> &'static str;
    fn parse(&self, sql: &str) -> Result<Vec<Statement>>;
}

#[derive(Default)]
pub struct DuckDbParser;

/// Parse one SQL type, without accepting a trailing statement or expression.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn parse_type_name(text: &str) -> Result<ast::DataType> {
    let mut parser = sqlparser::parser::Parser::new(&dialect::RewriteDialect)
        .with_recursion_limit(128)
        .try_with_sql(text)
        .map_err(|error| Error::Parse(error.to_string()))?;
    let data_type = parser
        .parse_data_type()
        .map_err(|error| Error::Parse(error.to_string()))?;
    if parser.peek_token().token != sqlparser::tokenizer::Token::EOF {
        return Err(Error::Parse("expected exactly one type name".into()));
    }
    Ok(data_type)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Parser for DuckDbParser {
    fn name(&self) -> &'static str {
        "sqlparser-duckdb"
    }
    fn parse(&self, sql: &str) -> Result<Vec<Statement>> {
        use sqlparser::tokenizer::Token;
        let mut parser = sqlparser::parser::Parser::new(&dialect::RewriteDialect)
            .with_recursion_limit(128)
            .with_tokens_with_locations(parameters::tokens(sql, &dialect::RewriteDialect)?);
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
            } else if parser.parse_keyword(sqlparser::keywords::Keyword::RESET) {
                use sqlparser::keywords::Keyword;
                let scope = if parser.parse_keyword(Keyword::GLOBAL) {
                    Some(ast::ContextModifier::Global)
                } else if parser.parse_keyword(Keyword::SESSION) {
                    Some(ast::ContextModifier::Session)
                } else if parser.parse_keyword(Keyword::LOCAL) {
                    Some(ast::ContextModifier::Local)
                } else {
                    None
                };
                if parser.parse_keyword(Keyword::ALL) {
                    return Err(Error::Unsupported(
                        "Can only SET or RESET a variable".into(),
                    ));
                }
                Statement::ResetSetting {
                    name: parser
                        .parse_object_name(false)
                        .map_err(|e| Error::Parse(e.to_string()))?,
                    scope,
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn named_enum_ddl_retains_conflict_policy_and_empty_dictionary() {
        let statements = DuckDbParser
            .parse(
                "CREATE TYPE mood AS ENUM ('sad', 'ok'); \
                 CREATE TYPE IF NOT EXISTS empty AS ENUM (); \
                 CREATE OR REPLACE TYPE main.mood AS ENUM ('new')",
            )
            .unwrap();
        let expected = [
            (false, false, 2, "CREATE TYPE mood AS ENUM ('sad', 'ok')"),
            (false, true, 0, "CREATE TYPE IF NOT EXISTS empty AS ENUM ()"),
            (
                true,
                false,
                1,
                "CREATE OR REPLACE TYPE main.mood AS ENUM ('new')",
            ),
        ];
        for (statement, (replace, ignore, labels, display)) in statements.iter().zip(expected) {
            let Statement::Sql(statement) = statement else {
                panic!("expected SQL statement")
            };
            let ast::Statement::CreateType {
                or_replace,
                if_not_exists,
                representation: Some(ast::UserDefinedTypeRepresentation::Enum { labels: got }),
                ..
            } = statement.as_ref()
            else {
                panic!("expected CREATE TYPE ENUM")
            };
            assert_eq!(
                (*or_replace, *if_not_exists, got.len()),
                (replace, ignore, labels)
            );
            assert_eq!(statement.to_string(), display);
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn named_enum_parser_rejects_conflicting_or_temporary_forms() {
        assert!(
            DuckDbParser
                .parse("CREATE OR REPLACE TYPE IF NOT EXISTS mood AS ENUM ('x')")
                .is_err()
        );
        assert!(
            DuckDbParser
                .parse("CREATE TEMP TYPE mood AS ENUM ('x')")
                .is_err()
        );
        for sql in [
            "CREATE OR ALTER TYPE mood AS ENUM ('x')",
            "CREATE PERSISTENT TYPE mood AS ENUM ('x')",
            "DROP TEMPORARY TYPE mood",
            "DROP PERSISTENT TYPE mood",
            "CREATE TYPE mood AS ENUM (not_a_literal)",
            "CREATE TYPE mood AS ENUM (\"not_a_literal\")",
            "CREATE TYPE mood AS ENUM (SELECT 'x')",
            "CREATE TYPE alias AS INTEGER",
        ] {
            assert!(DuckDbParser.parse(sql).is_err(), "{sql}");
        }
    }
}
