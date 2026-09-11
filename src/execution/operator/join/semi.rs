use super::{
    EqualityKeys, ExecutionContext, JoinPlan, Stream,
    membership::{MembershipBuilder, MembershipIndex},
    stream,
};
use crate::{common::Result, execution::subquery::PreparedExpression, planner::logical::JoinKind};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// A bounded right-side set with incremental left-side delivery. Nothing is
/// built until the first outer batch exists. Keys are owned, output shares its
/// input columns, and each cursor owns all state; no query results are cached.
pub(super) fn open<'a>(
    plan: JoinPlan<'a>,
    keys: EqualityKeys,
    context: &'a ExecutionContext<'a>,
) -> Result<Stream<'a>> {
    let mut left = stream::open(plan.left, context)?;
    let mut index: Option<MembershipIndex> = None;
    Ok(stream::from_fn(move |max_rows| {
        while let Some(batch) = left.next(max_rows)? {
            if index.is_none() {
                let mut right = stream::open(plan.right, context)?;
                let expression = PreparedExpression::new(&keys.right);
                let mut set = MembershipBuilder::new(keys.data_type.clone());
                let mut count = 0usize;
                while let Some(input) = right.next(context.query.batch_size())? {
                    count = count.saturating_add(input.len());
                    context.query.check_rows(count)?;
                    let values = expression.evaluate_batch(&input, context)?;
                    set.insert(&values, context.query)?;
                }
                index = Some(set.finish(context.query)?);
            }
            let index = index.as_ref().expect("initialized join build");
            if index.is_empty() {
                if plan.kind == JoinKind::Anti {
                    return Ok(Some(batch));
                }
                continue;
            }
            let values = PreparedExpression::new(&keys.left).evaluate_batch(&batch, context)?;
            let selected = index.select(&values, plan.kind == JoinKind::Semi, context.query)?;
            if !selected.is_empty() {
                return batch.select(&selected).map(Some);
            }
        }
        Ok(None)
    }))
}
