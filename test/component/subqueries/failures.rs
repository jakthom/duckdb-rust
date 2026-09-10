use super::*;
use duckdb_rust::{
    execution::{
        ExecutionContext,
        physical_plan::{NativePhysicalPlanner, PhysicalOperator, PhysicalPlanner},
        subquery::SubqueryRequest,
    },
    parallel::InterruptHandle,
    planner::LogicalPlan,
};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct CountingPlanner(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PhysicalPlanner for CountingPlanner {
    fn name(&self) -> &'static str {
        "counting-test-planner"
    }
    fn plan(&self, logical: &LogicalPlan) -> Result<Arc<dyn PhysicalOperator>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        NativePhysicalPlanner::default().plan(logical)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn physical_subplans_are_prepared_once_without_reusing_correlated_results() -> Result<()> {
    for subqueries in adapters() {
        let calls = Arc::new(AtomicUsize::new(0));
        let db = DatabaseBuilder::new()
            .subqueries(subqueries)
            .physical_planner(Arc::new(CountingPlanner(calls.clone())))
            .build()?;
        let mut connection = db.connect();
        let statement = connection
            .prepare("SELECT (SELECT(SELECT t.i+$1)) v FROM range(100) t(i) ORDER BY i")?;
        for offset in [0, 100] {
            let result = connection.execute_prepared(&statement, &[Value::Integer(offset)])?;
            assert_eq!(calls.swap(0, Ordering::SeqCst), 3);
            assert_eq!(
                result.rows,
                (0..100)
                    .map(|i| vec![Value::Integer(i + offset)])
                    .collect::<Vec<_>>()
            );
        }
        connection.query("SELECT CASE WHEN false THEN (SELECT 1) ELSE 2 END")?;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "unused subplans are not prepared"
        );
    }
    Ok(())
}

struct InvalidResult {
    value: Value,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SubqueryExecutor for InvalidResult {
    fn name(&self) -> &'static str {
        "invalid-test-subquery"
    }
    fn evaluate(
        &self,
        _: &dyn PhysicalOperator,
        _: SubqueryRequest<'_>,
        _: &ExecutionContext<'_>,
    ) -> Result<Value> {
        if let Some(interrupt) = self.interrupt.lock().unwrap().as_ref() {
            interrupt.interrupt();
        }
        Ok(self.value.clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn malformed_subquery_values_and_late_cancellation_remain_errors() -> Result<()> {
    for (value, sql) in [
        (
            Value::Varchar("bad adapter".into()),
            "SELECT TRY_CAST((SELECT 1) AS INTEGER)",
        ),
        (Value::Null, "SELECT EXISTS(SELECT 1)"),
        (Value::Integer(2), "SELECT 1 IN(SELECT 1)"),
    ] {
        let db = DatabaseBuilder::new()
            .subqueries(Arc::new(InvalidResult {
                value,
                interrupt: Arc::new(Mutex::new(None)),
            }))
            .build()?;
        assert!(
            matches!(db.connect().query(sql), Err(Error::Internal(_))),
            "{sql}"
        );
    }
    let interrupt = Arc::new(Mutex::new(None));
    let db = DatabaseBuilder::new()
        .subqueries(Arc::new(InvalidResult {
            value: Value::Integer(1),
            interrupt: interrupt.clone(),
        }))
        .build()?;
    let mut connection = db.connect();
    *interrupt.lock().unwrap() = Some(connection.interrupt_handle());
    assert!(matches!(
        connection.query("SELECT TRY_CAST((SELECT 1) AS INTEGER)"),
        Err(Error::Interrupted)
    ));
    *interrupt.lock().unwrap() = None;
    assert_eq!(
        connection.query("SELECT (SELECT 1)")?.rows,
        vec![vec![Value::Integer(1)]]
    );
    Ok(())
}

struct CancelDuringScan {
    calls: Arc<AtomicUsize>,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for CancelDuringScan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CancelDuringScan")
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::function::ScalarFunction for CancelDuringScan {
    fn name(&self) -> &str {
        "cancel_during_scan"
    }
    fn effects(&self) -> duckdb_rust::function::FunctionEffects {
        duckdb_rust::function::FunctionEffects {
            volatile: true,
            external_access: false,
        }
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments.len() != 1 {
            return Err(Error::Bind("one argument required".into()));
        }
        Ok(arguments[0].clone())
    }
    fn evaluate(
        &self,
        arguments: &[Value],
        _: &duckdb_rust::parallel::QueryContext,
    ) -> Result<Value> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 4 {
            self.interrupt.lock().unwrap().as_ref().unwrap().interrupt();
        }
        Ok(arguments[0].clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn both_subquery_adapters_cancel_during_nested_input_and_leave_connection_usable() -> Result<()> {
    for subqueries in adapters() {
        let interrupt = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = duckdb_rust::function::FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(CancelDuringScan {
            calls: calls.clone(),
            interrupt: interrupt.clone(),
        }))?;
        let db = DatabaseBuilder::new()
            .subqueries(subqueries)
            .functions(functions)
            .batch_size(1)
            .build()?;
        let mut connection = db.connect();
        connection.execute("CREATE TABLE t AS SELECT i FROM range(1000) t(i)")?;
        *interrupt.lock().unwrap() = Some(connection.interrupt_handle());
        for sql in [
            "SELECT 999 IN(SELECT cancel_during_scan(i) FROM range(1000) t(i))",
            "SELECT EXISTS(SELECT 1 FROM t WHERE cancel_during_scan(i)=999)",
        ] {
            assert!(matches!(connection.query(sql), Err(Error::Interrupted)));
            assert_eq!(calls.swap(0, Ordering::SeqCst), 5);
            assert_eq!(
                connection.query("SELECT (SELECT 7)")?.rows,
                vec![vec![Value::Integer(7)]]
            );
        }
    }
    Ok(())
}
