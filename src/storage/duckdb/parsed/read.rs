use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn expression(
    reader: &mut Reader,
    depth: usize,
    state: &mut State<'_>,
) -> Result<StoredExpression> {
    state.visit(depth)?;
    reader.field(100)?;
    let class = reader.unsigned()?;
    reader.field(101)?;
    let kind = reader.unsigned()?;
    let alias = state.optional_name(reader, 102)?;
    if reader.optional(103)? {
        reader.unsigned()?;
    }
    if reader.optional(104)? {
        u32::try_from(reader.unsigned()?)
            .map_err(|_| corrupt("expression source span overflow"))?;
    }
    let kind = match (class, kind) {
        (7, 75) => {
            reader.field(200)?;
            let (data_type, value) = state.values.read_typed(reader)?;
            StoredExpressionKind::Literal { data_type, value }
        }
        (3, 12) => {
            if !reader.optional(200)? {
                return Err(corrupt("cast has no expression"));
            }
            state.count(1)?;
            let expression = Box::new(child(reader, depth + 1, state)?);
            reader.field(201)?;
            let target = state.values.read_type(reader)?;
            let try_cast = reader.optional(202)? && reader.boolean()?;
            StoredExpressionKind::Cast {
                expression,
                target,
                try_cast,
            }
        }
        (9, 140) => function(reader, depth, state)?,
        (10, 153 | 155 | 156) => {
            let children = children(reader, 200, depth, state)?;
            let kind = match kind {
                153 => StoredOperator::Index,
                155 => StoredOperator::Field,
                156 => StoredOperator::ListConstructor,
                _ => unreachable!(),
            };
            if kind != StoredOperator::ListConstructor && children.len() != 2 {
                return Err(corrupt("native accessor requires two children"));
            }
            StoredExpressionKind::Operator { kind, children }
        }
        _ => {
            return Err(Error::Unsupported(format!(
                "native retained expression class {class}, kind {kind}"
            )));
        }
    };
    reader.end()?;
    Ok(StoredExpression { alias, kind })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn child(reader: &mut Reader, depth: usize, state: &mut State<'_>) -> Result<StoredExpression> {
    if !reader.boolean()? {
        return Err(corrupt("NULL native expression pointer"));
    }
    expression(reader, depth, state)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn children(
    reader: &mut Reader,
    field: u16,
    depth: usize,
    state: &mut State<'_>,
) -> Result<Vec<StoredExpression>> {
    if !reader.optional(field)? {
        return Ok(Vec::new());
    }
    let count = state.count(reader.length()?)?;
    let mut children = reserve(count)?;
    for _ in 0..count {
        children.push(child(reader, depth + 1, state)?);
    }
    Ok(children)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn function(
    reader: &mut Reader,
    depth: usize,
    state: &mut State<'_>,
) -> Result<StoredExpressionKind> {
    let name = state.optional_name(reader, 200)?;
    let schema = state.optional_name(reader, 201)?;
    let legacy = children(reader, 202, depth, state)?;
    if reader.optional(203)? && reader.boolean()? {
        return Err(Error::Unsupported("retained function FILTER".into()));
    }
    if reader.optional(204)? && reader.boolean()? {
        reader.field(100)?;
        if reader.unsigned()? != 2 {
            return Err(corrupt("expected ORDER_MODIFIER"));
        }
        if reader.optional(200)? && reader.length()? != 0 {
            return Err(Error::Unsupported("retained function ORDER BY".into()));
        }
        reader.end()?;
    }
    if reader.optional(205)? && reader.boolean()? {
        return Err(Error::Unsupported("retained DISTINCT function".into()));
    }
    let is_operator = reader.optional(206)? && reader.boolean()?;
    if reader.optional(207)? && reader.boolean()? {
        return Err(Error::Unsupported("retained function export state".into()));
    }
    let catalog = state.optional_name(reader, 208)?;
    let (arguments, argument_style) = if reader.optional(209)? {
        if !legacy.is_empty() {
            return Err(Error::Unsupported(
                "mixed legacy and modern function arguments".into(),
            ));
        }
        let count = state.count(reader.length()?)?;
        let mut arguments = reserve(count)?;
        for _ in 0..count {
            let name = state.optional_name(reader, 100)?;
            if !reader.optional(101)? {
                return Err(corrupt("function argument has no expression"));
            }
            let expression = child(reader, depth + 1, state)?;
            reader.end()?;
            arguments.push(StoredArgument { name, expression });
        }
        (arguments, StoredArgumentStyle::Named)
    } else if legacy.is_empty() {
        (Vec::new(), StoredArgumentStyle::Named)
    } else {
        (
            legacy
                .into_iter()
                .map(|expression| StoredArgument {
                    name: expression.alias.clone(),
                    expression,
                })
                .collect(),
            StoredArgumentStyle::LegacyAliases,
        )
    };
    // QualifiedName is authoritative in development. Reject contradictory
    // duplicate identity rather than quietly dropping either representation.
    let qualified = if reader.optional(210)? {
        let count = if reader.optional(100)? {
            reader.length()?
        } else {
            0
        };
        if count > 64 {
            return Err(Error::Resource(
                "native function qualification limit".into(),
            ));
        }
        let mut parts = reserve(count)?;
        for _ in 0..count {
            let part = state.string(reader)?;
            if part.is_empty() {
                return Err(corrupt("empty native qualification component"));
            }
            parts.push(part);
        }
        reader.end()?;
        parts
    } else {
        Vec::new()
    };
    let name = if !qualified.is_empty() {
        let n = qualified.len();
        for (legacy, index) in [
            (name.as_ref(), n.checked_sub(1)),
            (schema.as_ref(), n.checked_sub(2)),
            (catalog.as_ref(), (n >= 3).then_some(0)),
        ] {
            if let Some(legacy) = legacy
                && index.is_none_or(|index| qualified[index] != *legacy)
            {
                return Err(Error::Unsupported(
                    "conflicting native function qualification".into(),
                ));
            }
        }
        qualified
    } else {
        if catalog.is_some() && schema.is_none() {
            return Err(Error::Unsupported(
                "catalog-only native function qualification".into(),
            ));
        }
        let name = name.ok_or_else(|| corrupt("native function has no name"))?;
        catalog
            .into_iter()
            .chain(schema)
            .chain(Some(name))
            .collect()
    };
    Ok(StoredExpressionKind::Function {
        name,
        arguments,
        is_operator,
        argument_style,
    })
}
