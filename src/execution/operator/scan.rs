//! Filter owned storage rows before constructing output vectors. This retains
//! the unfused scan's demand and validation without chunks for rejected rows.
use crate::{
    common::{Error, Result, type_registry::BoundType},
    execution::{
        ExecutionContext,
        stream::{self, Stream},
        subquery::PreparedExpression,
    },
    planner::{BoundExpr, Schema},
    storage::scan::{TableScan, next_batch},
};

#[cfg(test)]
mod tests;

pub(crate) fn filtered<'a>(
    mut scan: Box<dyn TableScan + 'a>,
    schema: &'a Schema,
    predicate: &'a BoundExpr,
    context: &'a ExecutionContext<'a>,
) -> Result<Stream<'a>> {
    let types = schema
        .iter()
        .map(|field| context.query.types().bind(&field.data_type))
        .collect::<Result<Vec<BoundType>>>()?;
    let predicate = PreparedExpression::new(predicate);
    Ok(stream::from_fn(move |max_rows| {
        while let Some(rows) = next_batch(scan.as_mut(), max_rows, context.query)? {
            // Validate the entire input batch before evaluating its predicate,
            // including rows the filter will reject.
            for (_, row) in &rows {
                if row.len() != types.len() {
                    return Err(Error::Internal("row width differs from schema".into()));
                }
                for (value, data_type) in row.iter().zip(&types) {
                    data_type
                        .validate(value, context.query)
                        .map_err(|error| match error {
                            Error::Conversion(_) => {
                                Error::Internal("table scan returned an invalid value".into())
                            }
                            other => other,
                        })?;
                }
            }
            let mut selected = Vec::new();
            for (_, row) in rows {
                if predicate.evaluate(&row, context)?.as_bool()? == Some(true) {
                    selected.push(row);
                }
            }
            if !selected.is_empty() {
                return stream::chunk(schema, &selected);
            }
        }
        Ok(None)
    }))
}
