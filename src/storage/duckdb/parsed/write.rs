use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn expression(
    output: &mut Encoder,
    expression: &StoredExpression,
    depth: usize,
    state: &mut State<'_>,
) -> Result<()> {
    state.visit(depth)?;
    let (class, kind) = match &expression.kind {
        StoredExpressionKind::CurrentTimestamp => {
            return Err(Error::Unsupported(
                "native CURRENT_TIMESTAMP retained expression encoding".into(),
            ));
        }
        StoredExpressionKind::Literal { .. } => (7, 75),
        StoredExpressionKind::Cast { .. } => (3, 12),
        StoredExpressionKind::Function { .. } => (9, 140),
        StoredExpressionKind::Case { .. } => (2, 150),
        StoredExpressionKind::Comparison { kind, .. } => (
            5,
            match kind {
                StoredComparison::Equal => 25,
                StoredComparison::NotEqual => 26,
                StoredComparison::LessThan => 27,
                StoredComparison::GreaterThan => 28,
                StoredComparison::LessThanOrEqual => 29,
                StoredComparison::GreaterThanOrEqual => 30,
            },
        ),
        StoredExpressionKind::Conjunction { kind, .. } => (
            6,
            match kind {
                StoredConjunction::And => 50,
                StoredConjunction::Or => 51,
            },
        ),
        StoredExpressionKind::Between { .. } => (19, 38),
        StoredExpressionKind::Operator { kind, .. } => (
            10,
            match kind {
                StoredOperator::ListConstructor => 156,
                StoredOperator::Index => 153,
                StoredOperator::Field => 155,
                StoredOperator::Not => 13,
                StoredOperator::IsNull => 14,
                StoredOperator::IsNotNull => 15,
                StoredOperator::In => 35,
                StoredOperator::NotIn => 36,
            },
        ),
    };
    output.property(100, class);
    output.property(101, kind);
    state.write_name(output, 102, expression.alias.as_deref())?;
    if let Some(span) = expression.source_span {
        output.property(103, span.offset);
        if let Some(length) = span.length {
            output.property(104, u64::from(length));
        }
    }
    match &expression.kind {
        StoredExpressionKind::CurrentTimestamp => unreachable!("rejected before native encoding"),
        StoredExpressionKind::Literal { data_type, value } => {
            output.field(200);
            state.values.write_typed(output, data_type, value)?;
        }
        StoredExpressionKind::Cast {
            expression: inner,
            target,
            try_cast,
        } => {
            state.count(1)?;
            output.field(200);
            output.boolean(true);
            self::expression(output, inner, depth + 1, state)?;
            output.field(201);
            state.values.write_type(output, target)?;
            if *try_cast {
                output.field(202);
                output.boolean(true);
            }
        }
        StoredExpressionKind::Case { checks, otherwise } => {
            let count = checks
                .len()
                .checked_mul(2)
                .and_then(|count| count.checked_add(1))
                .ok_or_else(|| Error::Resource("native CASE child count overflow".into()))?;
            state.count(count)?;
            output.property(200, checks.len() as u64);
            for check in checks {
                output.field(100);
                output.boolean(true);
                self::expression(output, &check.when_expression, depth + 1, state)?;
                output.field(101);
                output.boolean(true);
                self::expression(output, &check.then_expression, depth + 1, state)?;
                output.end();
            }
            output.field(201);
            output.boolean(true);
            self::expression(output, otherwise, depth + 1, state)?;
        }
        StoredExpressionKind::Comparison { left, right, .. } => {
            state.count(2)?;
            output.field(200);
            output.boolean(true);
            self::expression(output, left, depth + 1, state)?;
            output.field(201);
            output.boolean(true);
            self::expression(output, right, depth + 1, state)?;
        }
        StoredExpressionKind::Conjunction { children, .. } => {
            state.count(children.len())?;
            output.property(200, children.len() as u64);
            for child in children {
                output.boolean(true);
                self::expression(output, child, depth + 1, state)?;
            }
        }
        StoredExpressionKind::Between {
            input,
            lower,
            upper,
        } => {
            state.count(3)?;
            for (field, child) in [(200, input), (201, lower), (202, upper)] {
                output.field(field);
                output.boolean(true);
                self::expression(output, child, depth + 1, state)?;
            }
        }
        StoredExpressionKind::Operator { children, .. } => {
            state.count(children.len())?;
            if !children.is_empty() {
                output.property(200, children.len() as u64);
                for child in children {
                    output.boolean(true);
                    self::expression(output, child, depth + 1, state)?;
                }
            }
        }
        StoredExpressionKind::Function {
            name,
            arguments,
            is_operator,
            argument_style,
        } => {
            state.count(arguments.len())?;
            if name.len() > 3 && state.version < 69 {
                return Err(Error::Unsupported(
                    "nested function qualification requires storage 69".into(),
                ));
            }
            let n = name.len();
            state.write_name(output, 200, Some(&name[n - 1]))?;
            if n >= 2 {
                state.write_name(output, 201, Some(&name[n - 2]))?;
            }
            // Pre-69 native records retain child aliases rather than modern
            // argument objects. Only positional SQL-origin arguments can
            // change representation without changing binding semantics: a
            // legacy child alias is not a named function argument.
            if state.version < 69
                && *argument_style == StoredArgumentStyle::Named
                && arguments.iter().any(|argument| argument.name.is_some())
            {
                return Err(Error::Unsupported(
                    "named function arguments require storage 69".into(),
                ));
            }
            let legacy = *argument_style == StoredArgumentStyle::LegacyAliases
                || (state.version < 69 && !arguments.is_empty());
            if legacy && arguments.is_empty() {
                return Err(Error::Unsupported(
                    "empty legacy argument provenance has no native representation".into(),
                ));
            }
            if legacy {
                output.property(202, arguments.len() as u64);
                for argument in arguments {
                    if argument.name != argument.expression.alias {
                        return Err(Error::Unsupported(
                            "legacy argument name differs from expression alias".into(),
                        ));
                    }
                    output.boolean(true);
                    self::expression(output, &argument.expression, depth + 1, state)?;
                }
            }
            // C++ ordinarily supplies an empty, non-NULL OrderModifier.
            output.field(204);
            output.boolean(true);
            output.property(100, 2);
            output.end();
            if *is_operator {
                output.field(206);
                output.boolean(true);
            }
            if n >= 3 {
                state.write_name(output, 208, Some(&name[0]))?;
            }
            if !legacy && !arguments.is_empty() {
                output.property(209, arguments.len() as u64);
                for argument in arguments {
                    state.write_name(output, 100, argument.name.as_deref())?;
                    output.field(101);
                    output.boolean(true);
                    self::expression(output, &argument.expression, depth + 1, state)?;
                    output.end();
                }
            }
            if state.version >= 69 {
                output.field(210);
                output.property(100, n as u64);
                for part in name {
                    state.bytes(part.len())?;
                    output.blob(part.as_bytes());
                }
                output.end();
            }
        }
    }
    output.end();
    state.query.check()
}
