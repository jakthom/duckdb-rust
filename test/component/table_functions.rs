use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    common::vector::DataChunk,
    function::{
        FunctionRegistry,
        table::{
            BoundTableFunction, TableFunction, TableFunctionArgument, TableFunctionBind,
            TableFunctionBindContext, TableFunctionState,
        },
    },
    parallel::{InterruptHandle, QueryContext},
    planner::Field,
    storage::table_function::TableFunctionScan,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ints(values: &[i128]) -> Vec<Vec<Value>> {
    values
        .iter()
        .map(|value| vec![Value::Integer(*value)])
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn integer_range_uses_registered_lifecycle_and_development_boundaries() -> Result<()> {
    let mut connection = DatabaseBuilder::new().batch_size(2).build()?.connect();
    assert_eq!(
        connection.query("SELECT * FROM range(4,15,6)")?.rows,
        ints(&[4, 10])
    );
    assert_eq!(connection.query("CALL range(3)")?.rows, ints(&[0, 1, 2]));
    assert_eq!(
        connection.query("SELECT * FROM range(-4,-15,-6)")?.rows,
        ints(&[-4, -10])
    );
    assert_eq!(
        connection
            .query("SELECT * FROM generate_series(5,1,-1) LIMIT 4")?
            .rows,
        ints(&[5, 4, 3, 2])
    );
    assert_eq!(
        connection
            .query("SELECT * FROM generate_series(0,9223372036854775807,9223372036854775807)")?
            .rows,
        ints(&[0, i64::MAX as i128])
    );
    assert_eq!(
        connection
            .query("SELECT * FROM generate_series(0,-9223372036854775808,-9223372036854775808)")?
            .rows,
        ints(&[0, i64::MIN as i128])
    );
    assert!(
        connection
            .query("SELECT * FROM range(1,NULL,0)")?
            .rows
            .is_empty()
    );
    for sql in [
        "SELECT * FROM range(NULL, TRUE)",
        "SELECT * FROM range(NULL, 'hello')",
        "SELECT * FROM range(NULL, 1.5)",
        "SELECT * FROM range(NULL, 1::HUGEINT)",
    ] {
        assert!(
            matches!(connection.query(sql), Err(Error::Bind(_))),
            "{sql}"
        );
    }
    assert!(
        connection
            .query("SELECT * FROM range(NULL::BIGINT, 1::INTEGER)")?
            .rows
            .is_empty()
    );
    assert_eq!(
        connection
            .query("SELECT * FROM range(CAST(2.0 AS BIGINT))")?
            .rows,
        ints(&[0, 1])
    );
    assert_eq!(
        connection.query("SELECT * FROM main.\"range\"(2)")?.rows,
        ints(&[0, 1])
    );
    assert!(
        connection
            .query("SELECT * FROM range(0,10,-1)")?
            .rows
            .is_empty()
    );
    assert!(matches!(
        connection.query("SELECT * FROM range(0,10,0)"),
        Err(Error::Bind(_))
    ));
    assert!(connection.query("SELECT * FROM range('hello')").is_err());
    assert!(connection.query("SELECT * FROM range(count := 2)").is_err());
    assert!(connection.query("SELECT range(2)").is_err());
    assert!(
        connection
            .query("SELECT * FROM range(1)t(i), range(i)")
            .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn prepared_range_reopens_fresh_state_for_cached_and_parameterized_plans() -> Result<()> {
    let mut connection = DatabaseBuilder::new().batch_size(1).build()?.connect();
    let cached = connection.prepare("SELECT * FROM range(4)")?;
    for _ in 0..3 {
        assert_eq!(
            connection.execute_prepared(&cached, &[])?.rows,
            ints(&[0, 1, 2, 3])
        );
    }
    let parameterized = connection.prepare("SELECT * FROM generate_series($1,$2,$3)")?;
    assert_eq!(
        connection
            .execute_prepared(
                &parameterized,
                &[Value::Integer(3), Value::Integer(1), Value::Integer(-1)],
            )?
            .rows,
        ints(&[3, 2, 1])
    );
    assert_eq!(
        connection
            .execute_prepared(
                &parameterized,
                &[Value::Integer(1), Value::Integer(3), Value::Integer(1)],
            )?
            .rows,
        ints(&[1, 2, 3])
    );
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum ProbeMode {
    Rows,
    Failure,
    CleanupFailure,
    FailureCleanupFailure,
    EmptyChunk,
    Oversized,
    WrongWidth,
    WrongType,
}

#[derive(Debug, Default)]
struct ProbeCounts {
    binds: AtomicUsize,
    opens: AtomicUsize,
    scans: AtomicUsize,
    cleanups: AtomicUsize,
}

struct Probe {
    name: String,
    mode: ProbeMode,
    counts: Arc<ProbeCounts>,
    interrupt_on_init: Option<InterruptHandle>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for Probe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Probe")
            .field("name", &self.name)
            .field("mode", &self.mode)
            .field("counts", &self.counts)
            .field("interrupt_on_init", &self.interrupt_on_init.is_some())
            .finish()
    }
}

#[derive(Debug)]
struct ProbeBind {
    rows: usize,
}

#[derive(Debug)]
struct ProbeState {
    position: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Probe {
    fn new(name: impl Into<String>, mode: ProbeMode) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            mode,
            counts: Arc::new(ProbeCounts::default()),
            interrupt_on_init: None,
        })
    }

    fn with_init_interrupt(name: impl Into<String>, interrupt: InterruptHandle) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            mode: ProbeMode::Rows,
            counts: Arc::new(ProbeCounts::default()),
            interrupt_on_init: Some(interrupt),
        })
    }

    fn bound(self: &Arc<Self>, rows: i128, query: &QueryContext) -> Result<BoundTableFunction> {
        let function: Arc<dyn TableFunction> = self.clone();
        let bind = function.bind(
            &[TableFunctionArgument {
                name: None,
                data_type: DataType::BigInt,
                value: Value::Integer(rows),
            }],
            &TableFunctionBindContext { query },
        )?;
        Ok(BoundTableFunction::new(function, bind))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableFunction for Probe {
    fn name(&self) -> &str {
        &self.name
    }

    fn bind(
        &self,
        arguments: &[TableFunctionArgument],
        context: &TableFunctionBindContext<'_>,
    ) -> Result<TableFunctionBind> {
        context.query.check()?;
        self.counts.binds.fetch_add(1, Ordering::SeqCst);
        let rows = match arguments {
            [] => 2,
            [TableFunctionArgument { name, value, .. }]
                if name
                    .as_deref()
                    .is_none_or(|name| name.eq_ignore_ascii_case("count")) =>
            {
                usize::try_from(value.as_i128()?)
                    .map_err(|_| Error::Bind("custom_source count is out of range".into()))?
            }
            _ => return Err(Error::Bind("custom_source accepts only count".into())),
        };
        Ok(TableFunctionBind::new(
            vec![Field::new("value", DataType::BigInt)],
            ProbeBind { rows },
        ))
    }

    fn init(
        &self,
        _bind: &TableFunctionBind,
        context: &QueryContext,
    ) -> Result<Box<dyn TableFunctionState>> {
        context.check()?;
        self.counts.opens.fetch_add(1, Ordering::SeqCst);
        if let Some(interrupt) = &self.interrupt_on_init {
            interrupt.interrupt();
        }
        Ok(Box::new(ProbeState { position: 0 }))
    }

    fn scan(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<DataChunk>> {
        context.check()?;
        self.counts.scans.fetch_add(1, Ordering::SeqCst);
        if matches!(self.mode, ProbeMode::Failure | ProbeMode::FailureCleanupFailure) {
            return Err(Error::Execution("custom source failed".into()));
        }
        let bind = bind
            .data()
            .downcast_ref::<ProbeBind>()
            .ok_or_else(|| Error::Internal("custom bind type mismatch".into()))?;
        let state = state
            .downcast_mut::<ProbeState>()
            .ok_or_else(|| Error::Internal("custom state type mismatch".into()))?;
        if state.position >= bind.rows {
            return Ok(None);
        }
        match self.mode {
            ProbeMode::EmptyChunk => DataChunk::from_rows(&[DataType::BigInt], &[]).map(Some),
            ProbeMode::Oversized => {
                let values = (0..=max_rows)
                    .map(|value| vec![Value::Integer(value as i128)])
                    .collect::<Vec<_>>();
                DataChunk::from_rows(&[DataType::BigInt], &values).map(Some)
            }
            ProbeMode::WrongWidth => DataChunk::new(vec![], 1).map(Some),
            ProbeMode::WrongType => {
                DataChunk::from_rows(&[DataType::Varchar], &[vec![Value::Varchar("bad".into())]])
                    .map(Some)
            }
            ProbeMode::Rows | ProbeMode::CleanupFailure => {
                let count = max_rows.min(bind.rows - state.position);
                let rows = (state.position..state.position + count)
                    .map(|value| vec![Value::Integer(value as i128)])
                    .collect::<Vec<_>>();
                state.position += count;
                DataChunk::from_rows(&[DataType::BigInt], &rows).map(Some)
            }
            ProbeMode::Failure | ProbeMode::FailureCleanupFailure => unreachable!(),
        }
    }

    fn cleanup(
        &self,
        _bind: &TableFunctionBind,
        _state: Box<dyn TableFunctionState>,
        _context: &QueryContext,
    ) -> Result<()> {
        self.counts.cleanups.fetch_add(1, Ordering::SeqCst);
        if matches!(
            self.mode,
            ProbeMode::CleanupFailure | ProbeMode::FailureCleanupFailure
        ) {
            Err(Error::Execution("custom cleanup failed".into()))
        } else {
            Ok(())
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn database_with(probe: Arc<Probe>) -> Result<duckdb_rust::Database> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_table(probe)?;
    DatabaseBuilder::new()
        .functions(functions)
        .batch_size(2)
        .build()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn custom_registration_retains_named_defaults_schema_and_prepared_reuse() -> Result<()> {
    let probe = Probe::new("custom_source", ProbeMode::Rows);
    let mut connection = database_with(probe.clone())?.connect();
    assert_eq!(
        connection.query("FROM custom_source()")?.rows,
        ints(&[0, 1])
    );
    assert_eq!(
        connection
            .query("SELECT value FROM custom_source(count := 3)")?
            .rows,
        ints(&[0, 1, 2])
    );
    assert!(matches!(
        connection.query("SELECT custom_source()"),
        Err(Error::Bind(message)) if message.contains("FROM")
    ));
    assert!(
        connection
            .query("SELECT * FROM range(1)t(i), custom_source(count := i)")
            .is_err()
    );

    let prepared = connection.prepare("SELECT * FROM custom_source(count := 3)")?;
    for _ in 0..2 {
        assert_eq!(
            connection.execute_prepared(&prepared, &[])?.rows,
            ints(&[0, 1, 2])
        );
    }
    assert_eq!(probe.counts.binds.load(Ordering::SeqCst), 3);
    assert_eq!(probe.counts.opens.load(Ordering::SeqCst), 4);
    assert_eq!(probe.counts.cleanups.load(Ordering::SeqCst), 4);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn quoted_table_function_names_use_identifier_values() -> Result<()> {
    let probe = Probe::new("Custom_Source", ProbeMode::Rows);
    let mut connection = database_with(probe)?.connect();
    assert_eq!(
        connection.query("SELECT * FROM \"Custom_Source\"(1)")?.rows,
        ints(&[0])
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn cleanup_is_once_only_on_eof_limit_failure_and_invalid_output() -> Result<()> {
    for (mode, sql) in [
        (ProbeMode::Rows, "SELECT * FROM custom_source(3)"),
        (ProbeMode::Rows, "SELECT * FROM custom_source(3) LIMIT 1"),
        (ProbeMode::Failure, "SELECT * FROM custom_source(3)"),
        (ProbeMode::EmptyChunk, "SELECT * FROM custom_source(3)"),
        (ProbeMode::Oversized, "SELECT * FROM custom_source(3)"),
        (ProbeMode::WrongWidth, "SELECT * FROM custom_source(3)"),
        (ProbeMode::WrongType, "SELECT * FROM custom_source(3)"),
    ] {
        let probe = Probe::new("custom_source", mode);
        let result = database_with(probe.clone())?.connect().query(sql);
        if matches!(mode, ProbeMode::Rows) {
            assert!(result.is_ok(), "{mode:?}: {result:?}");
        } else {
            assert!(result.is_err(), "{mode:?}");
        }
        assert_eq!(probe.counts.opens.load(Ordering::SeqCst), 1, "{mode:?}");
        assert_eq!(probe.counts.cleanups.load(Ordering::SeqCst), 1, "{mode:?}");
    }

    let cleanup_failure = Probe::new("custom_source", ProbeMode::CleanupFailure);
    assert!(matches!(
        database_with(cleanup_failure.clone())?
            .connect()
            .query("SELECT * FROM custom_source(1)"),
        Err(Error::Execution(message)) if message == "custom cleanup failed"
    ));
    assert_eq!(
        cleanup_failure.counts.cleanups.load(Ordering::SeqCst),
        1
    );

    let dual_failure = Probe::new("custom_source", ProbeMode::FailureCleanupFailure);
    assert!(matches!(
        database_with(dual_failure.clone())?
            .connect()
            .query("SELECT * FROM custom_source(1)"),
        Err(Error::Execution(message)) if message == "custom source failed"
    ));
    assert_eq!(dual_failure.counts.cleanups.load(Ordering::SeqCst), 1);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn cancellation_and_drop_finish_state_before_repeated_next() -> Result<()> {
    let interrupt = InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2, 100)?;
    let probe = Probe::new("cancel_source", ProbeMode::Rows);
    let bound = probe.bound(5, &query)?;

    let mut cancelled_before_next = TableFunctionScan::open(&bound, &query)?;
    interrupt.interrupt();
    assert!(matches!(
        cancelled_before_next.next(1),
        Err(Error::Interrupted)
    ));
    assert!(cancelled_before_next.next(1)?.is_none());
    drop(cancelled_before_next);
    assert_eq!(probe.counts.cleanups.load(Ordering::SeqCst), 1);

    interrupt.reset();
    let mut scan = TableFunctionScan::open(&bound, &query)?;
    assert_eq!(scan.next(1)?.unwrap().len(), 1);
    interrupt.interrupt();
    assert!(matches!(scan.next(1), Err(Error::Interrupted)));
    assert!(scan.next(1)?.is_none());
    drop(scan);
    assert_eq!(probe.counts.cleanups.load(Ordering::SeqCst), 2);

    interrupt.reset();
    let mut dropped = TableFunctionScan::open(&bound, &query)?;
    assert_eq!(dropped.next(1)?.unwrap().len(), 1);
    drop(dropped);
    assert_eq!(probe.counts.cleanups.load(Ordering::SeqCst), 3);

    let init_interrupt = InterruptHandle::default();
    let init_query = QueryContext::new(init_interrupt.clone(), None, 2, 100)?;
    let init_probe = Probe::with_init_interrupt("init_cancel", init_interrupt);
    let init_bound = init_probe.bound(1, &init_query)?;
    assert!(matches!(
        TableFunctionScan::open(&init_bound, &init_query),
        Err(Error::Interrupted)
    ));
    assert_eq!(init_probe.counts.cleanups.load(Ordering::SeqCst), 1);
    Ok(())
}
