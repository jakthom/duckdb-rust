use std::collections::{BTreeMap, HashMap, HashSet};

use super::*;
use crate::{
    Value, execution::subquery::PreparedExpression, function::AggregateState,
    planner::aggregation::AggregateOutput,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) trait GroupIndex: Default {
    fn find(&self, key: &[u8]) -> Option<usize>;
    fn add(&mut self, key: Vec<u8>, index: usize);
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl GroupIndex for HashMap<Vec<u8>, usize> {
    fn find(&self, key: &[u8]) -> Option<usize> {
        self.get(key).copied()
    }
    fn add(&mut self, key: Vec<u8>, index: usize) {
        self.insert(key, index);
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl GroupIndex for BTreeMap<Vec<u8>, usize> {
    fn find(&self, key: &[u8]) -> Option<usize> {
        self.get(key).copied()
    }
    fn add(&mut self, key: Vec<u8>, index: usize) {
        self.insert(key, index);
    }
}

struct Group {
    keys: Row,
    set: usize,
    states: Vec<Box<dyn AggregateState>>,
    distinct: Vec<HashSet<Vec<u8>>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Group {
    fn new(
        keys: Row,
        set: usize,
        functions: &[&AggregateExpr],
        context: &ExecutionContext<'_>,
    ) -> Result<Self> {
        let states = functions
            .iter()
            .map(|function| {
                function.function.create_state(
                    &function
                        .arguments
                        .iter()
                        .map(|e| e.data_type.clone())
                        .collect::<Vec<_>>(),
                    context.query.types(),
                )
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            keys,
            set,
            states,
            distinct: vec![HashSet::new(); functions.len()],
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn run<I: GroupIndex>(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Vec<Row>> {
    aggregation.validate_metadata(context.query)?;
    let groups = &aggregation.groups;
    if groups.is_empty()
        && aggregation.sets.len() == 1
        && aggregation.sets[0].is_empty()
        && let [AggregateOutput::Function(aggregate)] = aggregation.outputs.as_slice()
        && !aggregate.distinct
        && aggregate.filter.is_none()
        && aggregate.arguments.iter().all(|argument| {
            matches!(
                argument.kind,
                ExprKind::Column(_) | ExprKind::Literal(_) | ExprKind::Parameter(_)
            )
        })
    {
        return super::ungrouped(input, aggregate, context);
    }
    let functions = aggregation.functions().collect::<Vec<_>>();
    let group_types = groups
        .iter()
        .map(|group| context.query.types().bind(&group.data_type))
        .collect::<Result<Vec<_>>>()?;
    let argument_types = functions
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
        .map(PreparedExpression::new)
        .collect::<Vec<_>>();
    let argument_expressions = functions
        .iter()
        .map(|a| {
            a.arguments
                .iter()
                .map(PreparedExpression::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let filters = functions
        .iter()
        .map(|a| a.filter.as_ref().map(PreparedExpression::new))
        .collect::<Vec<_>>();
    let mut indices: Vec<I> = (0..aggregation.sets.len()).map(|_| I::default()).collect();
    let mut states = Vec::new();
    for (set_index, set) in aggregation.sets.iter().enumerate() {
        if set.is_empty() {
            context.query.check_rows(states.len() + 1)?;
            indices[set_index].add(Vec::new(), states.len());
            states.push(Group::new(
                vec![Value::Null; groups.len()],
                set_index,
                &functions,
                context,
            )?);
        }
    }
    let mut matched = Vec::with_capacity(aggregation.sets.len());
    while let Some(batch) = input.next(context.query.batch_size())? {
        for row in batch.rows() {
            context.query.check()?;
            let values = group_expressions
                .iter()
                .map(|e| e.evaluate(&row, context))
                .collect::<Result<Row>>()?;
            matched.clear();
            for (set_index, set) in aggregation.sets.iter().enumerate() {
                context.query.check()?;
                let mut key = Vec::new();
                for &ordinal in set.indices() {
                    group_types[ordinal].append_key(&values[ordinal], &mut key, context.query)?;
                }
                let index = if let Some(index) = indices[set_index].find(&key) {
                    index
                } else {
                    context.query.check_rows(states.len() + 1)?;
                    let index = states.len();
                    let keys = values
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            if set.contains(i) {
                                v.clone()
                            } else {
                                Value::Null
                            }
                        })
                        .collect();
                    states.push(Group::new(keys, set_index, &functions, context)?);
                    indices[set_index].add(key, index);
                    index
                };
                matched.push(index);
            }
            for (i, aggregate) in functions.iter().enumerate() {
                if let Some(filter) = &filters[i]
                    && filter.evaluate(&row, context)?.as_bool()? != Some(true)
                {
                    continue;
                }
                let args = argument_expressions[i]
                    .iter()
                    .map(|e| e.evaluate(&row, context))
                    .collect::<Result<Row>>()?;
                let mut key = Vec::new();
                if aggregate.distinct {
                    for (value, data_type) in args.iter().zip(&argument_types[i]) {
                        data_type.append_key(value, &mut key, context.query)?;
                    }
                }
                for &index in &matched {
                    context.query.check()?;
                    let group = &mut states[index];
                    if aggregate.distinct {
                        if !group.distinct[i].insert(key.clone()) {
                            continue;
                        }
                        context.query.check_rows(group.distinct[i].len())?;
                    }
                    group.states[i].update(&args, context.query)?;
                }
            }
        }
    }
    states
        .into_iter()
        .map(|group| {
            context.query.check()?;
            let mut row = group.keys;
            let mut functions = group.states.into_iter();
            for output in &aggregation.outputs {
                row.push(match output {
                    AggregateOutput::Function(_) => functions
                        .next()
                        .expect("validated aggregate state")
                        .finish()?,
                    AggregateOutput::Grouping(indices) => {
                        Value::Integer(indices.iter().fold(0, |mask, &index| {
                            (mask << 1) | i128::from(!aggregation.sets[group.set].contains(index))
                        }))
                    }
                });
            }
            Ok(row)
        })
        .collect()
}
