//! Generic column evaluation for total buffered aggregate modifiers.
//!
//! The executor retains owned values by group and column. Ordered groups build
//! a stable row permutation and clone their values into the permuted delivery
//! vectors before calling the selected aggregate state's batch callback. This
//! deliberately favors a bounded, capability-checked implementation over a
//! shallow-chunk or zero-copy representation; measurements decide whether a
//! later storage specialization is warranted.
use super::index::IntegerIndex;
use crate::{
    DataType, Value,
    common::{
        Error, Result, Row,
        type_registry::BoundType,
        vector::{DataChunk, Vector},
    },
    execution::{ExecutionContext, stream::BatchStream, subquery::PreparedExpression},
    function::{
        AggregateFunction, AggregateModifierStrategy, OrderedAggregateStrategy,
        grouped::{GroupSelection, GroupedAggregateState},
    },
    planner::{
        aggregation::{AggregateOutput, Aggregation},
        logical::OrderExpr,
    },
};
use std::{cmp::Ordering, collections::HashSet, sync::Arc};

enum Accumulator {
    Grouped(Box<dyn GroupedAggregateState>),
    Buffered(BufferedAccumulator),
}

struct BufferedAccumulator {
    function: Arc<dyn AggregateFunction>,
    argument_types: Vec<DataType>,
    argument_keys: Vec<BoundType>,
    order: Vec<OrderExpr>,
    order_types: Vec<BoundType>,
    distinct: bool,
    groups: Vec<BufferedGroup>,
}

struct BufferedGroup {
    arguments: Vec<Vec<Value>>,
    order: Vec<Vec<Value>>,
    seen: HashSet<Vec<u8>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BufferedGroup {
    fn new(arguments: usize, order: usize) -> Self {
        Self {
            arguments: (0..arguments).map(|_| Vec::new()).collect(),
            order: (0..order).map(|_| Vec::new()).collect(),
            seen: HashSet::new(),
        }
    }

    fn len(&self) -> usize {
        self.arguments.first().map_or(0, Vec::len)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Admit only selected adapters that explicitly promise total, effect-free
/// buffered callbacks. All broader shapes fall back before input is consumed.
pub(super) fn try_run(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    if aggregation.sets.len() != 1
        || aggregation.sets[0].indices().len() > 2
        || aggregation
            .groups
            .iter()
            .any(|expression| !expression.is_pure_and_total())
        || aggregation
            .outputs
            .iter()
            .any(|output| !matches!(output, AggregateOutput::Function(_)))
    {
        return Ok(None);
    }
    let functions = aggregation.functions().collect::<Vec<_>>();
    if functions.is_empty()
        || !functions
            .iter()
            .any(|function| function.distinct || !function.order_by.is_empty())
        || functions.iter().any(|function| {
            function.filter.is_some()
                || function
                    .arguments
                    .iter()
                    .any(|argument| !argument.is_pure_and_total())
                || function
                    .order_by
                    .iter()
                    .any(|order| !order.expression.is_pure_and_total())
        })
    {
        return Ok(None);
    }
    let representations = aggregation
        .groups
        .iter()
        .map(|group| {
            context
                .query
                .types()
                .bind(&group.data_type)
                .map(|data_type| data_type.key_representation())
        })
        .collect::<Result<Vec<_>>>()?;
    if representations.iter().any(|key| !key.has_integer_keys()) {
        return Ok(None);
    }

    let mut accumulators = Vec::with_capacity(functions.len());
    for function in &functions {
        let argument_types = function
            .arguments
            .iter()
            .map(|argument| argument.data_type.clone())
            .collect::<Vec<_>>();
        if function.distinct || !function.order_by.is_empty() {
            if argument_types.is_empty()
                || function.function.modifier_strategy(&argument_types)
                    != AggregateModifierStrategy::BufferedTotal
                || function.function.ordered_strategy(&argument_types)
                    != OrderedAggregateStrategy::Buffered
            {
                return Ok(None);
            }
            accumulators.push(Accumulator::Buffered(BufferedAccumulator {
                function: function.function.clone(),
                argument_keys: argument_types
                    .iter()
                    .map(|data_type| context.query.types().bind(data_type))
                    .collect::<Result<Vec<_>>>()?,
                order_types: function
                    .order_by
                    .iter()
                    .map(|order| context.query.types().bind(&order.expression.data_type))
                    .collect::<Result<Vec<_>>>()?,
                order: function.order_by.clone(),
                argument_types,
                distinct: function.distinct,
                groups: Vec::new(),
            }));
        } else {
            let Some(state) = function
                .function
                .create_grouped_state(&argument_types, context.query.types())?
            else {
                return Ok(None);
            };
            if state.group_count() != 0 {
                return Err(Error::Internal("new aggregate state is not empty".into()));
            }
            accumulators.push(Accumulator::Grouped(state));
        }
    }

    aggregation.validate_metadata(context.query)?;
    let group_expressions = aggregation
        .groups
        .iter()
        .map(PreparedExpression::new)
        .collect::<Vec<_>>();
    let argument_expressions = functions
        .iter()
        .map(|function| {
            function
                .arguments
                .iter()
                .map(PreparedExpression::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let order_expressions = functions
        .iter()
        .map(|function| {
            function
                .order_by
                .iter()
                .map(|order| PreparedExpression::new(&order.expression))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let set = &aggregation.sets[0];
    let mut groups = Vec::<Row>::new();
    let mut index = IntegerIndex::default();
    if set.is_empty() {
        context.query.check_rows(1)?;
        index.set_empty(0);
        groups.push(vec![Value::Null; aggregation.groups.len()]);
    }
    let mut retained_units = 0usize;
    let mut distinct_key = Vec::new();
    while let Some(batch) = input.next(context.query.batch_size())? {
        let group_columns = group_expressions
            .iter()
            .map(|expression| expression.evaluate_batch(&batch, context))
            .collect::<Result<Vec<_>>>()?;
        let inputs = argument_expressions
            .iter()
            .map(|expressions| {
                DataChunk::new(
                    expressions
                        .iter()
                        .map(|expression| expression.evaluate_batch(&batch, context))
                        .collect::<Result<_>>()?,
                    batch.len(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let orders = order_expressions
            .iter()
            .map(|expressions| {
                DataChunk::new(
                    expressions
                        .iter()
                        .map(|expression| expression.evaluate_batch(&batch, context))
                        .collect::<Result<_>>()?,
                    batch.len(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let keys = set
            .indices()
            .iter()
            .map(|&ordinal| &group_columns[ordinal])
            .collect::<Vec<_>>();
        let key_representations = set
            .indices()
            .iter()
            .map(|&ordinal| representations[ordinal])
            .collect::<Vec<_>>();
        let destinations = index.locate(
            &keys,
            &key_representations,
            batch.len(),
            context.query,
            |row| {
                context
                    .query
                    .check_rows(groups.len() + retained_units + 1)?;
                let values = group_columns
                    .iter()
                    .enumerate()
                    .map(|(ordinal, column)| {
                        if set.contains(ordinal) {
                            column.get(row).expect("validated grouping column")
                        } else {
                            Value::Null
                        }
                    })
                    .collect();
                let group = groups.len();
                groups.push(values);
                Ok(group)
            },
        )?;
        let needs_selection = accumulators
            .iter()
            .any(|accumulator| matches!(accumulator, Accumulator::Grouped(_)));
        let selection = needs_selection
            .then(|| GroupSelection::new(&destinations, groups.len(), context.query))
            .transpose()?;
        for (function, ((input, order), accumulator)) in functions
            .iter()
            .zip(inputs.iter().zip(&orders).zip(&mut accumulators))
        {
            match accumulator {
                Accumulator::Grouped(state) => {
                    state.resize(groups.len(), context.query)?;
                    state.update_batch(
                        selection
                            .as_ref()
                            .expect("grouped accumulator requires destinations"),
                        input,
                        context.query,
                    )?;
                }
                Accumulator::Buffered(buffered) => {
                    buffered.resize(groups.len());
                    for (row, &group) in destinations.iter().enumerate() {
                        if row % 1024 == 0 {
                            context.query.check()?;
                        }
                        let retained = &mut buffered.groups[group];
                        if buffered.distinct {
                            distinct_key.clear();
                            for ((column, data_type), argument) in input
                                .columns()
                                .iter()
                                .zip(&buffered.argument_keys)
                                .zip(&function.arguments)
                            {
                                with_value(column, row, |value| {
                                    data_type.append_key(value, &mut distinct_key, context.query)
                                })?;
                                debug_assert_eq!(column.data_type(), &argument.data_type);
                            }
                            if retained.seen.contains(&distinct_key) {
                                continue;
                            }
                            context.query.check_rows(retained.seen.len() + 1)?;
                            retained.seen.insert(distinct_key.clone());
                        }
                        let units = 2usize
                            .saturating_add(input.columns().len())
                            .saturating_add(order.columns().len());
                        let next = retained_units.checked_add(units).ok_or_else(|| {
                            Error::Resource("buffered aggregate row count overflow".into())
                        })?;
                        context
                            .query
                            .check_rows(groups.len().saturating_add(next))?;
                        retained_units = next;
                        for (output, column) in retained.arguments.iter_mut().zip(input.columns()) {
                            output.push(
                                column
                                    .get(row)
                                    .expect("validated aggregate argument column"),
                            );
                        }
                        for (output, column) in retained.order.iter_mut().zip(order.columns()) {
                            output.push(column.get(row).expect("validated aggregate order column"));
                        }
                    }
                }
            }
        }
    }

    let mut results = Vec::with_capacity(accumulators.len());
    for accumulator in accumulators {
        match accumulator {
            Accumulator::Grouped(mut state) => {
                state.resize(groups.len(), context.query)?;
                let values = state.finish(context.query)?;
                if values.len() != groups.len() {
                    return Err(Error::Internal(
                        "buffered aggregate result has wrong group count".into(),
                    ));
                }
                results.push(values);
            }
            Accumulator::Buffered(buffered) => {
                results.push(buffered.finish(groups.len(), context)?);
            }
        }
    }
    let rows = groups
        .into_iter()
        .enumerate()
        .map(|(group, mut row)| {
            context.query.check()?;
            row.extend(results.iter().map(|values| values[group].clone()));
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    context.query.check()?;
    Ok(Some(rows))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BufferedAccumulator {
    fn resize(&mut self, groups: usize) {
        self.groups.resize_with(groups, || {
            BufferedGroup::new(self.argument_types.len(), self.order.len())
        });
    }

    fn finish(mut self, groups: usize, context: &ExecutionContext<'_>) -> Result<Vec<Value>> {
        self.resize(groups);
        let mut output = Vec::with_capacity(groups);
        for group in self.groups {
            context.query.check()?;
            let count = group.len();
            let permutation = if self.order.is_empty() {
                (0..count).collect()
            } else {
                stable_permutation(&group.order, &self.order, &self.order_types, context)?
            };
            let columns = group
                .arguments
                .into_iter()
                .zip(&self.argument_types)
                .map(|(values, data_type)| {
                    let values = if self.order.is_empty() {
                        values
                    } else {
                        permutation
                            .iter()
                            .map(|&index| values[index].clone())
                            .collect()
                    };
                    Vector::flat(data_type.clone(), values)
                })
                .collect::<Result<Vec<_>>>()?;
            let mut state = self
                .function
                .create_state(&self.argument_types, context.query.types())?;
            state.update_batch(&DataChunk::new(columns, count)?, context.query)?;
            output.push(state.finish()?);
        }
        Ok(output)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn stable_permutation(
    columns: &[Vec<Value>],
    order: &[OrderExpr],
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Vec<usize>> {
    let rows = columns.first().map_or(0, Vec::len);
    if columns.is_empty() {
        return Ok(Vec::new());
    }
    let mut permutation = (0..rows).collect::<Vec<_>>();
    let mut scratch = vec![0; rows];
    let mut width = 1usize;
    while width < rows {
        for start in (0..rows).step_by(width.saturating_mul(2)) {
            context.query.check()?;
            let middle = start.saturating_add(width).min(rows);
            let end = middle.saturating_add(width).min(rows);
            let (mut left, mut right) = (start, middle);
            for output in &mut scratch[start..end] {
                let take_left = left < middle
                    && (right == end
                        || compare_order(
                            columns,
                            permutation[left],
                            permutation[right],
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
    Ok(permutation)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare_order(
    columns: &[Vec<Value>],
    left: usize,
    right: usize,
    order: &[OrderExpr],
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Ordering> {
    for ((column, order), data_type) in columns.iter().zip(order).zip(types) {
        let (left, right) = (&column[left], &column[right]);
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn with_value<T>(
    column: &Vector,
    row: usize,
    callback: impl FnOnce(&Value) -> Result<T>,
) -> Result<T> {
    if let Some(values) = column.flat_values() {
        return callback(
            values
                .get(row)
                .ok_or_else(|| Error::Internal("aggregate vector row outside input".into()))?,
        );
    }
    if let Some(value) = column.constant_value() {
        if row >= column.len() {
            return Err(Error::Internal("aggregate vector row outside input".into()));
        }
        return callback(value);
    }
    if let Some((parent, selection)) = column.dictionary() {
        let selected = *selection
            .get(row)
            .ok_or_else(|| Error::Internal("aggregate vector row outside input".into()))?;
        return with_value(parent, selected, callback);
    }
    let value = column
        .get(row)
        .ok_or_else(|| Error::Internal("aggregate vector row outside input".into()))?;
    callback(&value)
}
