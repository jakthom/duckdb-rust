use super::*;
use crate::{
    common::{
        Error, Value,
        type_registry::{BoundType, OrderingRepresentation},
    },
    execution::subquery::PreparedExpression,
};
use std::cmp::Ordering;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn row_bytes(row: &[Value]) -> Result<usize> {
    row.iter().try_fold(0usize, |total, value| {
        let payload = match value {
            Value::Varchar(value) => value.len(),
            Value::Blob(value) => value.len(),
            _ => 0,
        };
        total
            .checked_add(std::mem::size_of::<Value>())
            .and_then(|bytes| bytes.checked_add(payload))
            .ok_or_else(|| Error::Resource("comparison sort reservation overflow".into()))
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn vec_bytes<T>(count: usize) -> Result<usize> {
    count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| Error::Resource("comparison sort reservation overflow".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// The selected E1 path is a direct single VARCHAR column. Its payload length
/// is available without cloning values, so admission happens before row copy.
fn direct_varchar_batch_bytes(batch: &crate::common::vector::DataChunk) -> Result<Option<usize>> {
    let [column] = batch.columns() else {
        return Ok(None);
    };
    if column.data_type() != &crate::common::DataType::Varchar {
        return Ok(None);
    }
    (0..column.len())
        .try_fold(0usize, |bytes, index| {
            let value = column.varchar_at(index).ok_or_else(|| {
                Error::Internal("direct VARCHAR sort received another value type".into())
            })?;
            bytes
                .checked_add(std::mem::size_of::<Value>())
                .and_then(|n| n.checked_add(value.map_or(0, str::len)))
                .ok_or_else(|| Error::Resource("comparison sort reservation overflow".into()))
        })
        .map(Some)
}

/// Fallible stable merge sorting retains arbitrary type adapters and expressions.
#[derive(Debug, Default)]
pub struct ComparisonSort;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SortAlgorithm for ComparisonSort {
    fn name(&self) -> &'static str {
        "comparison-sort"
    }
    fn sort(
        &self,
        input: &mut dyn BatchStream,
        order: &[OrderExpr],
        context: &ExecutionContext<'_>,
    ) -> Result<SortedRows> {
        let mut output_reservations = Vec::new();
        let mut temporary_reservations = Vec::new();
        let types = order
            .iter()
            .map(|o| context.query.types().bind(&o.expression.data_type))
            .collect::<Result<Vec<_>>>()?;
        let expressions = order
            .iter()
            .map(|o| PreparedExpression::new(&o.expression))
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        let mut row_capacity_reservation = None;
        while let Some(batch) = input.next(context.query.batch_size())? {
            context
                .query
                .check_rows(rows.len().saturating_add(batch.len()))?;
            // Reserve the selected operator's retained row copies before the
            // Vec grows. Input vectors are source storage and intentionally
            // outside this E1 accounting boundary.
            let direct = direct_varchar_batch_bytes(&batch)?;
            let required = rows
                .len()
                .checked_add(batch.len())
                .ok_or_else(|| Error::Resource("comparison sort row count overflow".into()))?;
            if required > rows.capacity() {
                // Both allocations can coexist during growth. Keep the old
                // token until the new backing allocation has been installed.
                let reservation = context
                    .query
                    .memory_pool()
                    .reserve(vec_bytes::<Row>(required)?, context.query)?;
                rows.try_reserve_exact(required - rows.len())
                    .map_err(|_| Error::Resource("comparison sort row allocation failed".into()))?;
                row_capacity_reservation = Some(reservation);
            }
            if let Some(payload) = direct {
                output_reservations.push(
                    context
                        .query
                        .memory_pool()
                        .reserve(payload, context.query)?,
                );
                let column = &batch.columns()[0];
                for index in 0..column.len() {
                    context.query.check()?;
                    let value = match column
                        .varchar_at(index)
                        .ok_or_else(|| Error::Internal("invalid VARCHAR encoding".into()))?
                    {
                        Some(text) => {
                            let mut owned = String::new();
                            owned.try_reserve_exact(text.len()).map_err(|_| {
                                Error::Resource("comparison sort string allocation failed".into())
                            })?;
                            owned.push_str(text);
                            Value::Varchar(owned)
                        }
                        None => Value::Null,
                    };
                    let mut row = Vec::new();
                    row.try_reserve_exact(1).map_err(|_| {
                        Error::Resource("comparison sort value allocation failed".into())
                    })?;
                    row.push(value);
                    rows.push(row);
                }
            } else {
                // Generic row values keep a retained charge; exact admission
                // before adapter-owned cloning is a later operator extension.
                for row in batch.rows() {
                    context.query.check()?;
                    output_reservations.push(
                        context
                            .query
                            .memory_pool()
                            .reserve(row_bytes(&row)?, context.query)?,
                    );
                    rows.push(row);
                }
            }
        }
        let direct_column = if order.len() == 1
            && order[0].expression.data_type == crate::common::DataType::Varchar
            && types[0].ordering_representation() == OrderingRepresentation::VarcharBytes
            && let crate::planner::ExprKind::Column(column) = order[0].expression.kind
            && rows.first().is_none_or(|row| column < row.len())
        {
            Some(column)
        } else {
            None
        };
        let owned_keys = if direct_column.is_some() {
            None
        } else {
            temporary_reservations.push(
                context
                    .query
                    .memory_pool()
                    .reserve(vec_bytes::<Row>(rows.len())?, context.query)?,
            );
            let mut keys = Vec::new();
            keys.try_reserve(rows.len())
                .map_err(|_| Error::Resource("comparison sort key allocation failed".into()))?;
            for row in &rows {
                context.query.check()?;
                let key = expressions
                    .iter()
                    .zip(&types)
                    .map(|(expression, data_type)| {
                        let value = expression.evaluate(row, context)?;
                        data_type.validate(&value, context.query)?;
                        Ok(value)
                    })
                    .collect::<Result<Row>>()?;
                temporary_reservations.push(
                    context
                        .query
                        .memory_pool()
                        .reserve(row_bytes(&key)?, context.query)?,
                );
                keys.push(key);
            }
            Some(keys)
        };
        let keys = owned_keys.as_deref().unwrap_or(&rows);
        if let Some(column) = direct_column {
            for row in keys {
                types[0].validate(&row[column], context.query)?;
            }
        }
        let indexes = vec_bytes::<usize>(rows.len())?;
        temporary_reservations.push(
            context
                .query
                .memory_pool()
                .reserve(indexes, context.query)?,
        );
        let mut permutation = Vec::new();
        permutation
            .try_reserve(rows.len())
            .map_err(|_| Error::Resource("comparison sort permutation allocation failed".into()))?;
        let run = if let Some(column) = direct_column {
            direct_varchar_run(keys, column, &order[0], context)?
        } else {
            RunOrder::Unordered
        };
        match run {
            RunOrder::Ascending => permutation.extend(0..rows.len()),
            RunOrder::StrictDescending => permutation.extend((0..rows.len()).rev()),
            RunOrder::Unordered => {
                permutation.extend(0..rows.len());
                temporary_reservations.push(
                    context
                        .query
                        .memory_pool()
                        .reserve(indexes, context.query)?,
                );
                let mut scratch = Vec::new();
                scratch.try_reserve_exact(rows.len()).map_err(|_| {
                    Error::Resource("comparison sort scratch allocation failed".into())
                })?;
                scratch.resize(rows.len(), 0);
                let mut width = 1usize;
                while width < rows.len() {
                    for start in (0..rows.len()).step_by(width.saturating_mul(2)) {
                        context.query.check()?;
                        let middle = start.saturating_add(width).min(rows.len());
                        let end = middle.saturating_add(width).min(rows.len());
                        let (mut left, mut right) = (start, middle);
                        for output in &mut scratch[start..end] {
                            let comparison = if let Some(column) = direct_column
                                && left < middle
                                && right < end
                            {
                                direct_varchar_compare(
                                    &keys[permutation[left]][column],
                                    &keys[permutation[right]][column],
                                    &order[0],
                                )
                            } else if left < middle && right < end {
                                compare(
                                    &keys[permutation[left]],
                                    &keys[permutation[right]],
                                    order,
                                    &types,
                                    context,
                                )?
                            } else {
                                Ordering::Less
                            };
                            let take_left =
                                left < middle && (right == end || comparison != Ordering::Greater);
                            let position = if take_left {
                                let p = left;
                                left += 1;
                                p
                            } else {
                                let p = right;
                                right += 1;
                                p
                            };
                            *output = permutation[position];
                        }
                    }
                    std::mem::swap(&mut permutation, &mut scratch);
                    width = width.saturating_mul(2);
                }
            }
        }
        context.query.check()?;
        temporary_reservations.push(
            context
                .query
                .memory_pool()
                .reserve(vec_bytes::<Option<Row>>(rows.len())?, context.query)?,
        );
        let mut carrier = Vec::new();
        carrier
            .try_reserve_exact(rows.len())
            .map_err(|_| Error::Resource("comparison sort carrier allocation failed".into()))?;
        carrier.extend(rows.into_iter().map(Some));
        output_reservations.push(
            context
                .query
                .memory_pool()
                .reserve(vec_bytes::<Row>(carrier.len())?, context.query)?,
        );
        let mut rows = Vec::new();
        rows.try_reserve_exact(carrier.len())
            .map_err(|_| Error::Resource("comparison sort output allocation failed".into()))?;
        for index in permutation {
            context.query.check()?;
            rows.push(
                carrier[index]
                    .take()
                    .expect("sort permutation visits each row once"),
            );
        }
        // The carrier and its token cease to own rows after every row moved.
        drop(carrier);
        drop(owned_keys);
        drop(row_capacity_reservation);
        // Merge existing charges without a second admission or a release gap.
        drop(temporary_reservations);
        Ok(SortedRows {
            rows,
            reservation: Some(crate::parallel::Reservation::merge(output_reservations)),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunOrder {
    Ascending,
    StrictDescending,
    Unordered,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn direct_varchar_run(
    rows: &[Row],
    column: usize,
    order: &OrderExpr,
    context: &ExecutionContext<'_>,
) -> Result<RunOrder> {
    let mut ascending = true;
    let mut strict_descending = true;
    for (index, pair) in rows.windows(2).enumerate() {
        if index % 1024 == 0 {
            context.query.check()?;
        }
        match direct_varchar_compare(&pair[0][column], &pair[1][column], order) {
            Ordering::Less => strict_descending = false,
            Ordering::Equal => strict_descending = false,
            Ordering::Greater => ascending = false,
        }
        if !ascending && !strict_descending {
            return Ok(RunOrder::Unordered);
        }
    }
    Ok(if ascending {
        RunOrder::Ascending
    } else if strict_descending {
        RunOrder::StrictDescending
    } else {
        RunOrder::Unordered
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn direct_varchar_compare(left: &Value, right: &Value, order: &OrderExpr) -> Ordering {
    let comparison = match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => {
            if order.nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (_, Value::Null) => {
            if order.nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Value::Varchar(left), Value::Varchar(right)) => left.as_bytes().cmp(right.as_bytes()),
        _ => unreachable!("validated VARCHAR ordering capability received another value type"),
    };
    if order.descending && !left.is_null() && !right.is_null() {
        comparison.reverse()
    } else {
        comparison
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare(
    left: &[Value],
    right: &[Value],
    order: &[OrderExpr],
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Ordering> {
    context.query.check()?;
    for (((a, b), order), data_type) in left.iter().zip(right).zip(order).zip(types) {
        let comparison = match (a.is_null(), b.is_null()) {
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
                let comparison = data_type.compare(a, b, context.query)?;
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
