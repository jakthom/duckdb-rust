use super::*;
use crate::{
    common::{Value, type_registry::BoundType},
    execution::subquery::PreparedExpression,
};
use std::cmp::Ordering;

/// Fallible stable merge sorting retains arbitrary type adapters and expressions.
#[derive(Debug, Default)]
pub struct ComparisonSort;
impl SortAlgorithm for ComparisonSort {
    fn name(&self) -> &'static str {
        "comparison-sort"
    }
    fn sort(
        &self,
        input: &mut dyn BatchStream,
        order: &[OrderExpr],
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        let types = order
            .iter()
            .map(|o| context.query.types().bind(&o.expression.data_type))
            .collect::<Result<Vec<_>>>()?;
        let expressions = order
            .iter()
            .map(|o| PreparedExpression::new(&o.expression))
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        while let Some(batch) = input.next(context.query.batch_size())? {
            context
                .query
                .check_rows(rows.len().saturating_add(batch.len()))?;
            rows.extend(batch.rows());
        }
        let mut keys = Vec::with_capacity(rows.len());
        for row in &rows {
            context.query.check()?;
            keys.push(
                expressions
                    .iter()
                    .zip(&types)
                    .map(|(expression, data_type)| {
                        let value = expression.evaluate(row, context)?;
                        data_type.validate(&value, context.query)?;
                        Ok(value)
                    })
                    .collect::<Result<Row>>()?,
            );
        }
        let mut permutation: Vec<_> = (0..rows.len()).collect();
        let mut scratch = vec![0; rows.len()];
        let mut width = 1usize;
        while width < rows.len() {
            for start in (0..rows.len()).step_by(width.saturating_mul(2)) {
                context.query.check()?;
                let middle = start.saturating_add(width).min(rows.len());
                let end = middle.saturating_add(width).min(rows.len());
                let (mut left, mut right) = (start, middle);
                for output in &mut scratch[start..end] {
                    let take_left = left < middle
                        && (right == end
                            || compare(
                                &keys[permutation[left]],
                                &keys[permutation[right]],
                                order,
                                &types,
                                context,
                            )? != Ordering::Greater);
                    let position = if take_left {
                        let p = left;
                        left += 1;
                        p
                    } else {
                        let p = right;
                        right += 1;
                        p
                    };
                    *output = permutation[position];
                }
            }
            std::mem::swap(&mut permutation, &mut scratch);
            width = width.saturating_mul(2);
        }
        context.query.check()?;
        let mut rows = rows.into_iter().map(Some).collect::<Vec<_>>();
        permutation
            .into_iter()
            .map(|index| {
                context.query.check()?;
                Ok(rows[index]
                    .take()
                    .expect("sort permutation visits each row once"))
            })
            .collect()
    }
}

fn compare(
    left: &[Value],
    right: &[Value],
    order: &[OrderExpr],
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Ordering> {
    context.query.check()?;
    for (((a, b), order), data_type) in left.iter().zip(right).zip(order).zip(types) {
        let comparison = match (a.is_null(), b.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if order.nulls_first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, true) => {
                if order.nulls_first {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, false) => {
                let comparison = data_type.compare(a, b, context.query)?;
                if order.descending {
                    comparison.reverse()
                } else {
                    comparison
                }
            }
        };
        if comparison != Ordering::Equal {
            return Ok(comparison);
        }
    }
    Ok(Ordering::Equal)
}
