use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    common::{Row, vector::DataChunk},
    execution::{
        ExecutionContext, Executor, MaterializingExecutor, PullExecutor, StreamControl,
        expression_executor::ScalarEvaluator,
        physical_plan::{DeliveryMode, NativePhysicalPlanner, PhysicalOperator, PhysicalPlanner},
        stream::{self, BatchStream, Stream},
    },
    function::{FunctionEffects, FunctionRegistry, ScalarFunction},
    parallel::{InterruptHandle, QueryContext, Scheduler},
    planner::{Field, LogicalPlan, PlanNode, Schema},
    storage::checkpoint::MemoryDurability,
    transaction::{SnapshotTransactions, TransactionManager},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[path = "execution/batches.rs"]
mod batches;
#[path = "execution/expressions.rs"]
mod expressions;
#[path = "execution/joins.rs"]
mod joins;
#[path = "execution/sorting.rs"]
mod sorting;

fn ints(values: &[i128]) -> Row {
    values.iter().copied().map(Value::Integer).collect()
}

fn executors() -> Vec<Arc<dyn Executor>> {
    vec![Arc::new(PullExecutor), Arc::new(MaterializingExecutor)]
}

#[test]
fn executor_adapters_preserve_complete_relational_results_across_batches() -> Result<()> {
    for executor in executors() {
        for batch_size in [1, 3, 2048] {
            let db = DatabaseBuilder::new()
                .executor(executor.clone())
                .batch_size(batch_size)
                .build()?;
            let mut c = db.connect();
            c.execute("CREATE TABLE t(i INTEGER, v VARCHAR); INSERT INTO t VALUES (1,'a'),(NULL,NULL),(2,'b'),(1,'a'),(3,'c'),(NULL,'n')")?;
            for sql in [
                "SELECT i+1, v FROM t WHERE i>1",
                "SELECT DISTINCT i FROM t",
                "SELECT i, count(*), sum(i) FROM t GROUP BY i ORDER BY i",
                "SELECT count(DISTINCT i) FROM t",
                "SELECT i FROM t WHERE i<0 UNION ALL SELECT i FROM t WHERE i>1",
                "SELECT i FROM t WHERE i=1 UNION SELECT i FROM t",
                "SELECT a.i,b.i FROM t a FULL JOIN t b ON a.i=b.i ORDER BY a.i,b.i",
                "SELECT range FROM range(30) WHERE range%7=0 LIMIT 2 OFFSET 1",
                "SELECT i FROM t WHERE i>100",
                "SELECT 1 AS i UNION ALL SELECT 2 LIMIT 1",
                "SELECT count(*) FROM range(0)",
            ] {
                let expected = c.query(sql)?;
                let mut rows = Vec::new();
                let mut retained = Vec::new();
                let summary = c.query_batches(sql, |columns, batch| {
                    assert_eq!(columns.len(), expected.columns.len());
                    assert!(!batch.is_empty() && batch.len() <= batch_size);
                    rows.extend(batch.rows());
                    retained.push(batch);
                    Ok(StreamControl::Continue)
                })?;
                assert_eq!(
                    rows,
                    expected.rows,
                    "{} batch={batch_size}, {sql}",
                    executor.name()
                );
                assert_eq!(summary.execution.rows_delivered, rows.len());
                assert!(!summary.execution.stopped_early);
                assert_eq!(summary.columns.len(), expected.columns.len());
                assert_eq!(
                    retained
                        .iter()
                        .flat_map(DataChunk::rows)
                        .collect::<Vec<_>>(),
                    rows
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct CountCalls(Arc<AtomicUsize>);
impl ScalarFunction for CountCalls {
    fn name(&self) -> &str {
        "count_calls"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: false,
        }
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _types: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments.len() == 1 {
            Ok(arguments[0].clone())
        } else {
            Err(Error::Bind("one argument required".into()))
        }
    }
    fn evaluate(&self, arguments: &[Value], _: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(arguments[0].clone())
    }
}

#[test]
fn limit_and_consumer_stop_bound_actual_upstream_evaluation() -> Result<()> {
    for (executor, expected_calls) in [
        (Arc::new(PullExecutor) as Arc<dyn Executor>, 7),
        (Arc::new(MaterializingExecutor), 100),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(CountCalls(calls.clone())))?;
        let mut c = DatabaseBuilder::new()
            .executor(executor)
            .functions(functions)
            .batch_size(7)
            .build()?
            .connect();
        let summary =
            c.query_batches("SELECT count_calls(range) FROM range(100)", |_, batch| {
                assert_eq!(batch.len(), 7);
                Ok(StreamControl::Stop)
            })?;
        assert_eq!(summary.execution.rows_delivered, 7);
        assert!(summary.execution.stopped_early);
        assert_eq!(calls.swap(0, Ordering::Relaxed), expected_calls);
        assert_eq!(
            c.query("SELECT count_calls(range) FROM range(1000000000) LIMIT 2")?
                .rows,
            vec![ints(&[0]), ints(&[1])]
        );
        assert_eq!(calls.swap(0, Ordering::Relaxed), 2);
        c.query("SELECT count_calls(range) FROM range(1000000000) LIMIT 0")?;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            c.query("SELECT CAST(v AS INTEGER) FROM (VALUES ('1'),('bad')) t(v) LIMIT 1")?
                .rows,
            vec![ints(&[1])]
        );
    }
    Ok(())
}

#[test]
fn scan_filter_strategies_preserve_demand_errors_and_owned_results() -> Result<()> {
    use duckdb_rust::execution::physical_plan::ScanFilterStrategy;
    for strategy in [ScanFilterStrategy::Separate, ScanFilterStrategy::Fused] {
        for batch_size in [1, 7, 256] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut functions = FunctionRegistry::builtins();
            functions.register_scalar(Arc::new(CountCalls(calls.clone())))?;
            let mut c = DatabaseBuilder::new()
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_scan_filters(strategy),
                ))
                .functions(functions)
                .batch_size(batch_size)
                .build()?
                .connect();
            c.execute(
                "CREATE TABLE t AS SELECT range AS i FROM range(20); INSERT INTO t VALUES(NULL)",
            )?;
            assert_eq!(
                c.query("SELECT i FROM t WHERE count_calls(i)>=9 LIMIT 1")?
                    .rows,
                vec![ints(&[9])]
            );
            assert_eq!(calls.swap(0, Ordering::Relaxed), 10);
            assert_eq!(c.query("SELECT i FROM t WHERE CASE WHEN i>9 THEN CAST('bad' AS BOOLEAN) ELSE count_calls(i)=9 END LIMIT 1")?.rows, vec![ints(&[9])]);
            assert_eq!(calls.swap(0, Ordering::Relaxed), 10);
            assert!(matches!(
                c.query("SELECT i FROM t WHERE CAST('bad' AS BOOLEAN)"),
                Err(Error::Conversion(_))
            ));
            assert!(
                c.query("SELECT i FROM t WHERE count_calls(i)<0")?
                    .rows
                    .is_empty()
            );
            assert_eq!(calls.swap(0, Ordering::Relaxed), 21);
            let mut retained = Vec::new();
            c.query_batches("SELECT i FROM t WHERE i%3=0 OR i IS NULL", |_, batch| {
                retained.push(batch);
                Ok(StreamControl::Continue)
            })?;
            c.execute("DELETE FROM t")?;
            assert_eq!(
                retained
                    .iter()
                    .flat_map(DataChunk::rows)
                    .collect::<Vec<_>>(),
                (0..20)
                    .filter(|i| i % 3 == 0)
                    .map(|i| ints(&[i]))
                    .chain([vec![Value::Null]])
                    .collect::<Vec<_>>()
            );
        }
    }
    Ok(())
}

#[test]
fn streamed_results_and_aggregation_do_not_require_full_input_materialization() -> Result<()> {
    let mut c = DatabaseBuilder::new()
        .batch_size(64)
        .max_intermediate_rows(7)
        .build()?
        .connect();
    let mut count = 0;
    let mut sum = 0;
    let result = c.query_batches("SELECT range FROM range(10000)", |_, batch| {
        assert!(batch.len() <= 7);
        for row in batch.rows() {
            count += 1;
            sum += row[0].as_i128()?;
        }
        Ok(StreamControl::Continue)
    })?;
    assert_eq!(result.execution.rows_delivered, 10000);
    assert_eq!((count, sum), (10000, 49995000));
    assert_eq!(
        c.query("SELECT count(*),sum(range) FROM range(10000)")?
            .rows,
        vec![ints(&[10000, 49995000])]
    );
    assert!(matches!(
        c.query("SELECT * FROM range(10000)"),
        Err(Error::Resource(_))
    ));
    assert!(matches!(
        c.query("SELECT range FROM range(10000) ORDER BY range"),
        Err(Error::Resource(_))
    ));
    assert!(matches!(
        c.query("SELECT count(DISTINCT range) FROM range(10000)"),
        Err(Error::Resource(_))
    ));
    assert_eq!(
        c.query("SELECT range FROM range(10000) WHERE range=9999 LIMIT 1")?
            .rows,
        vec![ints(&[9999])]
    );
    Ok(())
}

#[test]
fn table_scan_visibility_and_owned_chunks_survive_other_writers() -> Result<()> {
    let manager = Arc::new(SnapshotTransactions::new(Arc::new(MemoryDurability))?);
    let writer = DatabaseBuilder::new()
        .transactions(manager.clone())
        .build()?;
    let mut w = writer.connect();
    w.execute("CREATE TABLE t AS SELECT range AS i FROM range(1000)")?;
    let mut reader = DatabaseBuilder::new()
        .transactions(manager)
        .batch_size(17)
        .max_intermediate_rows(40)
        .build()?
        .connect();
    let mut retained = None;
    let mut values = Vec::new();
    reader.query_batches("SELECT i FROM t", |_, batch| {
        if retained.is_none() {
            w.execute("DELETE FROM t WHERE i>=10; INSERT INTO t VALUES (2000)")?;
            retained = Some(batch.clone());
        }
        values.extend(batch.rows().map(|row| row[0].clone()));
        Ok(StreamControl::Continue)
    })?;
    assert_eq!(values, (0..1000).map(Value::Integer).collect::<Vec<_>>());
    assert_eq!(
        reader.query("SELECT count(*),sum(i) FROM t")?.rows,
        vec![ints(&[11, 2045])]
    );
    drop(reader);
    assert_eq!(
        retained.unwrap().rows().collect::<Vec<_>>(),
        (0..17).map(|i| ints(&[i])).collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn batch_errors_and_cancellation_follow_the_transaction_contract() -> Result<()> {
    let db = DatabaseBuilder::new().batch_size(1).build()?;
    let mut c = db.connect();
    let interrupt = c.interrupt_handle();
    let mut delivered = 0;
    assert!(matches!(
        c.query_batches("SELECT * FROM range(10)", |_, _| {
            delivered += 1;
            interrupt.interrupt();
            Ok(StreamControl::Continue)
        }),
        Err(Error::Interrupted)
    ));
    assert_eq!(delivered, 1);
    assert_eq!(c.query("SELECT 7")?.rows, vec![ints(&[7])]);
    c.execute("CREATE TABLE t(i INTEGER); BEGIN; INSERT INTO t VALUES (1)")?;
    assert!(matches!(
        c.query_batches("SELECT * FROM t", |_, _| Err(Error::Execution(
            "consumer failed".into()
        ))),
        Err(Error::Execution(_))
    ));
    assert!(c.query("SELECT * FROM t").is_err());
    c.execute("ROLLBACK")?;
    assert!(c.query("SELECT * FROM t")?.rows.is_empty());
    c.execute("BEGIN; INSERT INTO t VALUES (2)")?;
    assert!(
        c.query_batches("DELETE FROM t", |_, _| Ok(StreamControl::Continue))
            .is_err()
    );
    assert_eq!(c.query("SELECT * FROM t")?.rows, vec![ints(&[2])]);
    c.query_batches("SELECT * FROM t", |_, _| Ok(StreamControl::Stop))?;
    c.execute("COMMIT")?;
    let prepared = c.prepare("SELECT i+$1 AS value FROM t")?;
    let mut rows = Vec::new();
    c.execute_prepared_batches(&prepared, &ints(&[8]), |_, batch| {
        rows.extend(batch.rows());
        Ok(StreamControl::Continue)
    })?;
    assert_eq!(rows, vec![ints(&[10])]);
    Ok(())
}

#[test]
fn evaluation_failure_never_reports_a_partial_result_as_success() -> Result<()> {
    for (executor, expected_prefix) in [
        (Arc::new(PullExecutor) as Arc<dyn Executor>, 2),
        (Arc::new(MaterializingExecutor), 0),
    ] {
        let mut c = DatabaseBuilder::new()
            .executor(executor)
            .batch_size(1)
            .build()?
            .connect();
        let mut delivered = 0;
        let result = c.query_batches(
            "SELECT CASE WHEN range=2 THEN CAST('bad' AS BIGINT) ELSE range END FROM range(4)",
            |_, batch| {
                delivered += batch.len();
                Ok(StreamControl::Continue)
            },
        );
        assert!(matches!(result, Err(Error::Conversion(_))));
        assert_eq!(delivered, expected_prefix);
        assert_eq!(c.query("SELECT 1")?.rows, vec![ints(&[1])]);
    }
    Ok(())
}

struct InvalidScan(usize);
impl duckdb_rust::storage::scan::TableScan for InvalidScan {
    fn next(
        &mut self,
        _: usize,
        _: &QueryContext,
    ) -> Result<Option<duckdb_rust::storage::scan::ScanBatch>> {
        let rows = (0..self.0)
            .map(|id| ints(&[id as i128]))
            .collect::<Vec<_>>();
        duckdb_rust::storage::scan::ScanBatch::new(
            (0..self.0 as u64).collect(),
            DataChunk::from_rows(&[DataType::BigInt], &rows)?,
        )
        .map(Some)
    }
}

#[test]
fn table_scan_boundary_rejects_empty_and_oversized_batches() -> Result<()> {
    let query = QueryContext::background();
    for size in [0, 3] {
        assert!(matches!(
            duckdb_rust::storage::scan::next_batch(&mut InvalidScan(size), 2, &query),
            Err(Error::Internal(_))
        ));
    }
    Ok(())
}

#[test]
fn separate_streams_have_independent_progress_and_per_call_demand() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let query = QueryContext::background();
    let planner = NativePhysicalPlanner::default();
    let context = ExecutionContext {
        transaction: tx.as_ref(),
        expressions: &ScalarEvaluator,
        subquery_plans: &duckdb_rust::execution::subquery::PreparedSubqueries::new(&planner),
        subqueries: &duckdb_rust::execution::subquery::StreamingSubqueries,
        outer: None,
        recursive: None,
        query: &query,
    };
    let plan = NativePhysicalPlanner::default().plan(&LogicalPlan {
        schema: vec![Field::new("i", DataType::BigInt)],
        node: PlanNode::Range {
            start: 0,
            end: 10,
            step: 1,
        },
    })?;
    let mut first = stream::open(plan.as_ref(), &context)?;
    let mut second = stream::open(plan.as_ref(), &context)?;
    assert_eq!(
        first.next(1)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[0])]
    );
    assert_eq!(
        first.next(3)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[1]), ints(&[2]), ints(&[3])]
    );
    assert_eq!(
        second.next(2)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[0]), ints(&[1])]
    );
    drop(first);
    assert_eq!(second.next(20)?.unwrap().len(), 8);
    assert!(second.next(1)?.is_none());
    assert!(second.next(0)?.is_none());
    Ok(())
}

#[derive(Debug)]
struct InvalidOperator {
    schema: Schema,
    kind: usize,
}
struct InvalidStream(usize);
impl BatchStream for InvalidStream {
    fn next(&mut self, requested: usize) -> Result<Option<DataChunk>> {
        match self.0 {
            0 => DataChunk::from_rows(&[DataType::Integer], &[]).map(Some),
            1 => DataChunk::from_rows(&[DataType::Integer], &vec![ints(&[1]); requested + 1])
                .map(Some),
            _ => DataChunk::from_rows(&[DataType::Varchar], &[vec![Value::Varchar("bad".into())]])
                .map(Some),
        }
    }
}
impl PhysicalOperator for InvalidOperator {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn delivery(&self) -> DeliveryMode {
        DeliveryMode::Incremental
    }
    fn open<'a>(&'a self, _: &'a ExecutionContext<'a>) -> Result<Stream<'a>> {
        Ok(Box::new(InvalidStream(self.kind)))
    }
}

#[test]
fn operator_boundary_rejects_invalid_adapters_and_fuses_errors() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let query = QueryContext::new(InterruptHandle::default(), None, 2, 100)?;
    let planner = NativePhysicalPlanner::default();
    let context = ExecutionContext {
        transaction: tx.as_ref(),
        expressions: &ScalarEvaluator,
        subquery_plans: &duckdb_rust::execution::subquery::PreparedSubqueries::new(&planner),
        subqueries: &duckdb_rust::execution::subquery::StreamingSubqueries,
        outer: None,
        recursive: None,
        query: &query,
    };
    for kind in 0..3 {
        let operator = InvalidOperator {
            schema: vec![Field::new("i", DataType::Integer)],
            kind,
        };
        let mut input = stream::open(&operator, &context)?;
        assert!(matches!(input.next(2), Err(Error::Internal(_))));
        assert!(input.next(2)?.is_none());
        for executor in executors() {
            assert!(matches!(
                executor.execute(&operator, &context, &mut |_| panic!(
                    "invalid chunk reached consumer"
                )),
                Err(Error::Internal(_))
            ));
        }
    }
    Ok(())
}

struct BrokenScheduler(bool);
impl Scheduler for BrokenScheduler {
    fn name(&self) -> &'static str {
        "broken-test-scheduler"
    }
    fn run(&self, _: &QueryContext, task: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        if self.0 {
            task()?;
            task()?;
        }
        Ok(())
    }
}

#[test]
fn scheduler_must_run_each_statement_exactly_once_before_commit() -> Result<()> {
    for double in [false, true] {
        let manager = Arc::new(SnapshotTransactions::new(Arc::new(MemoryDurability))?);
        let writer = DatabaseBuilder::new()
            .transactions(manager.clone())
            .build()?;
        writer.connect().execute("CREATE TABLE t(i INTEGER)")?;
        let mut c = DatabaseBuilder::new()
            .transactions(manager)
            .scheduler(Arc::new(BrokenScheduler(double)))
            .build()?
            .connect();
        assert!(matches!(
            c.execute("INSERT INTO t VALUES (1)"),
            Err(Error::Internal(_))
        ));
        assert!(writer.connect().query("SELECT * FROM t")?.rows.is_empty());
    }
    Ok(())
}
