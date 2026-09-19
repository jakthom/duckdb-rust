//! Bounded native `CreateViewInfo` codec for a single-relation projection and
//! filter. Richer Rust views remain valid in private snapshots, while native
//! publication rejects them before effects.

use super::binary::{Encoder, Reader, corrupt};
use crate::{
    Value,
    catalog::expression::{
        StoredComparison, StoredConjunction, StoredExpression, StoredExpressionKind,
    },
    catalog::{TableName, ViewDefinition, ViewProjection, ViewQueryShape},
    common::{Error, Result},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write(
    output: &mut Encoder,
    definition: &ViewDefinition,
    version: u64,
    query: &crate::parallel::QueryContext,
) -> Result<()> {
    query.check()?;
    let shape = definition.query_shape.as_ref().ok_or_else(|| {
        Error::Unsupported(format!(
            "view {} has no native projection/filter representation",
            definition.name
        ))
    })?;
    let source = &shape.source;
    let mut projection = shape.projection.clone();
    if let ViewProjection::Expressions(expressions) = &mut projection
        && !definition.aliases.is_empty()
    {
        if definition.aliases.len() > expressions.len() {
            return Err(Error::Corrupt(format!(
                "view {} alias metadata does not match its projection",
                definition.name
            )));
        }
        for (expression, alias) in expressions.iter_mut().zip(&definition.aliases) {
            expression.alias = Some(alias.clone());
        }
    }
    output.property(100, 3); // VIEW_ENTRY
    if version < 69 {
        output.field(101);
        output.string("duckdb_rust")?;
        output.field(102);
        output.string(&definition.name.schema)?;
    }
    output.property(105, 0);
    if version >= 69 {
        output.field(111);
        output.property(100, 3);
        output.string("duckdb_rust")?;
        output.string(&definition.name.schema)?;
        output.string(&definition.name.name)?;
        output.end();
    }
    output.field(200);
    output.string(&definition.name.name)?;
    strings(output, 201, &definition.aliases)?;
    output.property(202, definition.types.len() as u64);
    for ty in &definition.types {
        super::primitive::write_type(output, ty)?;
    }
    output.field(203);
    output.boolean(true);
    select_query(
        output,
        source,
        &projection,
        shape.filter.as_ref(),
        version,
        query,
    )?;
    strings(output, 204, &definition.names)?;
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn strings(output: &mut Encoder, field: u16, values: &[String]) -> Result<()> {
    if !values.is_empty() {
        output.property(field, values.len() as u64);
        for value in values {
            output.string(value)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn select_query(
    output: &mut Encoder,
    source: &TableName,
    projection: &ViewProjection,
    filter: Option<&StoredExpression>,
    version: u64,
    query: &crate::parallel::QueryContext,
) -> Result<()> {
    // SelectStatement
    output.field(100);
    output.boolean(true);
    // QueryNode + SelectNode
    output.property(100, 1); // SELECT_NODE
    output.property(101, 0); // modifiers
    output.field(102);
    output.property(100, 0);
    output.end(); // empty CTE map
    let count = match projection {
        ViewProjection::Star => 1,
        ViewProjection::Expressions(values) => values.len(),
    };
    output.property(200, count as u64);
    match projection {
        ViewProjection::Star => {
            output.boolean(true);
            output.property(100, 11);
            output.property(101, 200);
            output.property(201, 0);
            output.end();
        }
        ViewProjection::Expressions(values) => {
            for expression in values {
                output.boolean(true);
                super::parsed::write(output, expression, version, query)?;
            }
        }
    }
    output.field(201);
    output.boolean(true);
    output.property(100, 1); // BASE_TABLE
    output.field(200);
    output.string(&source.schema)?;
    output.field(201);
    output.string(&source.name)?;
    output.end();
    if let Some(filter) = filter {
        output.field(202);
        output.boolean(true);
        super::parsed::write(output, filter, version, query)?;
    }
    output.property(203, 0); // groups
    output.property(204, 0); // grouping sets
    output.property(205, 0); // STANDARD_HANDLING
    output.end(); // SelectNode
    output.end(); // SelectStatement
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_query(
    reader: &mut Reader,
    version: u64,
    query: &crate::parallel::QueryContext,
) -> Result<(String, ViewQueryShape)> {
    if !reader
        .boolean()
        .map_err(|_| corrupt("invalid view query presence"))?
    {
        return Err(corrupt("view has no query"));
    }
    reader.field(100)?;
    if !reader
        .boolean()
        .map_err(|_| corrupt("invalid SELECT node presence"))?
    {
        return Err(corrupt("view SELECT has no query node"));
    }
    reader.field(100)?;
    if reader.unsigned()? != 1 {
        return Err(Error::Unsupported("native view query node".into()));
    }
    if reader.optional_unsigned(101, 0)? != 0 {
        return Err(Error::Unsupported("native view modifiers".into()));
    }
    // Struct-valued properties are flattened by older binary serializers;
    // newer writers retain the enclosing property tag.
    if reader.peek()? == 102 {
        reader.field(102)?;
    }
    let ctes = reader.optional_unsigned(100, 0)?;
    if ctes != 0 {
        return Err(Error::Unsupported(format!("native view CTEs ({ctes})")));
    }
    reader.end()?;
    reader.field(200)?;
    let projection_count = reader.length()?;
    if projection_count == 0 {
        return Err(corrupt("native view has empty projection"));
    }
    let mut projections = Vec::with_capacity(projection_count);
    let mut retained_projection = Vec::with_capacity(projection_count);
    let mut star = false;
    for _ in 0..projection_count {
        if !reader
            .boolean()
            .map_err(|_| corrupt("invalid view projection presence"))?
        {
            return Err(corrupt("NULL view projection"));
        }
        let start = reader.position;
        reader.field(100)?;
        let class = reader.unsigned()?;
        reader.position = start;
        if class == 11 {
            if projection_count != 1 {
                return Err(Error::Unsupported("native mixed star projection".into()));
            }
            read_star(reader)?;
            projections.push("*".into());
            star = true;
        } else {
            let expression = super::parsed::read(reader, version, query)?;
            projections.push(expression_sql(&expression)?);
            retained_projection.push(expression);
        }
    }
    reader.field(201)?;
    if !reader
        .boolean()
        .map_err(|_| corrupt("invalid view FROM presence"))?
    {
        return Err(corrupt("view has no FROM relation"));
    }
    reader.field(100)?;
    if reader.unsigned()? != 1 {
        return Err(Error::Unsupported("native view table reference".into()));
    }
    if reader.optional(101)? {
        return Err(Error::Unsupported("native view table alias".into()));
    }
    if reader.optional(102)? && reader.boolean()? {
        return Err(Error::Unsupported("native view sampled table".into()));
    }
    if reader.optional(103)? {
        reader.unsigned()?;
    }
    if reader.optional(104)? {
        reader.unsigned()?;
    }
    let mut schema = if reader.optional(200)? {
        reader.string()?
    } else {
        "main".into()
    };
    reader.field(201)?;
    let mut table = reader.string()?;
    if reader.optional_unsigned(202, 0)? != 0 {
        return Err(Error::Unsupported("native view column aliases".into()));
    }
    if reader.optional(203)? && !reader.string()?.is_empty() {
        return Err(Error::Unsupported(
            "native cross-catalog view source".into(),
        ));
    }
    if reader.optional(204)? {
        return Err(Error::Unsupported("native view AS OF".into()));
    }
    if reader.optional(205)? {
        let path = if reader.optional(100)? {
            (0..reader.length()?)
                .map(|_| reader.string())
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        reader.end()?;
        match path.as_slice() {
            [name] => {
                table = name.clone();
            }
            [scope, name] => {
                schema = scope.clone();
                table = name.clone();
            }
            [_, _, _] => {
                return Err(Error::Unsupported(
                    "native cross-catalog view source".into(),
                ));
            }
            [] => {}
            _ => {
                return Err(Error::Unsupported(
                    "native nested qualified view source".into(),
                ));
            }
        }
    }
    reader.end()?;
    let filter = if reader.optional(202)? {
        if !reader.boolean()? {
            return Err(corrupt("NULL native view WHERE"));
        }
        Some(super::parsed::read(reader, version, query)?)
    } else {
        None
    };
    if reader.optional_unsigned(203, 0)? != 0 || reader.optional_unsigned(204, 0)? != 0 {
        return Err(Error::Unsupported("native view grouping".into()));
    }
    reader.field(205)?;
    if reader.unsigned()? != 0 {
        return Err(Error::Unsupported("native view aggregate mode".into()));
    }
    for field in [206, 207, 208] {
        if reader.optional(field)? {
            return Err(Error::Unsupported("native view clause".into()));
        }
    }
    reader.end()?;
    if reader.optional_unsigned(101, 0)? != 0 {
        return Err(Error::Unsupported("native view named parameters".into()));
    }
    reader.end()?;
    let mut sql = format!(
        "SELECT {} FROM {}.{}",
        projections.join(", "),
        quote(&schema),
        quote(&table)
    );
    if let Some(filter) = &filter {
        sql.push_str(" WHERE ");
        sql.push_str(&expression_sql(filter)?);
    }
    Ok((
        sql,
        ViewQueryShape {
            source: TableName::new(schema, table),
            projection: if star {
                ViewProjection::Star
            } else {
                ViewProjection::Expressions(retained_projection)
            },
            filter,
        },
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn read_star(reader: &mut Reader) -> Result<()> {
    reader.field(100)?;
    if reader.unsigned()? != 11 {
        return Err(corrupt("view star class"));
    }
    reader.field(101)?;
    if reader.unsigned()? != 200 {
        return Err(Error::Unsupported("native table star".into()));
    }
    if reader.optional(102)? {
        reader.string()?;
    }
    if reader.optional(103)? {
        reader.unsigned()?;
    }
    if reader.optional(104)? {
        reader.unsigned()?;
    }
    if reader.optional_unsigned(201, 0)? != 0 {
        return Err(Error::Unsupported("native view EXCLUDE".into()));
    }
    if reader.optional(202)? {
        return Err(Error::Unsupported("native view REPLACE".into()));
    }
    if reader.optional(203)? && reader.boolean()? {
        return Err(Error::Unsupported("native COLUMNS expression".into()));
    }
    if reader.optional(204)? {
        return Err(Error::Unsupported("native star expression body".into()));
    }
    if reader.optional_unsigned(206, 0)? != 0 || reader.optional_unsigned(207, 0)? != 0 {
        return Err(Error::Unsupported("native qualified star modifiers".into()));
    }
    reader.end()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn expression_sql(expression: &StoredExpression) -> Result<String> {
    let body = match &expression.kind {
        StoredExpressionKind::ColumnReference(parts) => parts
            .iter()
            .map(|part| quote(part))
            .collect::<Vec<_>>()
            .join("."),
        StoredExpressionKind::Literal {
            value: Value::Integer(value),
            ..
        } => value.to_string(),
        StoredExpressionKind::Literal {
            value: Value::Varchar(value),
            ..
        } => format!("'{}'", value.replace('\'', "''")),
        StoredExpressionKind::Comparison { kind, left, right } => format!(
            "({} {} {})",
            expression_sql(left)?,
            match kind {
                StoredComparison::Equal => "=",
                StoredComparison::NotEqual => "<>",
                StoredComparison::LessThan => "<",
                StoredComparison::GreaterThan => ">",
                StoredComparison::LessThanOrEqual => "<=",
                StoredComparison::GreaterThanOrEqual => ">=",
            },
            expression_sql(right)?
        ),
        StoredExpressionKind::Conjunction { kind, children } => {
            let separator = if *kind == StoredConjunction::And {
                " AND "
            } else {
                " OR "
            };
            format!(
                "({})",
                children
                    .iter()
                    .map(expression_sql)
                    .collect::<Result<Vec<_>>>()?
                    .join(separator)
            )
        }
        StoredExpressionKind::Function {
            name,
            arguments,
            is_operator: true,
            ..
        } if arguments.len() == 2 => {
            format!(
                "({} {} {})",
                expression_sql(&arguments[0].expression)?,
                name.join("."),
                expression_sql(&arguments[1].expression)?
            )
        }
        _ => {
            return Err(Error::Unsupported(
                "native view expression SQL conversion".into(),
            ));
        }
    };
    Ok(match &expression.alias {
        Some(alias) => format!("{body} AS {}", quote(alias)),
        None => body,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::QueryContext;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn star_query(catalog_field: Option<&str>, path: Option<&[&str]>) -> Result<Vec<u8>> {
        let mut output = Encoder::default();
        output.boolean(true);
        output.field(100);
        output.boolean(true);
        output.property(100, 1);
        output.property(101, 0);
        output.field(102);
        output.property(100, 0);
        output.end();
        output.property(200, 1);
        output.boolean(true);
        output.property(100, 11);
        output.property(101, 200);
        output.property(201, 0);
        output.end();
        output.field(201);
        output.boolean(true);
        output.property(100, 1);
        output.field(200);
        output.string("main")?;
        output.field(201);
        output.string("local_name")?;
        if let Some(catalog) = catalog_field {
            output.field(203);
            output.string(catalog)?;
        }
        if let Some(path) = path {
            output.field(205);
            output.field(100);
            output.unsigned(path.len() as u64);
            for part in path {
                output.string(part)?;
            }
            output.end();
        }
        output.end();
        output.property(203, 0);
        output.property(204, 0);
        output.property(205, 0);
        output.end();
        output.end();
        Ok(output.0)
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn native_view_reader_rejects_both_cross_catalog_source_encodings() -> Result<()> {
        let query = QueryContext::background();
        for (version, bytes) in [
            (64, star_query(Some("attached"), None)?),
            (
                69,
                star_query(None, Some(&["attached", "main", "remote_name"]))?,
            ),
        ] {
            assert!(matches!(
                read_query(&mut Reader::new(bytes), version, &query),
                Err(Error::Unsupported(message)) if message == "native cross-catalog view source"
            ));
        }
        Ok(())
    }
}
