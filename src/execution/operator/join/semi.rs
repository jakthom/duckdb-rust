use super::{EqualityKeys, ExecutionContext, JoinPlan, Stream, stream};
use crate::{common::Result, execution::subquery::PreparedExpression, planner::logical::JoinKind};
use std::collections::HashSet;

/// A bounded right-side set with incremental left-side delivery. Nothing is
/// built until the first outer batch exists. Keys are owned, output shares its
/// input columns, and each cursor owns all state; no query results are cached.
pub(super) fn open<'a>(
    plan: JoinPlan<'a>,
    keys: EqualityKeys,
    context: &'a ExecutionContext<'a>,
) -> Result<Stream<'a>> {
    let mut left = stream::open(plan.left, context)?;
    let mut index: Option<HashSet<Vec<u8>>> = None;
    Ok(stream::from_fn(move |max_rows| {
        while let Some(batch) = left.next(max_rows)? {
            if index.is_none() {
                let mut right = stream::open(plan.right, context)?;
                let expression = PreparedExpression::new(&keys.right);
                let mut set = HashSet::new();
                let mut count = 0usize;
                while let Some(input) = right.next(context.query.batch_size())? {
                    count = count.saturating_add(input.len());
                    context.query.check_rows(count)?;
                    let values = expression.evaluate_batch(&input, context)?;
                    keys.data_type
                        .for_each_key(&values, context.query, |_, key| {
                            if let Some(key) = key {
                                set.insert(key.to_vec());
                            }
                            Ok(())
                        })?;
                }
                index = Some(set);
            }
            let index = index.as_ref().expect("initialized join build");
            if index.is_empty() {
                if plan.kind == JoinKind::Anti {
                    return Ok(Some(batch));
                }
                continue;
            }
            let values = PreparedExpression::new(&keys.left).evaluate_batch(&batch, context)?;
            let mut selected = Vec::new();
            keys.data_type
                .for_each_key(&values, context.query, |position, key| {
                    let matched = key.is_some_and(|key| index.contains(key));
                    if matched == (plan.kind == JoinKind::Semi) {
                        selected.push(position);
                    }
                    Ok(())
                })?;
            if !selected.is_empty() {
                return batch.select(&selected).map(Some);
            }
        }
        Ok(None)
    }))
}
