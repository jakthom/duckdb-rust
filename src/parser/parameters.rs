//! Assign anonymous parameter positions in lexical order, before the binder
//! visits FROM clauses, aliases, windows or repeated expression trees.
use crate::common::{Error, Result};
use sqlparser::{
    dialect::Dialect,
    keywords::Keyword,
    tokenizer::{Token, TokenWithSpan, Tokenizer, Word},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn tokens(sql: &str, dialect: &dyn Dialect) -> Result<Vec<TokenWithSpan>> {
    let mut tokens = Tokenizer::new(dialect, sql)
        .tokenize_with_location()
        .map_err(|error| Error::Parse(error.to_string()))?;
    let mut position = 0_usize;
    for token in &mut tokens {
        match &mut token.token {
            Token::SemiColon => position = 0,
            Token::Placeholder(name) if name == "?" => {
                position = position
                    .checked_add(1)
                    .ok_or_else(|| Error::Parse("parameter number overflow".into()))?;
                *name = format!("${position}");
            }
            Token::Placeholder(name) => {
                if let Ok(number) = name.trim_start_matches(['$', '?']).parse::<usize>() {
                    position = position.max(number);
                }
            }
            _ => {}
        }
    }
    Ok(filter_shorthand(tokens))
}

/// sqlparser accepts the standard `FILTER (WHERE predicate)` spelling while
/// DuckDB also accepts `FILTER (predicate)`. Insert the optional keyword in the
/// token stream so both spellings produce the same AST. Requiring a preceding
/// closing parenthesis keeps an ordinary function named `filter` untouched.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn filter_shorthand(tokens: Vec<TokenWithSpan>) -> Vec<TokenWithSpan> {
    let mut rewritten = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        rewritten.push(token.clone());
        if token.token != Token::LParen {
            continue;
        }
        let previous = tokens[..index]
            .iter()
            .rev()
            .filter(|token| !is_trivia(&token.token))
            .take(2)
            .collect::<Vec<_>>();
        if !matches!(previous.as_slice(), [filter, close]
            if matches!(&filter.token, Token::Word(word)
                if word.quote_style.is_none() && word.keyword == Keyword::FILTER)
                && close.token == Token::RParen)
        {
            continue;
        }
        let next = tokens[index + 1..]
            .iter()
            .find(|token| !is_trivia(&token.token));
        if matches!(next, Some(TokenWithSpan { token: Token::Word(word), .. })
            if word.quote_style.is_none() && word.keyword == Keyword::WHERE)
        {
            continue;
        }
        rewritten.push(TokenWithSpan::wrap(Token::Word(Word {
            value: "WHERE".into(),
            quote_style: None,
            keyword: Keyword::WHERE,
        })));
    }
    rewritten
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn is_trivia(token: &Token) -> bool {
    matches!(token, Token::Whitespace(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::dialect::RewriteDialect;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn adds_where_only_to_aggregate_filter_shorthand() {
        let tokens = tokens(
            "SELECT sum(v) FILTER(v > 0), sum(v) FILTER (WHERE v > 0), filter(v), \
                    sum(v) /* before */ FILTER /* inside */ ( /* predicate */ v > 0), \
                    \"filter\"(v)",
            &RewriteDialect,
        )
        .unwrap();
        let where_count = tokens
            .iter()
            .filter(
                |token| matches!(&token.token, Token::Word(word) if word.keyword == Keyword::WHERE),
            )
            .count();
        assert_eq!(where_count, 3);
    }
}
