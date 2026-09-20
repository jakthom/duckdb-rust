use super::{DataSet, ExecutionContext, physical_plan::PhysicalOperator};
use crate::{
    common::{
        Error, Result, Row, Value,
        vector::{DataChunk, Vector},
    },
    parallel::QueryContext,
    planner::Schema,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Demand is a maximum, not a required batch size. Some batches contain between
/// one and max_rows rows; None is permanent exhaustion. A cursor is local to its
/// driver and owns its progress; physical plans can be shared by many cursors.
/// Output chunks own their data and survive later calls and cursor destruction.
/// Errors terminate a cursor, and dropping it performs no further computation.
pub trait BatchStream {
    fn next(&mut self, max_rows: usize) -> Result<Option<DataChunk>>;
}

pub type Stream<'a> = Box<dyn BatchStream + 'a>;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Validate adapter output at every operator boundary and make terminal states
/// permanent, including failures. This also bounds each producer's demand.
pub fn open<'a>(
    plan: &'a dyn PhysicalOperator,
    context: &'a ExecutionContext<'a>,
) -> Result<Stream<'a>> {
    context.query.check()?;
    let mut validators = Vec::new();
    for (ordinal, field) in plan.schema().iter().enumerate() {
        let data_type = context.query.types().bind(&field.data_type)?;
        if data_type.requires_logical_validation() {
            validators.push((ordinal, data_type));
        }
    }
    Ok(Box::new(CheckedStream {
        inner: Some(plan.open(context)?),
        schema: plan.schema(),
        validators,
        query: context.query,
    }))
}

struct CheckedStream<'a> {
    inner: Option<Stream<'a>>,
    schema: &'a Schema,
    validators: Vec<(usize, crate::common::type_registry::BoundType)>,
    query: &'a QueryContext,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BatchStream for CheckedStream<'_> {
    fn next(&mut self, max_rows: usize) -> Result<Option<DataChunk>> {
        if self.inner.is_none() {
            return Ok(None);
        }
        let result = (|| {
            let max_rows = self.query.batch_demand(max_rows)?;
            let batch = self.inner.as_mut().expect("live stream").next(max_rows)?;
            if let Some(batch) = &batch {
                if batch.is_empty() || batch.len() > max_rows {
                    return Err(Error::Internal(
                        "operator violated batch cardinality".into(),
                    ));
                }
                if batch.columns().len() != self.schema.len()
                    || batch
                        .columns()
                        .iter()
                        .zip(self.schema)
                        .any(|(column, field)| column.data_type() != &field.data_type)
                {
                    return Err(Error::Internal(
                        "operator batch differs from its declared schema".into(),
                    ));
                }
                for (ordinal, data_type) in &self.validators {
                    data_type
                        .validate_vector(&batch.columns()[*ordinal], self.query)
                        .map_err(|error| match error {
                            Error::Conversion(_) => {
                                Error::Internal("operator returned an invalid logical value".into())
                            }
                            other => other,
                        })?;
                }
            }
            self.query.check()?;
            Ok(batch)
        })();
        if !matches!(result, Ok(Some(_))) {
            self.inner = None;
        }
        result
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn collect(plan: &dyn PhysicalOperator, context: &ExecutionContext<'_>) -> Result<DataSet> {
    let mut input = open(plan, context)?;
    let mut rows = Vec::new();
    while let Some(batch) = input.next(context.query.batch_size())? {
        context
            .query
            .check_rows(rows.len().saturating_add(batch.len()))?;
        rows.extend(batch.rows());
    }
    Ok(DataSet {
        schema: plan.schema().clone(),
        rows,
        chunks: None,
    })
}

struct CallbackStream<F>(Option<F>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<F: FnMut(usize) -> Result<Option<DataChunk>>> BatchStream for CallbackStream<F> {
    fn next(&mut self, max_rows: usize) -> Result<Option<DataChunk>> {
        let Some(callback) = &mut self.0 else {
            return Ok(None);
        };
        let result = if max_rows == 0 {
            Err(Error::Internal("batch demand must be positive".into()))
        } else {
            callback(max_rows)
        };
        if !matches!(result, Ok(Some(_))) {
            self.0 = None;
        }
        result
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn from_fn<'a>(
    callback: impl FnMut(usize) -> Result<Option<DataChunk>> + 'a,
) -> Stream<'a> {
    Box::new(CallbackStream(Some(callback)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn chunk(schema: &Schema, rows: &[Row]) -> Result<Option<DataChunk>> {
    if rows.is_empty() {
        return Ok(None);
    }
    DataChunk::from_rows(
        &schema
            .iter()
            .map(|f| f.data_type.clone())
            .collect::<Vec<_>>(),
        rows,
    )
    .map(Some)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn deferred<'a>(
    schema: &'a Schema,
    context: &'a ExecutionContext<'a>,
    load: impl FnOnce() -> Result<Vec<Row>> + 'a,
) -> Stream<'a> {
    let mut load = Some(load);
    let mut rows = None;
    from_fn(move |max_rows| {
        if let Some(load) = load.take() {
            let loaded = load()?;
            context.query.check_rows(loaded.len())?;
            rows = Some(loaded.into_iter());
        }
        let next: Vec<_> = rows
            .as_mut()
            .expect("loaded stream")
            .take(max_rows)
            .collect();
        chunk(schema, &next)
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn owned_chunk(schema: &Schema, rows: Vec<Row>) -> Result<Option<DataChunk>> {
    if rows.is_empty() {
        return Ok(None);
    }
    DataChunk::from_owned_rows(
        &schema
            .iter()
            .map(|field| field.data_type.clone())
            .collect::<Vec<_>>(),
        rows,
    )
    .map(Some)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Aggregate algorithms already return newly owned rows. Consume each emitted
/// batch at this boundary so heap-owning results are not copied into vectors.
/// Other deferred operators retain the established borrowed conversion above.
pub(crate) fn deferred_owned<'a>(
    schema: &'a Schema,
    context: &'a ExecutionContext<'a>,
    load: impl FnOnce() -> Result<Vec<Row>> + 'a,
) -> Stream<'a> {
    let mut load = Some(load);
    let mut rows = None;
    from_fn(move |max_rows| {
        if let Some(load) = load.take() {
            let loaded = load()?;
            context.query.check_rows(loaded.len())?;
            rows = Some(loaded.into_iter());
        }
        let next: Vec<_> = rows
            .as_mut()
            .expect("loaded stream")
            .take(max_rows)
            .collect();
        owned_chunk(schema, next)
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Publish blocking-sort rows while retaining the sort's quota token in every
/// emitted batch. The stream owns one clone until exhaustion; retained caller
/// batches keep their own clone after the cursor and connection are dropped.
pub(crate) fn deferred_sorted<'a>(
    schema: &'a Schema,
    context: &'a ExecutionContext<'a>,
    load: impl FnOnce() -> Result<super::operator::order::SortedRows> + 'a,
) -> Stream<'a> {
    let mut load = Some(load);
    let mut rows = None;
    let mut reservation = None;
    from_fn(move |max_rows| {
        if let Some(load) = load.take() {
            let sorted = load()?;
            context.query.check_rows(sorted.rows.len())?;
            reservation = sorted.reservation;
            rows = Some(sorted.rows.into_iter());
        }
        let count = rows
            .as_ref()
            .expect("loaded sorted rows")
            .len()
            .min(max_rows);
        if count == 0 {
            rows.take();
            reservation.take();
            return Ok(None);
        }
        // Row payload ownership transfers to columns, but their new slot arrays
        // coexist with the old row arrays during transposition. Admit that
        // temporary peak before allocating the batch carrier or columns.
        let temporary = if reservation.is_some() {
            let column_slots = schema
                .len()
                .checked_mul(count)
                .and_then(|n| n.checked_mul(std::mem::size_of::<Value>()))
                .and_then(|n| n.checked_mul(2))
                .ok_or_else(|| Error::Resource("sorted batch size overflow".into()))?;
            let bytes = count
                .checked_mul(std::mem::size_of::<Row>())
                .and_then(|n| n.checked_add(column_slots))
                .and_then(|n| {
                    schema
                        .len()
                        .checked_mul(std::mem::size_of::<Vector>())
                        .and_then(|columns| n.checked_add(columns))
                })
                .ok_or_else(|| Error::Resource("sorted batch size overflow".into()))?;
            Some(context.query.memory_pool().reserve(bytes, context.query)?)
        } else {
            None
        };
        let mut next = Vec::new();
        next.try_reserve_exact(count)
            .map_err(|_| Error::Resource("sorted batch allocation failed".into()))?;
        next.extend(rows.as_mut().expect("loaded sorted rows").take(count));
        let Some(batch) = owned_chunk(schema, next)? else {
            return Ok(None);
        };
        drop(temporary);
        Ok(Some(match reservation.as_ref() {
            Some(token) => batch.with_reservation(token.clone()),
            None => batch,
        }))
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Preserve the selected row adapter's publication path, or publish its owned
/// columns after validating the complete blocking result before the first row.
pub(crate) fn deferred_aggregate<'a>(
    schema: &'a Schema,
    context: &'a ExecutionContext<'a>,
    own_rows: bool,
    load: impl FnOnce() -> Result<super::operator::aggregate::AggregateResult> + 'a,
) -> Stream<'a> {
    use super::operator::aggregate::AggregateResult;
    let mut load = Some(load);
    let mut output: Option<Stream<'a>> = None;
    from_fn(move |max_rows| {
        context.query.check()?;
        if let Some(load) = load.take() {
            output = Some(match load()? {
                AggregateResult::Rows(rows) => {
                    if own_rows {
                        deferred_owned(schema, context, move || Ok(rows))
                    } else {
                        deferred(schema, context, move || Ok(rows))
                    }
                }
                AggregateResult::Columns(columns) => {
                    context.query.check_rows(columns.len())?;
                    if columns.columns().len() != schema.len()
                        || columns
                            .columns()
                            .iter()
                            .zip(schema)
                            .any(|(column, field)| column.data_type() != &field.data_type)
                    {
                        return Err(Error::Internal(
                            "aggregate output differs from its declared schema".into(),
                        ));
                    }
                    let mut position = 0;
                    from_fn(move |max_rows| {
                        context.query.check()?;
                        if position == columns.len() {
                            return Ok(None);
                        }
                        let count = max_rows.min(columns.len() - position);
                        let batch = columns.slice(position, count)?;
                        position += count;
                        Ok(Some(batch))
                    })
                }
            });
        }
        context.query.check()?;
        output.as_mut().expect("loaded aggregate").next(max_rows)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        common::{DataType, Value},
        execution::{
            expression_executor::ScalarEvaluator,
            physical_plan::NativePhysicalPlanner,
            subquery::{PreparedSubqueries, StreamingSubqueries},
        },
        planner::Field,
        storage::checkpoint::MemoryDurability,
        transaction::{SnapshotTransactions, TransactionManager},
    };
    use std::sync::Arc;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn aggregate_owned_deferred_preserves_demand_ownership_and_terminal_errors() -> Result<()> {
        let query = QueryContext::background();
        let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
        let transaction = manager.begin()?;
        let planner = NativePhysicalPlanner::default();
        let prepared = PreparedSubqueries::new(&planner);
        let context = ExecutionContext {
            transaction: transaction.as_ref(),
            query: &query,
            expressions: &ScalarEvaluator,
            subquery_plans: &prepared,
            subqueries: &StreamingSubqueries,
            outer: None,
            recursive: None,
        };
        let schema = vec![Field::new("text", DataType::Varchar)];
        let first = String::from("first-é\0");
        let first_pointer = first.as_ptr();
        let mut output = deferred_owned(&schema, &context, move || {
            Ok(vec![
                vec![Value::Varchar(first)],
                vec![Value::Null],
                vec![Value::Varchar("last".into())],
            ])
        });
        let batch = output.next(1)?.expect("first owned aggregate batch");
        assert_eq!(batch.len(), 1);
        let Value::Varchar(value) = &batch.columns()[0]
            .flat_values()
            .expect("owned VARCHAR flat values")[0]
        else {
            panic!("owned VARCHAR result");
        };
        assert_eq!(value.as_ptr(), first_pointer);
        drop(batch);
        let batch = output.next(8)?.expect("remaining owned aggregate batch");
        assert_eq!(
            batch.rows().collect::<Vec<_>>(),
            vec![vec![Value::Null], vec![Value::Varchar("last".into())]]
        );
        assert!(output.next(8)?.is_none());
        assert!(output.next(8)?.is_none());

        let mut load_error = deferred_owned(&schema, &context, || {
            Err(Error::Execution("owned aggregate load failure".into()))
        });
        assert!(matches!(load_error.next(1), Err(Error::Execution(_))));
        assert!(load_error.next(1)?.is_none());

        let mut conversion_error =
            deferred_owned(&schema, &context, || Ok(vec![vec![Value::Integer(1)]]));
        assert!(conversion_error.next(1).is_err());
        assert!(conversion_error.next(1)?.is_none());

        let mut invalid_demand = deferred_owned(&schema, &context, || {
            Ok(vec![vec![Value::Varchar("unused".into())]])
        });
        assert!(matches!(invalid_demand.next(0), Err(Error::Internal(_))));
        assert!(invalid_demand.next(1)?.is_none());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn aggregate_result_columns_preserve_demand_ownership_and_terminal_contracts() -> Result<()> {
        use crate::{
            common::vector::Vector, execution::operator::aggregate::AggregateResult,
            parallel::InterruptHandle,
        };
        let interrupt = InterruptHandle::default();
        let query = QueryContext::new(interrupt.clone(), None, 2, 3)?;
        let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
        let transaction = manager.begin()?;
        let planner = NativePhysicalPlanner::default();
        let prepared = PreparedSubqueries::new(&planner);
        let context = ExecutionContext {
            transaction: transaction.as_ref(),
            query: &query,
            expressions: &ScalarEvaluator,
            subquery_plans: &prepared,
            subqueries: &StreamingSubqueries,
            outer: None,
            recursive: None,
        };
        let schema = vec![Field::new("text", DataType::Varchar)];
        let text = String::from("column-é\0");
        let pointer = text.as_ptr();
        let columns = DataChunk::new(
            vec![Vector::flat(
                DataType::Varchar,
                vec![
                    Value::Varchar(text),
                    Value::Null,
                    Value::Varchar("last".into()),
                ],
            )?],
            3,
        )?;
        let loads = std::cell::Cell::new(0);
        let mut output = deferred_aggregate(&schema, &context, true, || {
            loads.set(loads.get() + 1);
            Ok(AggregateResult::Columns(columns))
        });
        assert_eq!(loads.get(), 0);
        let retained = output.next(1)?.expect("first column slice");
        assert_eq!(retained.len(), 1);
        assert_eq!(loads.get(), 1);
        let Value::Varchar(value) = &retained.columns()[0].flat_values().unwrap()[0] else {
            panic!("VARCHAR result");
        };
        assert_eq!(value.as_ptr(), pointer);
        assert_eq!(
            output.next(8)?.unwrap().rows().collect::<Vec<_>>(),
            vec![vec![Value::Null], vec![Value::Varchar("last".into())]]
        );
        assert!(output.next(1)?.is_none());
        assert!(output.next(1)?.is_none());
        assert_eq!(loads.get(), 1);
        drop(output);
        assert_eq!(
            retained.rows().next().unwrap(),
            vec![Value::Varchar("column-é\0".into())]
        );

        for columns in [
            DataChunk::new(vec![], 1)?,
            DataChunk::new(
                vec![Vector::flat(DataType::BigInt, vec![Value::Integer(1)])?],
                1,
            )?,
            // Even empty output must not hide a bad declared schema.
            DataChunk::new(vec![], 0)?,
        ] {
            let mut output = deferred_aggregate(&schema, &context, true, || {
                Ok(AggregateResult::Columns(columns))
            });
            assert!(matches!(output.next(1), Err(Error::Internal(_))));
            assert!(output.next(1)?.is_none());
        }
        let empty = DataChunk::new(vec![Vector::flat(DataType::Varchar, vec![])?], 0)?;
        let mut output = deferred_aggregate(&schema, &context, false, || {
            Ok(AggregateResult::Columns(empty))
        });
        assert!(output.next(1)?.is_none());
        assert!(output.next(1)?.is_none());

        let zero_schema = vec![];
        let mut zero_width = deferred_aggregate(&zero_schema, &context, false, || {
            Ok(AggregateResult::Columns(DataChunk::new(vec![], 3)?))
        });
        assert_eq!(zero_width.next(2)?.unwrap().rows().count(), 2);
        assert_eq!(zero_width.next(2)?.unwrap().rows().count(), 1);
        assert!(zero_width.next(2)?.is_none());

        let mut oversized = deferred_aggregate(&zero_schema, &context, false, || {
            Ok(AggregateResult::Columns(DataChunk::new(vec![], 4)?))
        });
        assert!(matches!(oversized.next(1), Err(Error::Resource(_))));
        assert!(oversized.next(1)?.is_none());
        let mut failed = deferred_aggregate(&schema, &context, false, || {
            Err(Error::Execution(
                "aggregate failed before publication".into(),
            ))
        });
        assert!(matches!(failed.next(1), Err(Error::Execution(_))));
        assert!(failed.next(1)?.is_none());

        let mut no_demand = deferred_aggregate(&schema, &context, false, || {
            panic!("invalid demand must not load output")
        });
        assert!(matches!(no_demand.next(0), Err(Error::Internal(_))));
        assert!(no_demand.next(1)?.is_none());
        let mut cancelled = deferred_aggregate(&zero_schema, &context, false, || {
            interrupt.interrupt();
            Ok(AggregateResult::Columns(DataChunk::new(vec![], 1)?))
        });
        assert!(matches!(cancelled.next(1), Err(Error::Interrupted)));
        interrupt.reset();
        assert!(cancelled.next(1)?.is_none());
        Ok(())
    }
}
