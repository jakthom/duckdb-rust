//! Column grouping for total expressions and opt-in aggregate states.
mod index;
use super::*;
use crate::{
    Value, common::type_registry::KeyRepresentation, execution::subquery::PreparedExpression,
    function::grouped::GroupSelection, planner::aggregation::AggregateOutput,
};

/// None is returned only before consuming input. Unknown expression effects,
/// DISTINCT/FILTER, broader keys and functions retain the ordered row driver.
pub(super) fn try_run(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    // Keep the existing ungrouped column kernel, including its overflow proof.
    if aggregation.groups.is_empty() && aggregation.sets.len() == 1 {
        return Ok(None);
    }
    if aggregation.sets.iter().any(|s| s.indices().len() > 2)
        || aggregation.groups.iter().any(|e| !e.is_pure_and_total())
        || aggregation.functions().any(|f| {
            f.distinct || f.filter.is_some() || f.arguments.iter().any(|e| !e.is_pure_and_total())
        })
    {
        return Ok(None);
    }
    for group in &aggregation.groups {
        let data_type = context.query.types().bind(&group.data_type)?;
        if data_type.key_representation() != KeyRepresentation::Integer {
            return Ok(None);
        }
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
            let destinations =
                indices[set_index].locate(&keys, batch.len(), context.query, |row| {
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
                })?;
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
