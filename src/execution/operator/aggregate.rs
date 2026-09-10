//! Aggregation algorithms consume the same bound grouping and function contract.
use super::super::{ExecutionContext, stream::BatchStream};
use crate::{
    common::{
        Error, Result, Row,
        vector::{DataChunk, Vector},
    },
    planner::{ExprKind, aggregation::Aggregation, logical::AggregateExpr},
};
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
};

mod batched;
mod grouped;

/// Consumes a validated input stream once and owns all group, aggregate and
/// DISTINCT state until completion. Each set has independent state; group
/// expressions and each aggregate's arguments/filter are evaluated once per
/// input row, regardless of set count. Function updates retain input order.
/// Output order is unspecified. Failure/cancellation discards all state and
/// publishes no partial result; retained groups obey the query row budget.
pub trait AggregationAlgorithm: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn aggregate(
        &self,
        input: &mut dyn BatchStream,
        aggregation: &Aggregation,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>>;
}

#[derive(Debug, Default)]
pub struct HashAggregation;
impl AggregationAlgorithm for HashAggregation {
    fn name(&self) -> &'static str {
        "hash-aggregation"
    }
    fn aggregate(
        &self,
        input: &mut dyn BatchStream,
        aggregation: &Aggregation,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        if let Some(rows) = batched::try_run(input, aggregation, context)? {
            return Ok(rows);
        }
        grouped::run::<HashMap<Vec<u8>, usize>>(input, aggregation, context)
    }
}

/// Ordered key storage is an independent grouping index with the same key
/// semantics and emission contract. Byte ordering does not define SQL ordering.
#[derive(Debug, Default)]
pub struct OrderedAggregation;
impl AggregationAlgorithm for OrderedAggregation {
    fn name(&self) -> &'static str {
        "ordered-aggregation"
    }
    fn aggregate(
        &self,
        input: &mut dyn BatchStream,
        aggregation: &Aggregation,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        grouped::run::<BTreeMap<Vec<u8>, usize>>(input, aggregation, context)
    }
}
fn ungrouped(
    input: &mut dyn BatchStream,
    aggregate: &AggregateExpr,
    context: &ExecutionContext<'_>,
) -> Result<Vec<Row>> {
    let types = aggregate
        .arguments
        .iter()
        .map(|argument| argument.data_type.clone())
        .collect::<Vec<_>>();
    let mut state = aggregate
        .function
        .create_state(&types, context.query.types())?;
    while let Some(batch) = input.next(context.query.batch_size())? {
        let columns = aggregate
            .arguments
            .iter()
            .map(|argument| match &argument.kind {
                ExprKind::Column(index) => batch
                    .columns()
                    .get(*index)
                    .cloned()
                    .ok_or_else(|| Error::Internal("aggregate column outside input".into())),
                ExprKind::Literal(value) => {
                    Vector::constant(argument.data_type.clone(), value.clone(), batch.len())
                }
                _ => Err(Error::Internal(
                    "aggregate argument requires row evaluation".into(),
                )),
            })
            .collect::<Result<_>>()?;
        state.update_batch(&DataChunk::new(columns, batch.len())?, context.query)?;
        context.query.check()?;
    }
    Ok(vec![vec![state.finish()?]])
}
