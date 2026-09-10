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
                    for value in batch.columns()[*ordinal].values() {
                        data_type
                            .validate(value, self.query)
                            .map_err(|error| match error {
                                Error::Conversion(_) => Error::Internal(
                                    "operator returned an invalid logical value".into(),
                                ),
                                other => other,
                            })?;
                    }
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
