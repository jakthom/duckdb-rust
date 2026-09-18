use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    common::{
        Row,
        vector::{DataChunk, Vector},
    },
    execution::{
        ExecutionContext,
        operator::aggregate::{AggregateResult, AggregationAlgorithm},
        physical_plan::NativePhysicalPlanner,
        stream::BatchStream,
    },
    planner::aggregation::Aggregation,
};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
struct LegacyRows {
    calls: Arc<AtomicUsize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregationAlgorithm for LegacyRows {
    fn name(&self) -> &'static str {
        "legacy-aggregate-transport"
    }

    fn aggregate(
        &self,
        input: &mut dyn BatchStream,
        _: &Aggregation,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        while input.next(context.query.batch_size())?.is_some() {
            context.query.check()?;
        }
        Ok(vec![vec![Value::Integer(7)]])
    }
}

#[derive(Clone, Copy, Debug)]
enum ColumnShape {
    Exact,
    WrongWidth,
    WrongType,
}

struct ColumnResult {
    shape: ColumnShape,
    result_calls: Arc<AtomicUsize>,
    legacy_calls: Arc<AtomicUsize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Debug for ColumnResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ColumnResult")
            .field("shape", &self.shape)
            .finish()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregationAlgorithm for ColumnResult {
    fn name(&self) -> &'static str {
        "column-aggregate-transport"
    }

    fn aggregate(
        &self,
        _: &mut dyn BatchStream,
        _: &Aggregation,
        _: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        self.legacy_calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::Internal(
            "column aggregate transport incorrectly used legacy callback".into(),
        ))
    }

    fn aggregate_result(
        &self,
        input: &mut dyn BatchStream,
        _: &Aggregation,
        context: &ExecutionContext<'_>,
    ) -> Result<AggregateResult> {
        self.result_calls.fetch_add(1, Ordering::SeqCst);
        while input.next(context.query.batch_size())?.is_some() {
            context.query.check()?;
        }
        let columns = match self.shape {
            ColumnShape::Exact => vec![Vector::flat(DataType::HugeInt, vec![Value::Integer(7)])?],
            ColumnShape::WrongWidth => Vec::new(),
            ColumnShape::WrongType => vec![Vector::flat(
                DataType::Varchar,
                vec![Value::Varchar("wrong".into())],
            )?],
        };
        Ok(AggregateResult::Columns(DataChunk::new(columns, 1)?))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn query(algorithm: Arc<dyn AggregationAlgorithm>) -> Result<duckdb_rust::QueryResult> {
    let database = DatabaseBuilder::new()
        .physical_planner(Arc::new(
            NativePhysicalPlanner::default().with_aggregation(algorithm),
        ))
        .batch_size(1)
        .build()?;
    database.connect().query("SELECT sum(i) FROM range(3) t(i)")
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aggregate_result_default_uses_legacy_custom_callback_once() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let result = query(Arc::new(LegacyRows {
        calls: calls.clone(),
    }))?;
    assert_eq!(result.rows, vec![vec![Value::Integer(7)]]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aggregate_result_column_override_bypasses_legacy_callback_once() -> Result<()> {
    let result_calls = Arc::new(AtomicUsize::new(0));
    let legacy_calls = Arc::new(AtomicUsize::new(0));
    let result = query(Arc::new(ColumnResult {
        shape: ColumnShape::Exact,
        result_calls: result_calls.clone(),
        legacy_calls: legacy_calls.clone(),
    }))?;
    assert_eq!(result.rows, vec![vec![Value::Integer(7)]]);
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
    assert_eq!(legacy_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aggregate_result_column_override_rejects_exact_schema_mismatches() -> Result<()> {
    for shape in [ColumnShape::WrongWidth, ColumnShape::WrongType] {
        let result_calls = Arc::new(AtomicUsize::new(0));
        let legacy_calls = Arc::new(AtomicUsize::new(0));
        let error = query(Arc::new(ColumnResult {
            shape,
            result_calls: result_calls.clone(),
            legacy_calls: legacy_calls.clone(),
        }))
        .unwrap_err();
        assert!(
            matches!(error, Error::Internal(message) if message == "aggregate output differs from its declared schema")
        );
        assert_eq!(result_calls.load(Ordering::SeqCst), 1);
        assert_eq!(legacy_calls.load(Ordering::SeqCst), 0);
    }
    Ok(())
}
