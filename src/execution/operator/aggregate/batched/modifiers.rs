//! Generic column evaluation for total buffered aggregate modifiers.
//!
//! The executor retains owned values by group and column. Ordered groups build
//! a stable row permutation. Each ordered argument moves into one flat parent
//! and uses that checked permutation as a dictionary selection, so the driver
//! does not clone every payload again before the selected aggregate state's
//! batch callback. This deliberately favors a bounded, capability-checked
//! implementation over a shallow-chunk representation; measurements decide
//! whether a later storage specialization is warranted.
use super::index::IntegerIndex;
use crate::{
    DataType, Value,
    common::{
        Error, Result, Row,
        type_registry::{BoundType, KeyRepresentation, OrderingRepresentation},
        vector::{DataChunk, Vector},
    },
    execution::{ExecutionContext, stream::BatchStream, subquery::PreparedExpression},
    function::{
        AggregateFunction, AggregateModifierStrategy, OrderedAggregateStrategy,
        grouped::{GroupSelection, GroupedAggregateState},
    },
    parallel::QueryContext,
    planner::{
        aggregation::{AggregateOutput, Aggregation},
        logical::OrderExpr,
    },
};
use std::{cmp::Ordering, collections::HashSet, sync::Arc};

const SMALL_VARCHAR_DISTINCT_KEYS: usize = 64;
const MAX_SIGNED_ORDER_BUCKETS: usize = 65_536;

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
    seen: DistinctKeys,
    seen_null: bool,
}

enum DistinctKeys {
    Canonical(HashSet<Vec<u8>>),
    SmallVarchar(Vec<SmallVarcharKey>),
    VarcharHash(HashSet<Vec<u8>>),
}

struct SmallVarcharKey {
    prefix: u64,
    bytes: Vec<u8>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BufferedGroup {
    fn new(arguments: usize, order: usize, borrowed_varchar_key: bool) -> Self {
        Self {
            arguments: (0..arguments).map(|_| Vec::new()).collect(),
            order: (0..order).map(|_| Vec::new()).collect(),
            seen: if borrowed_varchar_key {
                DistinctKeys::SmallVarchar(Vec::new())
            } else {
                DistinctKeys::Canonical(HashSet::new())
            },
            seen_null: false,
        }
    }

    fn len(&self) -> usize {
        self.arguments.first().map_or(0, Vec::len)
    }

    fn distinct_count(&self) -> Result<usize> {
        let non_null = match &self.seen {
            DistinctKeys::Canonical(keys) | DistinctKeys::VarcharHash(keys) => keys.len(),
            DistinctKeys::SmallVarchar(keys) => keys.len(),
        };
        non_null
            .checked_add(usize::from(self.seen_null))
            .ok_or_else(|| Error::Resource("buffered DISTINCT key count overflow".into()))
    }

    fn insert_varchar_key(&mut self, bytes: &[u8], query: &QueryContext) -> Result<bool> {
        let null_count = usize::from(self.seen_null);
        let DistinctKeys::SmallVarchar(keys) = &mut self.seen else {
            let DistinctKeys::VarcharHash(keys) = &mut self.seen else {
                return Err(Error::Internal(
                    "borrowed VARCHAR key used with canonical DISTINCT state".into(),
                ));
            };
            if keys.contains(bytes) {
                return Ok(false);
            }
            let next = keys
                .len()
                .checked_add(null_count)
                .and_then(|count| count.checked_add(1))
                .ok_or_else(|| Error::Resource("buffered DISTINCT key count overflow".into()))?;
            query.check_rows(next)?;
            let owned = copy_distinct_key(bytes)?;
            keys.try_reserve(1)
                .map_err(|_| Error::Resource("buffered DISTINCT key allocation failed".into()))?;
            keys.insert(owned);
            return Ok(true);
        };
        let prefix = varchar_key_prefix(bytes);
        let insertion = match keys.binary_search_by(|key| {
            key.prefix.cmp(&prefix).then_with(|| {
                if bytes.len() <= 7 {
                    Ordering::Equal
                } else {
                    key.bytes.as_slice().cmp(bytes)
                }
            })
        }) {
            Ok(_) => return Ok(false),
            Err(insertion) => insertion,
        };
        let next = keys
            .len()
            .checked_add(null_count)
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| Error::Resource("buffered DISTINCT key count overflow".into()))?;
        query.check_rows(next)?;
        let owned = copy_distinct_key(bytes)?;
        if keys.len() < SMALL_VARCHAR_DISTINCT_KEYS {
            keys.try_reserve(1)
                .map_err(|_| Error::Resource("buffered DISTINCT key allocation failed".into()))?;
            keys.insert(
                insertion,
                SmallVarcharKey {
                    prefix,
                    bytes: owned,
                },
            );
            return Ok(true);
        }

        let mut hashed = HashSet::new();
        let capacity = keys
            .len()
            .checked_add(1)
            .ok_or_else(|| Error::Resource("buffered DISTINCT key count overflow".into()))?;
        hashed
            .try_reserve(capacity)
            .map_err(|_| Error::Resource("buffered DISTINCT key allocation failed".into()))?;
        for key in std::mem::take(keys) {
            hashed.insert(key.bytes);
        }
        hashed.insert(owned);
        self.seen = DistinctKeys::VarcharHash(hashed);
        Ok(true)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn copy_distinct_key(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|_| Error::Resource("buffered DISTINCT key allocation failed".into()))?;
    owned.extend_from_slice(bytes);
    Ok(owned)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn varchar_key_prefix(bytes: &[u8]) -> u64 {
    let mut prefix = if bytes.len() <= 7 {
        (bytes.len() as u64) << 56
    } else {
        u64::MAX << 56
    };
    for (position, byte) in bytes.iter().take(7).enumerate() {
        prefix |= u64::from(*byte) << (48 - position * 8);
    }
    prefix
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
                    let borrowed_varchar_key = buffered.distinct
                        && matches!(
                            buffered.argument_keys.as_slice(),
                            [data_type]
                                if data_type.key_representation()
                                    == KeyRepresentation::VarcharBytes
                        );
                    if borrowed_varchar_key {
                        let [column] = input.columns() else {
                            return Err(Error::Internal(
                                "borrowed VARCHAR DISTINCT requires one argument".into(),
                            ));
                        };
                        buffered.argument_keys[0].validate_vector(column, context.query)?;
                    }
                    for (row, &group) in destinations.iter().enumerate() {
                        if row % 1024 == 0 {
                            context.query.check()?;
                        }
                        let retained = &mut buffered.groups[group];
                        if buffered.distinct {
                            if borrowed_varchar_key {
                                let representation = buffered.argument_keys[0].key_representation();
                                let inserted = with_value(&input.columns()[0], row, |value| {
                                    match representation.varchar_key(value)? {
                                        None if retained.seen_null => Ok(false),
                                        None => {
                                            let next = retained
                                                .distinct_count()?
                                                .checked_add(1)
                                                .ok_or_else(|| {
                                                    Error::Resource(
                                                        "buffered DISTINCT key count overflow"
                                                            .into(),
                                                    )
                                                })?;
                                            context.query.check_rows(next)?;
                                            retained.seen_null = true;
                                            Ok(true)
                                        }
                                        Some(bytes) => {
                                            retained.insert_varchar_key(bytes, context.query)
                                        }
                                    }
                                })?;
                                if !inserted {
                                    continue;
                                }
                            } else {
                                distinct_key.clear();
                                for ((column, data_type), argument) in input
                                    .columns()
                                    .iter()
                                    .zip(&buffered.argument_keys)
                                    .zip(&function.arguments)
                                {
                                    with_value(column, row, |value| {
                                        data_type.append_key(
                                            value,
                                            &mut distinct_key,
                                            context.query,
                                        )
                                    })?;
                                    debug_assert_eq!(column.data_type(), &argument.data_type);
                                }
                                let DistinctKeys::Canonical(seen) = &mut retained.seen else {
                                    return Err(Error::Internal(
                                        "canonical key used with borrowed VARCHAR DISTINCT state"
                                            .into(),
                                    ));
                                };
                                if seen.contains(&distinct_key) {
                                    continue;
                                }
                                context.query.check_rows(seen.len() + 1)?;
                                seen.insert(distinct_key.clone());
                            }
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
            row.extend(
                results
                    .iter_mut()
                    .map(|values| std::mem::replace(&mut values[group], Value::Null)),
            );
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    context.query.check()?;
    Ok(Some(rows))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BufferedAccumulator {
    fn resize(&mut self, groups: usize) {
        let borrowed_varchar_key = self.distinct
            && matches!(
                self.argument_keys.as_slice(),
                [data_type]
                    if data_type.key_representation() == KeyRepresentation::VarcharBytes
            );
        self.groups.resize_with(groups, || {
            BufferedGroup::new(
                self.argument_types.len(),
                self.order.len(),
                borrowed_varchar_key,
            )
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
                    let values = Vector::flat(data_type.clone(), values)?;
                    if self.order.is_empty() {
                        Ok(values)
                    } else {
                        Arc::new(values).select(permutation.clone())
                    }
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
    if let Some(permutation) = signed_counting_permutation(columns, order, types, context)? {
        return Ok(permutation);
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
            for (offset, output) in scratch[start..end].iter_mut().enumerate() {
                if offset % 1024 == 0 {
                    context.query.check()?;
                }
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
fn signed_counting_permutation(
    columns: &[Vec<Value>],
    order: &[OrderExpr],
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<usize>>> {
    let ([column], [order], [data_type]) = (columns, order, types) else {
        return Ok(None);
    };
    if data_type.ordering_representation() != OrderingRepresentation::SignedInteger
        || data_type.requires_logical_validation()
    {
        return Ok(None);
    }
    let rows = column.len();
    if rows == 0 {
        return Ok(Some(Vec::new()));
    }
    let mut minimum = None::<i128>;
    let mut maximum = None::<i128>;
    let mut has_null = false;
    for (row, value) in column.iter().enumerate() {
        if row % 1024 == 0 {
            context.query.check()?;
        }
        match value {
            Value::Null => has_null = true,
            Value::Integer(value) => {
                minimum = Some(minimum.map_or(*value, |current| current.min(*value)));
                maximum = Some(maximum.map_or(*value, |current| current.max(*value)));
            }
            _ => {
                return Err(Error::Internal(
                    "signed ordering value differs from selected representation".into(),
                ));
            }
        }
    }
    let numeric_buckets = match (minimum, maximum) {
        (Some(minimum), Some(maximum)) => maximum
            .checked_sub(minimum)
            .and_then(|span| span.checked_add(1))
            .and_then(|span| usize::try_from(span).ok()),
        (None, None) => Some(0),
        _ => unreachable!("minimum and maximum are updated together"),
    };
    let Some(numeric_buckets) = numeric_buckets else {
        return Ok(None);
    };
    let Some(bucket_count) = numeric_buckets.checked_add(usize::from(has_null)) else {
        return Ok(None);
    };
    if bucket_count > rows.min(MAX_SIGNED_ORDER_BUCKETS) {
        return Ok(None);
    }
    let mut offsets = vec![0usize; bucket_count];
    let minimum = minimum.unwrap_or(0);
    let null_bucket = numeric_buckets;
    for (row, value) in column.iter().enumerate() {
        if row % 1024 == 0 {
            context.query.check()?;
        }
        let bucket = signed_order_bucket(value, minimum, null_bucket)?;
        offsets[bucket] = offsets[bucket]
            .checked_add(1)
            .ok_or_else(|| Error::Resource("signed order bucket count overflow".into()))?;
    }

    let mut next = 0usize;
    if order.nulls_first && has_null {
        let count = offsets[null_bucket];
        offsets[null_bucket] = next;
        next += count;
    }
    if order.descending {
        for (iteration, bucket) in (0..numeric_buckets).rev().enumerate() {
            if iteration % 1024 == 0 {
                context.query.check()?;
            }
            let count = offsets[bucket];
            offsets[bucket] = next;
            next += count;
        }
    } else {
        for (bucket, offset) in offsets[..numeric_buckets].iter_mut().enumerate() {
            if bucket % 1024 == 0 {
                context.query.check()?;
            }
            let count = *offset;
            *offset = next;
            next += count;
        }
    }
    if !order.nulls_first && has_null {
        offsets[null_bucket] = next;
    }

    let mut permutation = vec![0usize; rows];
    for (row, value) in column.iter().enumerate() {
        if row % 1024 == 0 {
            context.query.check()?;
        }
        let bucket = signed_order_bucket(value, minimum, null_bucket)?;
        let output = offsets[bucket];
        permutation[output] = row;
        offsets[bucket] = output + 1;
    }
    context.query.check()?;
    Ok(Some(permutation))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn signed_order_bucket(value: &Value, minimum: i128, null_bucket: usize) -> Result<usize> {
    match value {
        Value::Null => Ok(null_bucket),
        Value::Integer(value) => value
            .checked_sub(minimum)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or_else(|| Error::Internal("signed order bucket is outside planned span".into())),
        _ => Err(Error::Internal(
            "signed ordering value differs from selected representation".into(),
        )),
    }
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
                let comparison = if data_type.ordering_representation()
                    == OrderingRepresentation::SignedInteger
                    && !data_type.requires_logical_validation()
                {
                    let (Value::Integer(left), Value::Integer(right)) = (left, right) else {
                        return Err(Error::Internal(
                            "signed ordering value differs from selected representation".into(),
                        ));
                    };
                    left.cmp(right)
                } else {
                    data_type.compare(left, right, context.query)?
                };
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
