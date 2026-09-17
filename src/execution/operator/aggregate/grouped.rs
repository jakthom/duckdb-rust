use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
};

use super::*;
use crate::{
    Value,
    execution::subquery::PreparedExpression,
    function::{AggregateState, OrderedAggregateStrategy},
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
    /// Rows buffered only for aggregate calls with an argument ORDER BY.
    /// The ordinary aggregate path keeps its streaming state and allocation
    /// behaviour unchanged.
    ordered: Vec<Vec<(Row, Row)>>,
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
            ordered: vec![Vec::new(); functions.len()],
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
        && aggregate.order_by.is_empty()
        && aggregate.arguments.iter().all(BoundExpr::is_pure_and_total)
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
    let order_expressions = functions
        .iter()
        .map(|aggregate| {
            aggregate
                .order_by
                .iter()
                .map(|order| PreparedExpression::new(&order.expression))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let order_types = functions
        .iter()
        .map(|aggregate| {
            aggregate
                .order_by
                .iter()
                .map(|order| context.query.types().bind(&order.expression.data_type))
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let ordered_strategies = functions
        .iter()
        .map(|aggregate| {
            aggregate.function.ordered_strategy(
                &aggregate
                    .arguments
                    .iter()
                    .map(|argument| argument.data_type.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let mut indices: Vec<I> = (0..aggregation.sets.len()).map(|_| I::default()).collect();
    let mut states = Vec::new();
    // Buffered aggregates retain one row plus two stable-sort index vectors.
    // FIRST/LAST retain a single candidate row instead. This is a shared
    // total across every group and aggregate, not a per-buffer limit.
    let mut ordered_units = 0usize;
    for (set_index, set) in aggregation.sets.iter().enumerate() {
        if set.is_empty() {
            context
                .query
                .check_rows(states.len().saturating_add(1).saturating_add(ordered_units))?;
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
                    context
                        .query
                        .check_rows(states.len().saturating_add(1).saturating_add(ordered_units))?;
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
                let order = order_expressions[i]
                    .iter()
                    .map(|expression| expression.evaluate(&row, context))
                    .collect::<Result<Row>>()?;
                let mut key = Vec::new();
                if aggregate.distinct {
                    for (value, data_type) in args.iter().zip(&argument_types[i]) {
                        data_type.append_key(value, &mut key, context.query)?;
                    }
                }
                let group_count = states.len();
                for &index in &matched {
                    context.query.check()?;
                    let group = &mut states[index];
                    if aggregate.distinct {
                        if !group.distinct[i].insert(key.clone()) {
                            continue;
                        }
                        context.query.check_rows(group.distinct[i].len())?;
                    }
                    if functions[i].order_by.is_empty() {
                        group.states[i].update(&args, context.query)?;
                    } else {
                        let strategy = ordered_strategies[i];
                        let replaces = candidate_replaces(
                            &group.ordered[i],
                            &order,
                            strategy,
                            &aggregate.order_by,
                            &order_types[i],
                            context,
                        )?;
                        let grows = replaces
                            && (strategy == OrderedAggregateStrategy::Buffered
                                || group.ordered[i].is_empty());
                        if grows {
                            let units = match strategy {
                                OrderedAggregateStrategy::Buffered => 3,
                                OrderedAggregateStrategy::First
                                | OrderedAggregateStrategy::Last => 1,
                            };
                            let next = ordered_units.checked_add(units).ok_or_else(|| {
                                Error::Resource("ordered aggregate row count overflow".into())
                            })?;
                            context.query.check_rows(group_count.saturating_add(next))?;
                            ordered_units = next;
                        }
                        retain_ordered(
                            &mut group.ordered[i],
                            &args,
                            &order,
                            strategy,
                            &aggregate.order_by,
                            &order_types[i],
                            context,
                        )?;
                    }
                }
            }
        }
    }
    states
        .into_iter()
        .map(|mut group| {
            context.query.check()?;
            for (index, aggregate) in functions.iter().enumerate() {
                update_ordered(
                    group.states[index].as_mut(),
                    &mut group.ordered[index],
                    &aggregate.order_by,
                    &order_types[index],
                    context,
                )?;
            }
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

/// Whether this row adds retained storage. Candidate replacement is constant
/// space; a generic buffered aggregate retains every row.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn candidate_replaces(
    rows: &[(Row, Row)],
    order: &Row,
    strategy: OrderedAggregateStrategy,
    expressions: &[crate::planner::logical::OrderExpr],
    types: &[crate::common::type_registry::BoundType],
    context: &ExecutionContext<'_>,
) -> Result<bool> {
    let Some((_, current)) = rows.first() else {
        return Ok(true);
    };
    match strategy {
        OrderedAggregateStrategy::Buffered => Ok(true),
        OrderedAggregateStrategy::First => {
            Ok(compare_ordered(order, current, expressions, types, context)? == Ordering::Less)
        }
        OrderedAggregateStrategy::Last => {
            // Stable ascending LAST retains the newest row whose key is at
            // least the current candidate, including an equal-key tie.
            Ok(compare_ordered(order, current, expressions, types, context)? != Ordering::Less)
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn retain_ordered(
    rows: &mut Vec<(Row, Row)>,
    args: &Row,
    order: &Row,
    strategy: OrderedAggregateStrategy,
    expressions: &[crate::planner::logical::OrderExpr],
    types: &[crate::common::type_registry::BoundType],
    context: &ExecutionContext<'_>,
) -> Result<()> {
    if !candidate_replaces(rows, &order, strategy, expressions, types, context)? {
        return Ok(());
    }
    match strategy {
        OrderedAggregateStrategy::Buffered => rows.push((args.clone(), order.clone())),
        OrderedAggregateStrategy::First | OrderedAggregateStrategy::Last => {
            rows.clear();
            rows.push((args.clone(), order.clone()));
        }
    }
    Ok(())
}

/// Sort buffered aggregate arguments stably, then use the normal aggregate
/// state. This mirrors DuckDB's sorted-aggregate wrapper while deliberately
/// keeping the initial implementation local to blocking hash aggregation.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn update_ordered(
    state: &mut dyn AggregateState,
    rows: &mut [(Row, Row)],
    order: &[crate::planner::logical::OrderExpr],
    types: &[crate::common::type_registry::BoundType],
    context: &ExecutionContext<'_>,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut permutation = (0..rows.len()).collect::<Vec<_>>();
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
                        || compare_ordered(
                            &rows[permutation[left]].1,
                            &rows[permutation[right]].1,
                            order,
                            types,
                            context,
                        )? != Ordering::Greater);
                let position = if take_left {
                    let position = left;
                    left += 1;
                    position
                } else {
                    let position = right;
                    right += 1;
                    position
                };
                *output = permutation[position];
            }
        }
        std::mem::swap(&mut permutation, &mut scratch);
        width = width.saturating_mul(2);
    }
    for index in permutation {
        context.query.check()?;
        state.update(&rows[index].0, context.query)?;
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare_ordered(
    left: &[Value],
    right: &[Value],
    order: &[crate::planner::logical::OrderExpr],
    types: &[crate::common::type_registry::BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Ordering> {
    for (((left, right), order), data_type) in left.iter().zip(right).zip(order).zip(types) {
        let comparison = match (left.is_null(), right.is_null()) {
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
                let comparison = data_type.compare(left, right, context.query)?;
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
