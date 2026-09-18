use super::{DataSet, ExecutionContext, physical_plan::PhysicalOperator};
use crate::{
    common::{Error, Result, Row, vector::DataChunk},
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
}
