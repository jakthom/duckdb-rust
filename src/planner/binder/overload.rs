//! SQL-owned pseudo-types and selected cast costs; no argument evaluation.
use super::*;
use crate::{common::type_registry::IntegerLiteral, function::ScalarSignature};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn select(
    name: &str,
    candidates: &[ScalarSignature],
    arguments: &[BoundExpr],
    context: &BindContext<'_>,
) -> Result<usize> {
    ScalarSignature::validate_candidates(name, candidates, context.query)?;
    for argument in arguments {
        context.query.check()?;
        context.query.types().bind(&argument.data_type)?;
    }
    let mut best = None;
    let mut lowest = u64::MAX;
    let mut equal = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        context.query.check()?;
        if candidate.arguments.len() != arguments.len() {
            continue;
        }
        let mut cost = 0_u64;
        let mut available = true;
        for (argument, target) in arguments.iter().zip(&candidate.arguments) {
            context.query.check()?;
            let Some(part) = argument_cost(argument, target, context)? else {
                available = false;
                break;
            };
            cost = cost
                .checked_add(u64::from(part))
                .ok_or_else(|| Error::Internal("scalar overload cost overflow".into()))?;
        }
        if !available {
            continue;
        }
        if cost == lowest {
            equal.push(index);
        } else if cost < lowest {
            lowest = cost;
            best = Some(index);
            equal.clear();
        }
    }
    let Some(best) = best else {
        return Err(diagnostic(name, arguments, candidates.iter(), false));
    };
    if equal.is_empty() {
        return Ok(best);
    }
    // The source appends later tied candidates, then the original best last.
    equal.push(best);
    Err(diagnostic(
        name,
        arguments,
        equal.iter().map(|&index| &candidates[index]),
        true,
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn argument_cost(
    argument: &BoundExpr,
    target: &DataType,
    context: &BindContext<'_>,
) -> Result<Option<u32>> {
    let string = super::coercion::string_literal(argument);
    let integer = super::coercion::full_integer_literal(argument);
    let fitting_integer = integer.is_some_and(|value| integer_fits(value, target));
    let mode = if string || fitting_integer {
        CastMode::Explicit
    } else {
        CastMode::Implicit
    };
    let Some(mut cost) = context.casts.coercion_cost_with_types(
        &argument.data_type,
        target,
        mode,
        context.query.types(),
    )?
    else {
        return Ok(None);
    };
    if string {
        // SQL's STRING_LITERAL pseudo-type is distinct from a VARCHAR column.
        // Availability/errors still come from the selected explicit conversion.
        cost = if *target == DataType::Varchar { 1 } else { 20 };
    } else if fitting_integer && argument.data_type != *target {
        cost = cost.saturating_sub(90);
    }
    Ok(Some(cost))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn integer_fits(value: IntegerLiteral, target: &DataType) -> bool {
    match value {
        IntegerLiteral::Signed(value) if target.is_unsigned_integer() => {
            value >= 0 && Value::Unsigned(value as u128).fits_type(target)
        }
        IntegerLiteral::Signed(value) => {
            target.is_signed_integer() && Value::Integer(value).fits_type(target)
        }
        IntegerLiteral::Unsigned(value) if target.is_signed_integer() => {
            i128::try_from(value).is_ok_and(|value| Value::Integer(value).fits_type(target))
        }
        IntegerLiteral::Unsigned(value) => {
            target.is_unsigned_integer() && Value::Unsigned(value).fits_type(target)
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn diagnostic<'a>(
    name: &str,
    arguments: &[BoundExpr],
    candidates: impl Iterator<Item = &'a ScalarSignature>,
    ambiguous: bool,
) -> Error {
    let arguments = arguments
        .iter()
        .map(|argument| {
            if super::coercion::string_literal(argument) {
                "STRING_LITERAL".into()
            } else if super::coercion::full_integer_literal(argument).is_some() {
                "INTEGER_LITERAL".into()
            } else {
                type_name(&argument.data_type)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let call = format!("{name}({arguments})");
    let mut message = if ambiguous {
        format!(
            "Could not choose a best candidate function for the function call \"{call}\". In order to select one, please add explicit type casts.\n\tCandidate functions:\n"
        )
    } else {
        format!(
            "No function matches the given name and argument types '{call}'. You might need to add explicit type casts.\n\tCandidate functions:\n"
        )
    };
    for candidate in candidates {
        let arguments = candidate
            .arguments
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let name = candidate
                    .argument_names
                    .as_ref()
                    .map_or_else(|| format!("col{index}"), |names| names[index].clone());
                format!("{name} {}", type_name(kind))
            })
            .collect::<Vec<_>>()
            .join(", ");
        message.push_str(&format!(
            "\t{name}({arguments}) -> {}\n",
            type_name(&candidate.return_type)
        ));
    }
    Error::Bind(message)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn type_name(kind: &DataType) -> String {
    match kind {
        DataType::Null => "\"NULL\"".into(),
        DataType::TimeTz => "TIME WITH TIME ZONE".into(),
        DataType::TimestampTz => "TIMESTAMP WITH TIME ZONE".into(),
        _ => kind.to_string(),
    }
}
