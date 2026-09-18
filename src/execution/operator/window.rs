//! Window preparation retains batches and explicit row identities.
use super::order::{ComparisonSort, RadixSort, SortAlgorithm};
use crate::{
    common::{
        DataType, Error, Result, RowCollection, Value,
        type_registry::{BoundType, OrderingRepresentation},
        vector::{DataChunk, Vector},
    },
    execution::{
        ExecutionContext,
        physical_plan::{DeliveryMode, PhysicalOperator},
        stream::{self, BatchStream, Stream},
        subquery::PreparedExpression,
    },
    function::window::{WindowBounds, WindowInput, WindowRows},
    planner::{
        BoundExpr, Field, Schema,
        logical::OrderExpr,
        window::{FrameBound, FrameUnits, WindowExpression},
    },
};
use std::{collections::HashMap, fmt::Debug, ops::Range, sync::Arc};

mod evaluate;
mod partition;
mod prepare;
use evaluate::evaluate;
use partition::{equality_key, integer_partitions, sort_indices};
use prepare::PreparedWindow;

pub struct WindowPlan<'a> {
    pub input: &'a dyn PhysicalOperator,
    pub expressions: &'a [WindowExpression],
    pub schema: &'a Schema,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Append one result column per window, retaining input row order. Each call
/// owns its partitions and function state. Arguments and keys are evaluated
/// once per input row per window; effects preserve row order. Typed equality
/// defines partitions/peers. Functions and sort permutations are validated.
/// Errors return no partial blocking output. Retained input obeys the row
/// budget; byte accounting and spill remain separate resource capabilities.
pub trait WindowAlgorithm: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn delivery(&self, _windows: &[WindowExpression]) -> DeliveryMode {
        DeliveryMode::Blocking
    }
    fn open<'a>(
        &'a self,
        plan: WindowPlan<'a>,
        context: &'a ExecutionContext<'a>,
    ) -> Result<Stream<'a>> {
        Ok(blocking(self, plan, context))
    }
    fn evaluate(
        &self,
        input: &mut dyn BatchStream,
        schema: &Schema,
        windows: &[WindowExpression],
        context: &ExecutionContext<'_>,
    ) -> Result<DataChunk>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn blocking<'a, A: WindowAlgorithm + ?Sized>(
    algorithm: &'a A,
    plan: WindowPlan<'a>,
    context: &'a ExecutionContext<'a>,
) -> Stream<'a> {
    let mut output = None;
    let mut position = 0usize;
    stream::from_fn(move |max_rows| {
        context.query.check()?;
        if output.is_none() {
            let mut input = stream::open(plan.input, context)?;
            let result = algorithm.evaluate(
                input.as_mut(),
                plan.input.schema(),
                plan.expressions,
                context,
            )?;
            if result.columns().len() != plan.schema.len()
                || !result
                    .columns()
                    .iter()
                    .zip(plan.schema)
                    .all(|(column, field)| column.data_type() == &field.data_type)
            {
                return Err(Error::Internal(
                    "window algorithm output schema differs".into(),
                ));
            }
            output = Some(result);
        }
        context.query.check()?;
        let output = output.as_ref().expect("initialized window output");
        if position == output.len() {
            return Ok(None);
        }
        let count = max_rows.min(output.len() - position);
        let batch = output.slice(position, count)?;
        position += count;
        Ok(Some(batch))
    })
}

#[derive(Debug)]
pub struct PartitionedWindows {
    sorting: Arc<dyn SortAlgorithm>,
}
#[derive(Debug)]
pub struct SortedWindows {
    sorting: Arc<dyn SortAlgorithm>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for PartitionedWindows {
    fn default() -> Self {
        Self::new(Arc::new(RadixSort))
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for SortedWindows {
    fn default() -> Self {
        Self::new(Arc::new(ComparisonSort))
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PartitionedWindows {
    pub fn new(sorting: Arc<dyn SortAlgorithm>) -> Self {
        Self { sorting }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SortedWindows {
    pub fn new(sorting: Arc<dyn SortAlgorithm>) -> Self {
        Self { sorting }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn can_stream(windows: &[WindowExpression]) -> bool {
    windows.iter().all(|window| {
        window.partition.is_empty()
            && window.order.is_empty()
            && !window.options.distinct
            && window.options.null_treatment.is_none()
            && window.filter.is_none()
            && window.arguments.iter().all(BoundExpr::is_pure_and_total)
            && window.function.supports_streaming()
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowAlgorithm for PartitionedWindows {
    fn name(&self) -> &'static str {
        "hash-partitioned-windows"
    }
    fn delivery(&self, windows: &[WindowExpression]) -> DeliveryMode {
        if can_stream(windows) {
            DeliveryMode::Incremental
        } else {
            DeliveryMode::Blocking
        }
    }
    fn open<'a>(
        &'a self,
        plan: WindowPlan<'a>,
        context: &'a ExecutionContext<'a>,
    ) -> Result<Stream<'a>> {
        if !can_stream(plan.expressions) {
            return Ok(blocking(self, plan, context));
        }
        let mut states = plan
            .expressions
            .iter()
            .map(|window| window.function.start_stream())
            .collect::<Result<Vec<_>>>()?;
        let mut input = stream::open(plan.input, context)?;
        Ok(stream::from_fn(move |max_rows| {
            let Some(batch) = input.next(max_rows)? else {
                return Ok(None);
            };
            let mut columns = batch.columns().to_vec();
            for (state, window) in states.iter_mut().zip(plan.expressions) {
                let arguments = window
                    .arguments
                    .iter()
                    .map(|expr| PreparedExpression::new(expr).evaluate_batch(&batch, context))
                    .collect::<Result<_>>()?;
                let output =
                    state.evaluate(&DataChunk::new(arguments, batch.len())?, context.query)?;
                if output.len() != batch.len() || output.data_type() != &window.data_type {
                    return Err(Error::Internal(
                        "streaming window output shape differs".into(),
                    ));
                }
                context
                    .query
                    .types()
                    .bind(&window.data_type)?
                    .validate_vector(&output, context.query)?;
                columns.push(output);
            }
            DataChunk::new(columns, batch.len()).map(Some)
        }))
    }
    fn evaluate(
        &self,
        input: &mut dyn BatchStream,
        schema: &Schema,
        windows: &[WindowExpression],
        context: &ExecutionContext<'_>,
    ) -> Result<DataChunk> {
        evaluate(input, schema, windows, context, &|data, window, context| {
            let mut partitions: Vec<Vec<usize>> = Vec::new();
            if data.partition_types.is_empty() {
                partitions.push((0..data.arguments.len()).collect());
            } else if let Some(dense) =
                integer_partitions(&data.partition, &data.partition_types, context)?
            {
                partitions = dense;
            } else {
                let mut buckets = HashMap::new();
                for (index, row) in data.partition.iter().enumerate() {
                    if index % 1024 == 0 {
                        context.query.check()?;
                    }
                    let key = equality_key(row, &data.partition_types, context)?;
                    let next = partitions.len();
                    let bucket = *buckets.entry(key).or_insert(next);
                    if bucket == next {
                        partitions.push(Vec::new());
                    }
                    partitions[bucket].push(index);
                }
            }
            for partition in &mut partitions {
                *partition = sort_indices(
                    partition,
                    &data.order,
                    &data.order_types,
                    &window.order,
                    self.sorting.as_ref(),
                    context,
                )?;
            }
            Ok(partitions)
        })
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowAlgorithm for SortedWindows {
    fn name(&self) -> &'static str {
        "sorted-partition-windows"
    }
    fn evaluate(
        &self,
        input: &mut dyn BatchStream,
        schema: &Schema,
        windows: &[WindowExpression],
        context: &ExecutionContext<'_>,
    ) -> Result<DataChunk> {
        evaluate(input, schema, windows, context, &|data, window, context| {
            let mut combined = RowCollection::new(data.partition.width() + data.order.width());
            for (left, right) in data.partition.iter().zip(&data.order) {
                combined.push(&left.iter().chain(right).cloned().collect::<Vec<_>>())?;
            }
            let types = data
                .partition_types
                .iter()
                .chain(&data.order_types)
                .cloned()
                .collect::<Vec<_>>();
            let mut order = data
                .partition_types
                .iter()
                .enumerate()
                .map(|(index, data_type)| OrderExpr {
                    expression: BoundExpr::column(index, data_type.data_type().clone()),
                    descending: false,
                    nulls_first: false,
                })
                .collect::<Vec<_>>();
            order.extend(
                window
                    .order
                    .iter()
                    .enumerate()
                    .map(|(index, key)| OrderExpr {
                        expression: BoundExpr::column(
                            index + data.partition_types.len(),
                            data.order_types[index].data_type().clone(),
                        ),
                        ..key.clone()
                    }),
            );
            let indices = sort_indices(
                &(0..combined.len()).collect::<Vec<_>>(),
                &combined,
                &types,
                &order,
                self.sorting.as_ref(),
                context,
            )?;
            let mut partitions: Vec<Vec<usize>> = Vec::new();
            let mut previous = None;
            for index in indices {
                let key = equality_key(&data.partition[index], &data.partition_types, context)?;
                if previous.as_ref() != Some(&key) {
                    partitions.push(Vec::new());
                    previous = Some(key);
                }
                partitions
                    .last_mut()
                    .expect("partition established")
                    .push(index);
            }
            Ok(partitions)
        })
    }
}
