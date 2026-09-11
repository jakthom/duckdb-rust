//! Assign anonymous parameter positions in lexical order, before the binder
//! visits FROM clauses, aliases, windows or repeated expression trees.
use crate::common::{Error, Result};
use sqlparser::{
    dialect::Dialect,
    tokenizer::{Token, TokenWithSpan, Tokenizer},
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
    Ok(tokens)
}
