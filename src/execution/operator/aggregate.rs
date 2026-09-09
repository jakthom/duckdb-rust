use std::collections::{HashMap, HashSet};

use super::super::{ExecutionContext, stream::BatchStream};
use crate::{
    common::{
        Error, Result, Row,
        vector::{DataChunk, Vector},
    },
    function::AggregateState,
    planner::{BoundExpr, ExprKind, logical::AggregateExpr},
};

struct Group {
    keys: Row,
    states: Vec<Box<dyn AggregateState>>,
    distinct: Vec<HashSet<Vec<u8>>>,
}

impl Group {
    fn new(
        keys: Row,
        aggregates: &[AggregateExpr],
        context: &ExecutionContext<'_>,
    ) -> Result<Self> {
        let states = aggregates
            .iter()
            .map(|a| {
                a.function.create_state(
                    &a.arguments
                        .iter()
                        .map(|e| e.data_type.clone())
                        .collect::<Vec<_>>(),
                    context.query.types(),
                )
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            keys,
            states,
            distinct: vec![HashSet::new(); aggregates.len()],
        })
    }
}

pub fn aggregate(
    input: &mut dyn BatchStream,
    groups: &[BoundExpr],
    aggregates: &[AggregateExpr],
    context: &ExecutionContext<'_>,
) -> Result<Vec<Row>> {
    // A single aggregate over value access can consume columns without changing
    // the order of effectful expressions or interleaved aggregate updates.
    if groups.is_empty()
        && let [aggregate] = aggregates
        && !aggregate.distinct
        && aggregate.filter.is_none()
        && aggregate
            .arguments
            .iter()
            .all(|argument| matches!(argument.kind, ExprKind::Column(_) | ExprKind::Literal(_)))
    {
        return ungrouped(input, aggregate, context);
    }
    let group_types = groups
        .iter()
        .map(|g| context.query.types().bind(&g.data_type))
        .collect::<Result<Vec<_>>>()?;
    let argument_types = aggregates
        .iter()
        .map(|a| {
            a.arguments
                .iter()
                .map(|e| context.query.types().bind(&e.data_type))
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let group_expressions = groups
        .iter()
        .map(crate::execution::subquery::PreparedExpression::new)
        .collect::<Vec<_>>();
    let argument_expressions = aggregates
        .iter()
        .map(|a| {
            a.arguments
                .iter()
                .map(crate::execution::subquery::PreparedExpression::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let filters = aggregates
        .iter()
        .map(|a| {
            a.filter
                .as_ref()
                .map(crate::execution::subquery::PreparedExpression::new)
        })
        .collect::<Vec<_>>();
    let mut indices = HashMap::<Vec<u8>, usize>::new();
    let mut states = Vec::<Group>::new();
    if groups.is_empty() {
        indices.insert(Vec::new(), 0);
        states.push(Group::new(Vec::new(), aggregates, context)?);
    }
    while let Some(batch) = input.next(context.query.batch_size())? {
        for row in batch.rows() {
            context.query.check()?;
            let values = group_expressions
                .iter()
                .map(|e| e.evaluate(&row, context))
                .collect::<Result<Row>>()?;
            let mut key = Vec::new();
            for (value, data_type) in values.iter().zip(&group_types) {
                data_type.append_key(value, &mut key, context.query)?;
            }
            let index = if let Some(index) = indices.get(&key) {
                *index
            } else {
                context.query.check_rows(states.len() + 1)?;
                let index = states.len();
                indices.insert(key, index);
                states.push(Group::new(values, aggregates, context)?);
                index
            };
            let group = &mut states[index];
            for (i, aggregate) in aggregates.iter().enumerate() {
                if let Some(filter) = &filters[i]
                    && filter.evaluate(&row, context)?.as_bool()? != Some(true)
                {
                    continue;
                }
                let args = argument_expressions[i]
                    .iter()
                    .map(|e| e.evaluate(&row, context))
                    .collect::<Result<Row>>()?;
                if aggregate.distinct {
                    let mut key = Vec::new();
                    for (value, data_type) in args.iter().zip(&argument_types[i]) {
                        data_type.append_key(value, &mut key, context.query)?;
                    }
                    if !group.distinct[i].insert(key) {
                        continue;
                    }
                    context.query.check_rows(group.distinct[i].len())?;
                }
                group.states[i].update(&args, context.query)?;
            }
        }
    }
    states
        .into_iter()
        .map(|group| {
            let mut row = group.keys;
            for state in group.states {
                row.push(state.finish()?);
            }
            Ok(row)
        })
        .collect()
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
