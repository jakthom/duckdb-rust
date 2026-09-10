//! DATE(x) is parser cast syntax in the pinned development transformer, not
//! a scalar catalog entry. Preserve its early modifier handling and OVER split.
use sqlparser::{
    ast::{self, Expr, FunctionArg, FunctionArgExpr, helpers::attached_token::AttachedToken},
    parser::{Parser, ParserError},
    tokenizer::Token,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn parse(parser: &mut Parser) -> Option<Result<Expr, ParserError>> {
    let mut offset = 0;
    let mut expect_word = true;
    let mut final_date = false;
    loop {
        // Raw-token lookahead is linear even for long qualified names. Repeated
        // whitespace-skipping nth-token searches would rescan earlier parts.
        match parser.peek_nth_token_no_skip(offset).token {
            Token::Whitespace(_) => (),
            Token::Word(word) if expect_word => {
                final_date = word.value.eq_ignore_ascii_case("date");
                expect_word = false;
            }
            Token::Period if !expect_word => expect_word = true,
            Token::LParen if !expect_word && final_date => {
                return Some(lower(parser));
            }
            _ => return None,
        }
        offset += 1;
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn lower(parser: &mut Parser) -> Result<Expr, ParserError> {
    let name = parser.parse_object_name(false)?;
    let expression = parser.parse_function(name)?;
    let Expr::Function(function) = expression else {
        unreachable!("parse_function returns a function AST")
    };
    if function.over.is_some() {
        return Ok(Expr::Function(function));
    }
    let wrong_arity =
        || ParserError::ParserError("Wrong number of arguments provided to DATE function".into());
    let ast::FunctionArguments::List(mut arguments) = function.args else {
        return Err(wrong_arity());
    };
    if arguments.args.len() != 1 {
        return Err(wrong_arity());
    }
    let argument = match arguments.args.remove(0) {
        FunctionArg::Unnamed(arg)
        | FunctionArg::Named { arg, .. }
        | FunctionArg::ExprNamed { arg, .. } => arg,
    };
    // The reference removes a lone empty star before the DATE arity check,
    // except with DISTINCT or argument ORDER BY. Other modifiers are discarded.
    if matches!(argument, FunctionArgExpr::Wildcard)
        && arguments.duplicate_treatment != Some(ast::DuplicateTreatment::Distinct)
        && !arguments
            .clauses
            .iter()
            .any(|clause| matches!(clause, ast::FunctionArgumentClause::OrderBy(_)))
    {
        return Err(wrong_arity());
    }
    let expression = match argument {
        FunctionArgExpr::Expr(expression) => expression,
        FunctionArgExpr::Wildcard | FunctionArgExpr::WildcardWithOptions(_) => {
            Expr::Wildcard(AttachedToken::empty())
        }
        FunctionArgExpr::QualifiedWildcard(name) => {
            Expr::QualifiedWildcard(name, AttachedToken::empty())
        }
    };
    Ok(Expr::Cast {
        kind: ast::CastKind::Cast,
        expr: Box::new(expression),
        data_type: ast::DataType::Date,
        array: false,
        format: None,
    })
}
