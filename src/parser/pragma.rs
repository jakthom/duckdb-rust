//! PRAGMA literals use the ordinary AST, including signed numeric values.
use sqlparser::{
    ast::{Statement, Value, ValueWithSpan},
    parser::{Parser, ParserError},
    tokenizer::Token,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn parse(parser: &mut Parser) -> Result<Statement, ParserError> {
    let name = parser.parse_object_name(false)?;
    let is_eq = parser.consume_token(&Token::Eq);
    let parenthesized = !is_eq && parser.consume_token(&Token::LParen);
    let value = if is_eq || parenthesized {
        if parser.peek_token().token == Token::RParen {
            return Err(ParserError::ParserError(
                "syntax error at or near \")\"".into(),
            ));
        }
        let negative = parser.consume_token(&Token::Minus);
        let positive = !negative && parser.consume_token(&Token::Plus);
        let mut value = parser.parse_value()?;
        if negative || positive {
            let Value::Number(number, _) = &mut value.value else {
                return Err(ParserError::ParserError(
                    "expected a numeric literal after PRAGMA sign".into(),
                ));
            };
            if negative {
                *number = format!("-{number}");
            }
        }
        if parenthesized {
            parser.expect_token(&Token::RParen)?;
        }
        Some(value)
    } else {
        None::<ValueWithSpan>
    };
    Ok(Statement::Pragma { name, value, is_eq })
}
