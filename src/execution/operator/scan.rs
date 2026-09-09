//! Validate storage columns before filtering. Selected columns retain their
//! input ownership, without copying accepted rows into new vectors.
use crate::{
    common::{Result, type_registry::BoundType},
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
    let validators = schema
        .iter()
        .map(|field| context.query.types().bind(&field.data_type))
        .collect::<Result<Vec<BoundType>>>()?;
    let predicate = PreparedExpression::new(predicate);
    let mut row = Vec::new();
    Ok(stream::from_fn(move |max_rows| {
        while let Some(batch) = next_batch(scan.as_mut(), max_rows, context.query)? {
            // Validate the entire input batch before evaluating its predicate,
            // including rows the filter will reject.
            batch.validate(&validators, context.query)?;
            let mut selected = Vec::new();
            for index in 0..batch.len() {
                let input = batch.read_row(index, &mut row)?;
                if predicate.evaluate(input, context)?.as_bool()? == Some(true) {
                    selected.push(index);
                }
            }
            if !selected.is_empty() {
                return batch.select(&selected).map(Some);
            }
        }
        Ok(None)
    }))
}
