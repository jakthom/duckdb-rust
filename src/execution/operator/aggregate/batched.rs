//! Column grouping for total expressions and opt-in aggregate states.
mod index;
use super::*;
use crate::{
    Value,
    common::type_registry::BoundType,
    execution::subquery::PreparedExpression,
    function::{
        OrderedAggregateStrategy,
        grouped::{GroupSelection, GroupedAggregateState},
    },
    planner::aggregation::AggregateOutput,
};
use std::cmp::Ordering;

enum OrderedAccumulator {
    State(Box<dyn GroupedAggregateState>),
    Candidate {
        strategy: OrderedAggregateStrategy,
        order: Vec<crate::planner::logical::OrderExpr>,
        types: Vec<BoundType>,
        values: Vec<Option<(Value, Vec<Value>)>>,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// None is returned only before consuming input. Unknown expression effects,
/// DISTINCT/FILTER, broader keys and functions retain the ordered row driver.
pub(super) fn try_run(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    if let Some(rows) = try_ordered_candidates(input, aggregation, context)? {
        return Ok(Some(rows));
    }
    if let Some(rows) = try_ungrouped(input, aggregation, context)? {
        return Ok(Some(rows));
    }
    // Keep the existing ungrouped column kernel, including its overflow proof.
    if aggregation.groups.is_empty() && aggregation.sets.len() == 1 {
        return Ok(None);
    }
    if aggregation.sets.iter().any(|s| s.indices().len() > 2)
        || aggregation.groups.iter().any(|e| !e.is_pure_and_total())
        || aggregation.functions().any(|f| {
            f.distinct
                || f.filter.is_some()
                || !f.order_by.is_empty()
                || f.arguments.iter().any(|e| !e.is_pure_and_total())
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
    aggregation.validate_metadata(context.query)?;
    let functions = aggregation.functions().collect::<Vec<_>>();
    let mut accumulators = Vec::with_capacity(functions.len());
    for function in &functions {
        let arguments = function
            .arguments
            .iter()
            .map(|e| e.data_type.clone())
            .collect::<Vec<_>>();
        let Some(state) = function
            .function
            .create_grouped_state(&arguments, context.query.types())?
        else {
            return Ok(None);
        };
        if state.group_count() != 0 {
            return Err(Error::Internal("new aggregate state is not empty".into()));
        }
        accumulators.push(state);
    }
    let expressions = aggregation
        .groups
        .iter()
        .map(PreparedExpression::new)
        .collect::<Vec<_>>();
    let arguments = functions
        .iter()
        .map(|f| {
            f.arguments
                .iter()
                .map(PreparedExpression::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut groups: Vec<(Row, usize)> = Vec::new();
    let mut indices = aggregation
        .sets
        .iter()
        .enumerate()
        .map(|(set_index, set)| {
            let mut index = index::IntegerIndex::default();
            if set.is_empty() {
                context.query.check_rows(groups.len() + 1)?;
                index.set_empty(groups.len());
                groups.push((vec![Value::Null; aggregation.groups.len()], set_index));
            }
            Ok(index)
        })
        .collect::<Result<Vec<_>>>()?;
    while let Some(batch) = input.next(context.query.batch_size())? {
        let columns = expressions
            .iter()
            .map(|e| e.evaluate_batch(&batch, context))
            .collect::<Result<Vec<_>>>()?;
        let inputs = arguments
            .iter()
            .map(|args| {
                DataChunk::new(
                    args.iter()
                        .map(|e| e.evaluate_batch(&batch, context))
                        .collect::<Result<_>>()?,
                    batch.len(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        for (set_index, set) in aggregation.sets.iter().enumerate() {
            let keys = set
                .indices()
                .iter()
                .map(|&index| &columns[index])
                .collect::<Vec<_>>();
            let key_representations = set
                .indices()
                .iter()
                .map(|&index| representations[index])
                .collect::<Vec<_>>();
            let destinations = indices[set_index].locate(
                &keys,
                &key_representations,
                batch.len(),
                context.query,
                |row| {
                    context.query.check_rows(groups.len() + 1)?;
                    let index = groups.len();
                    let values = columns
                        .iter()
                        .enumerate()
                        .map(|(i, values)| {
                            if set.contains(i) {
                                values.get(row).expect("validated grouping column").clone()
                            } else {
                                Value::Null
                            }
                        })
                        .collect();
                    groups.push((values, set_index));
                    Ok(index)
                },
            )?;
            let destinations = GroupSelection::new(&destinations, groups.len(), context.query)?;
            for (state, arguments) in accumulators.iter_mut().zip(&inputs) {
                state.resize(groups.len(), context.query)?;
                if state.group_count() != groups.len() {
                    return Err(Error::Internal(
                        "aggregate resize returned wrong group count".into(),
                    ));
                }
                state.update_batch(&destinations, arguments, context.query)?;
            }
        }
    }
    let values = accumulators
        .into_iter()
        .map(|mut state| {
            state.resize(groups.len(), context.query)?;
            if state.group_count() != groups.len() {
                return Err(Error::Internal(
                    "aggregate resize returned wrong group count".into(),
                ));
            }
            let values = state.finish(context.query)?;
            if values.len() != groups.len() {
                return Err(Error::Internal(
                    "aggregate result has wrong group count".into(),
                ));
            }
            Ok(values)
        })
        .collect::<Result<Vec<_>>>()?;
    let rows = groups
        .into_iter()
        .enumerate()
        .map(|(group_index, (mut row, set_index))| {
            context.query.check()?;
            let mut function = 0;
            for output in &aggregation.outputs {
                row.push(match output {
                    AggregateOutput::Function(_) => {
                        let value = values[function][group_index].clone();
                        function += 1;
                        value
                    }
                    AggregateOutput::Grouping(indices) => {
                        Value::Integer(indices.iter().fold(0, |mask, &index| {
                            (mask << 1) | i128::from(!aggregation.sets[set_index].contains(index))
                        }))
                    }
                });
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(rows))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Batched, bounded retention for FIRST/LAST argument ORDER BY. This route is
/// intentionally narrower than the scalar ordered driver: all expressions
/// must be pure and total, arguments are unary, and only integer
/// grouping keys accepted by IntegerIndex may enter. Everything observable or
/// broader returns None before input is consumed and keeps the generic path.
fn try_ordered_candidates(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    if aggregation.sets.iter().any(|set| set.indices().len() > 2)
        || aggregation
            .groups
            .iter()
            .any(|group| !group.is_pure_and_total())
        || aggregation
            .outputs
            .iter()
            .any(|output| !matches!(output, AggregateOutput::Function(_)))
    {
        return Ok(None);
    }
    let functions = aggregation.functions().collect::<Vec<_>>();
    if !functions
        .iter()
        .any(|function| !function.order_by.is_empty())
        || functions.iter().any(|function| {
            let strategy = function.function.ordered_strategy(
                &function
                    .arguments
                    .iter()
                    .map(|arg| arg.data_type.clone())
                    .collect::<Vec<_>>(),
            );
            let candidate = matches!(
                strategy,
                OrderedAggregateStrategy::First | OrderedAggregateStrategy::Last
            ) && !function.distinct
                && function.filter.is_none()
                && function.arguments.len() == 1
                && !function.order_by.is_empty()
                && function.arguments.iter().all(BoundExpr::is_pure_and_total)
                && function
                    .order_by
                    .iter()
                    .all(|order| order.expression.is_pure_and_total());
            !candidate
                && (function.distinct
                    || function.filter.is_some()
                    || !function.order_by.is_empty()
                    || function
                        .arguments
                        .iter()
                        .any(|arg| !arg.is_pure_and_total()))
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
                .map(|ty| ty.key_representation())
        })
        .collect::<Result<Vec<_>>>()?;
    if representations.iter().any(|key| !key.has_integer_keys()) {
        return Ok(None);
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
    let mut accumulators = Vec::with_capacity(functions.len());
    for function in &functions {
        let arguments = function
            .arguments
            .iter()
            .map(|arg| arg.data_type.clone())
            .collect::<Vec<_>>();
        match function.function.ordered_strategy(&arguments) {
            OrderedAggregateStrategy::First | OrderedAggregateStrategy::Last
                if !function.distinct
                    && function.filter.is_none()
                    && function.arguments.len() == 1
                    && !function.order_by.is_empty() =>
            {
                accumulators.push(OrderedAccumulator::Candidate {
                    strategy: function.function.ordered_strategy(&arguments),
                    order: function.order_by.clone(),
                    types: function
                        .order_by
                        .iter()
                        .map(|order| context.query.types().bind(&order.expression.data_type))
                        .collect::<Result<Vec<_>>>()?,
                    values: Vec::new(),
                });
            }
            _ => {
                let Some(state) = function
                    .function
                    .create_grouped_state(&arguments, context.query.types())?
                else {
                    return Ok(None);
                };
                accumulators.push(OrderedAccumulator::State(state));
            }
        }
    }
    let mut groups: Vec<(Row, usize)> = Vec::new();
    let mut indices = aggregation
        .sets
        .iter()
        .enumerate()
        .map(|(set_index, set)| {
            let mut index = index::IntegerIndex::default();
            if set.is_empty() {
                context.query.check_rows(groups.len() + 1)?;
                index.set_empty(groups.len());
                groups.push((vec![Value::Null; aggregation.groups.len()], set_index));
            }
            Ok(index)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut candidate_units = 0usize;
    while let Some(batch) = input.next(context.query.batch_size())? {
        let columns = group_expressions
            .iter()
            .map(|expr| expr.evaluate_batch(&batch, context))
            .collect::<Result<Vec<_>>>()?;
        let inputs = argument_expressions
            .iter()
            .map(|expressions| {
                DataChunk::new(
                    expressions
                        .iter()
                        .map(|expr| expr.evaluate_batch(&batch, context))
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
                        .map(|expr| expr.evaluate_batch(&batch, context))
                        .collect::<Result<_>>()?,
                    batch.len(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        for (set_index, set) in aggregation.sets.iter().enumerate() {
            let keys = set
                .indices()
                .iter()
                .map(|&index| &columns[index])
                .collect::<Vec<_>>();
            let key_representations = set
                .indices()
                .iter()
                .map(|&index| representations[index])
                .collect::<Vec<_>>();
            let destinations = indices[set_index].locate(
                &keys,
                &key_representations,
                batch.len(),
                context.query,
                |row| {
                    context
                        .query
                        .check_rows(groups.len() + 1 + candidate_units)?;
                    let index = groups.len();
                    groups.push((
                        columns
                            .iter()
                            .enumerate()
                            .map(|(i, values)| {
                                if set.contains(i) {
                                    values.get(row).expect("validated grouping column").clone()
                                } else {
                                    Value::Null
                                }
                            })
                            .collect(),
                        set_index,
                    ));
                    Ok(index)
                },
            )?;
            let destinations = GroupSelection::new(&destinations, groups.len(), context.query)?;
            for (function_index, accumulator) in accumulators.iter_mut().enumerate() {
                match accumulator {
                    OrderedAccumulator::State(state) => {
                        state.resize(groups.len(), context.query)?;
                        state.update_batch(
                            &destinations,
                            &inputs[function_index],
                            context.query,
                        )?;
                    }
                    OrderedAccumulator::Candidate {
                        strategy,
                        order,
                        types,
                        values,
                    } => {
                        values.resize_with(groups.len(), || None);
                        for (row, &group) in destinations.indices().iter().enumerate() {
                            if row % 1024 == 0 {
                                context.query.check()?;
                            }
                            let replaces = match &values[group] {
                                None => true,
                                Some((_, current)) => match compare_candidate_order(
                                    &orders[function_index],
                                    row,
                                    current,
                                    order,
                                    types,
                                    context,
                                )? {
                                    Ordering::Less => *strategy == OrderedAggregateStrategy::First,
                                    Ordering::Greater => {
                                        *strategy == OrderedAggregateStrategy::Last
                                    }
                                    Ordering::Equal => false,
                                },
                            };
                            if replaces {
                                if values[group].is_none() {
                                    context.query.check_rows(
                                        groups
                                            .len()
                                            .saturating_add(candidate_units)
                                            .saturating_add(1),
                                    )?;
                                    candidate_units += 1;
                                }
                                let argument = inputs[function_index].columns()[0]
                                    .value(row)
                                    .expect("validated candidate argument");
                                let key = orders[function_index]
                                    .columns()
                                    .iter()
                                    .map(|column| {
                                        column.value(row).expect("validated candidate order")
                                    })
                                    .collect();
                                values[group] = Some((argument, key));
                            }
                        }
                    }
                }
            }
        }
    }
    let values = accumulators
        .into_iter()
        .zip(&functions)
        .map(|(accumulator, function)| match accumulator {
            OrderedAccumulator::State(mut state) => {
                state.resize(groups.len(), context.query)?;
                state.finish(context.query)
            }
            OrderedAccumulator::Candidate {
                strategy, values, ..
            } => {
                // The candidate capability is admitted only for built-in
                // FIRST/LAST with one already bound argument. Their state is
                // precisely the retained argument, or NULL for an empty
                // group; constructing thousands of one-row states here would
                // add no semantics and dominates high-cardinality groups.
                debug_assert!(matches!(
                    strategy,
                    OrderedAggregateStrategy::First | OrderedAggregateStrategy::Last
                ));
                Ok(values
                    .into_iter()
                    .map(|candidate| candidate.map_or(Value::Null, |(argument, _)| argument))
                    .collect())
            }
        })
        .collect::<Result<Vec<_>>>()?;
    groups
        .into_iter()
        .enumerate()
        .map(|(group, (mut row, _))| {
            context.query.check()?;
            for function in 0..functions.len() {
                row.push(values[function][group].clone());
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Compare one vector row with a retained candidate without constructing a
/// temporary Row. The vector seam yields owned scalar values for encoded
/// lanes; candidate allocation occurs only after a strict replacement.
fn compare_candidate_order(
    columns: &DataChunk,
    row: usize,
    current: &[Value],
    order: &[crate::planner::logical::OrderExpr],
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Ordering> {
    for (((column, current), order), data_type) in
        columns.columns().iter().zip(current).zip(order).zip(types)
    {
        let value = column.value(row).expect("validated candidate order");
        let comparison = match (value.is_null(), current.is_null()) {
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
                let comparison = data_type.compare(&value, current, context.query)?;
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
/// Evaluate effect-free aggregate arguments as columns, then retain aggregate
/// updates in exact row/output order. If speculative column evaluation finds a
/// data error, replay only that untouched batch through the scalar expression
/// order so the reported first error remains source-compatible.
fn try_ungrouped(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    if !aggregation.groups.is_empty()
        || aggregation.sets.len() != 1
        || !aggregation.sets[0].is_empty()
        || aggregation
            .outputs
            .iter()
            .any(|output| !matches!(output, AggregateOutput::Function(_)))
    {
        return Ok(None);
    }
    let functions = aggregation.functions().collect::<Vec<_>>();
    if functions.iter().any(|function| {
        function.distinct
            || function.filter.is_some()
            || !function.order_by.is_empty()
            || function
                .arguments
                .iter()
                .any(|argument| !argument.is_effect_free())
    }) {
        return Ok(None);
    }
    aggregation.validate_metadata(context.query)?;
    let expressions = functions
        .iter()
        .map(|function| {
            function
                .arguments
                .iter()
                .map(PreparedExpression::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let batch_updates_are_total = functions.iter().all(|function| {
        function.function.batch_update_is_total(
            &function
                .arguments
                .iter()
                .map(|argument| argument.data_type.clone())
                .collect::<Vec<_>>(),
        )
    });
    let mut states = functions
        .iter()
        .map(|function| {
            function.function.create_state(
                &function
                    .arguments
                    .iter()
                    .map(|argument| argument.data_type.clone())
                    .collect::<Vec<_>>(),
                context.query.types(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let mut argument_rows = expressions
        .iter()
        .map(|arguments| Vec::with_capacity(arguments.len()))
        .collect::<Vec<Row>>();
    while let Some(batch) = input.next(context.query.batch_size())? {
        let evaluated = expressions
            .iter()
            .map(|arguments| {
                DataChunk::new(
                    arguments
                        .iter()
                        .map(|argument| argument.evaluate_batch(&batch, context))
                        .collect::<Result<_>>()?,
                    batch.len(),
                )
            })
            .collect::<Result<Vec<_>>>();
        let evaluated = match evaluated {
            Ok(evaluated) => evaluated,
            Err(error) if data_error(&error) => {
                update_scalar_batch(
                    &batch,
                    &expressions,
                    &mut states,
                    &mut argument_rows,
                    context,
                )?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if batch_updates_are_total {
            for (arguments, state) in evaluated.iter().zip(states.iter_mut()) {
                state.update_batch(arguments, context.query)?;
            }
        } else {
            for row in 0..batch.len() {
                if row % 1024 == 0 {
                    context.query.check()?;
                }
                for ((arguments, state), values) in evaluated
                    .iter()
                    .zip(states.iter_mut())
                    .zip(argument_rows.iter_mut())
                {
                    arguments.read_row(row, values)?;
                    state.update(values, context.query)?;
                }
            }
        }
    }
    states
        .into_iter()
        .map(|state| state.finish())
        .collect::<Result<Row>>()
        .map(|row| Some(vec![row]))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn update_scalar_batch(
    batch: &DataChunk,
    expressions: &[Vec<PreparedExpression<'_>>],
    states: &mut [Box<dyn crate::function::AggregateState>],
    argument_rows: &mut [Row],
    context: &ExecutionContext<'_>,
) -> Result<()> {
    for row in batch.rows() {
        context.query.check()?;
        for ((arguments, state), values) in expressions
            .iter()
            .zip(states.iter_mut())
            .zip(argument_rows.iter_mut())
        {
            values.clear();
            for argument in arguments {
                values.push(argument.evaluate(&row, context)?);
            }
            state.update(values, context.query)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn data_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Conversion(_)
            | Error::Execution(_)
            | Error::OutOfRange(_)
            | Error::InvalidInput(_)
            | Error::InvalidType(_)
    )
}
