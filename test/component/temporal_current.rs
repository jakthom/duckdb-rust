use super::*;
use duckdb_rust::{
    TransactionClock,
    catalog::{TableName, expression::StoredExpressionKind},
    function::FunctionRegistry,
    parallel::{InterruptHandle, QueryContext},
};

#[derive(Debug)]
struct StepClock {
    first: i64,
    calls: AtomicUsize,
}

impl StepClock {
    fn new(first: i64) -> Self {
        Self {
            first,
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TransactionClock for StepClock {
    fn name(&self) -> &'static str {
        "step-clock"
    }

    fn timestamp_micros(&self, query: &QueryContext) -> Result<i64> {
        query.check()?;
        let step = self.calls.fetch_add(1, Ordering::SeqCst);
        let step =
            i64::try_from(step).map_err(|_| Error::OutOfRange("test transaction clock".into()))?;
        self.first
            .checked_add(step)
            .ok_or_else(|| Error::OutOfRange("test transaction clock".into()))
    }
}

fn timestamp(micros: i64) -> Value {
    Value::Temporal(TemporalValue::TimestampTz(micros))
}

fn timestamps(row: &[Value], micros: i64) -> bool {
    row.iter().all(|value| value == &timestamp(micros))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn current_timestamp_functions_share_transaction_start_and_prepare_does_not_sample() -> Result<()> {
    let clock = Arc::new(StepClock::new(100));
    let database = DatabaseBuilder::new()
        .transaction_clock(clock.clone())
        .build()?;
    assert!(
        database
            .adapters()
            .contains(&("transaction-clock", "step-clock"))
    );
    let mut connection = database.connect();

    let prepared = connection.prepare(
        "SELECT get_current_timestamp(),now(),transaction_timestamp(),current_timestamp FROM range(3)",
    )?;
    assert_eq!(clock.calls(), 0);
    let result = connection.execute_prepared(&prepared, &[])?;
    assert_eq!(clock.calls(), 1);
    assert_eq!(result.rows.len(), 3);
    assert!(result.rows.iter().all(|row| timestamps(row, 100)));
    assert!(
        result
            .columns
            .iter()
            .all(|column| column.data_type == DataType::TimestampTz)
    );

    assert_eq!(
        connection.query("SELECT now(),current_timestamp")?.rows[0],
        vec![timestamp(101), timestamp(101)]
    );
    assert_eq!(clock.calls(), 2);

    connection.execute("BEGIN")?;
    assert_eq!(clock.calls(), 3);
    for sql in [
        "SELECT current_timestamp,now()",
        "SELECT get_current_timestamp(),transaction_timestamp()",
    ] {
        assert!(timestamps(&connection.query(sql)?.rows[0], 102));
        assert_eq!(clock.calls(), 3);
    }
    connection.execute("COMMIT")?;
    assert_eq!(clock.calls(), 3);
    assert_eq!(
        connection.query("SELECT current_timestamp")?.rows[0][0],
        timestamp(103)
    );
    assert_eq!(clock.calls(), 4);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn current_timestamp_column_and_alias_precedence_leave_parenthesized_call_unregistered()
-> Result<()> {
    let clock = Arc::new(StepClock::new(200));
    let mut connection = DatabaseBuilder::new()
        .transaction_clock(clock.clone())
        .build()?
        .connect();

    assert_eq!(
        connection
            .query(
                "SELECT current_timestamp FROM (VALUES (17),(18)) AS t(current_timestamp) ORDER BY current_timestamp",
            )?
            .rows,
        vec![vec![Value::Integer(17)], vec![Value::Integer(18)]]
    );
    assert_eq!(
        connection
            .query("SELECT value AS current_timestamp FROM (VALUES (2),(1)) t(value) ORDER BY current_timestamp")?
            .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert!(matches!(
        connection.query("SELECT CURRENT_TIMESTAMP()"),
        Err(Error::Catalog(_))
    ));
    // All executable statements own a logical transaction even when a column
    // wins or binding subsequently reports a catalog error.
    assert_eq!(clock.calls(), 3);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn retained_current_timestamp_defaults_evaluate_once_per_transaction_not_per_row() -> Result<()> {
    let clock = Arc::new(StepClock::new(1_000));
    let database = DatabaseBuilder::new()
        .batch_size(2)
        .transaction_clock(clock.clone())
        .build()?;
    let mut connection = database.connect();
    connection.execute("CREATE TABLE t(id BIGINT, ts TIMESTAMPTZ DEFAULT current_timestamp)")?;
    let definition = connection.resolve_table(&TableName::main("t"))?;
    assert!(matches!(
        definition.definition().columns[1]
            .default
            .as_ref()
            .map(|expression| &expression.kind),
        Some(StoredExpressionKind::CurrentTimestamp)
    ));
    assert_eq!(clock.calls(), 1);

    connection.execute("INSERT INTO t(id) SELECT * FROM range(5)")?;
    assert_eq!(clock.calls(), 2);
    connection.execute("BEGIN")?;
    connection.execute("INSERT INTO t(id) VALUES (10),(11)")?;
    connection.execute("INSERT INTO t(id) VALUES (12)")?;
    connection.execute("COMMIT")?;
    assert_eq!(clock.calls(), 3);

    let rows = connection.query("SELECT id,ts FROM t ORDER BY id")?.rows;
    assert_eq!(clock.calls(), 4);
    assert_eq!(rows.len(), 8);
    for row in &rows {
        let expected = if row[0].as_i128()? < 5 { 1_001 } else { 1_002 };
        assert_eq!(row[1], timestamp(expected));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn private_checkpoint_retains_current_timestamp_syntax_for_future_transactions() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("current-timestamp.snapshot");
    let open = |clock: Arc<StepClock>| {
        DatabaseBuilder::new()
            .transaction_clock(clock)
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()
    };

    let first = Arc::new(StepClock::new(2_000));
    {
        let mut connection = open(first.clone())?.connect();
        connection
            .execute("CREATE TABLE t(id INTEGER, ts TIMESTAMPTZ DEFAULT current_timestamp)")?;
        connection.execute("INSERT INTO t(id) VALUES (1)")?;
        assert_eq!(first.calls(), 2);
    }

    let second = Arc::new(StepClock::new(3_000));
    {
        let mut connection = open(second.clone())?.connect();
        let definition = connection.resolve_table(&TableName::main("t"))?;
        assert!(matches!(
            definition.definition().columns[1]
                .default
                .as_ref()
                .map(|expression| &expression.kind),
            Some(StoredExpressionKind::CurrentTimestamp)
        ));
        assert_eq!(second.calls(), 0);
        connection.execute("INSERT INTO t(id) VALUES (2)")?;
        assert_eq!(second.calls(), 1);
        assert_eq!(
            connection.query("SELECT id,ts FROM t ORDER BY id")?.rows,
            vec![
                vec![Value::Integer(1), timestamp(2_001)],
                vec![Value::Integer(2), timestamp(3_000)],
            ]
        );
    }
    Ok(())
}

#[derive(Debug)]
struct InterruptedClock;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TransactionClock for InterruptedClock {
    fn name(&self) -> &'static str {
        "interrupted-clock"
    }

    fn timestamp_micros(&self, query: &QueryContext) -> Result<i64> {
        query.check()?;
        Err(Error::Interrupted)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn current_timestamp_requires_runtime_capability_and_preserves_cancellation() -> Result<()> {
    let function = FunctionRegistry::builtins().scalar("get_current_timestamp")?;
    assert!(matches!(
        function.evaluate(&[], &QueryContext::background()),
        Err(Error::Unsupported(_))
    ));

    let handle = InterruptHandle::default();
    let query = QueryContext::new(handle.clone(), None, 1, 1)?.with_transaction_timestamp(7);
    handle.interrupt();
    assert!(matches!(
        function.evaluate(&[], &query),
        Err(Error::Interrupted)
    ));

    let mut connection = DatabaseBuilder::new()
        .transaction_clock(Arc::new(InterruptedClock))
        .build()?
        .connect();
    assert!(matches!(
        connection.query("SELECT now()"),
        Err(Error::Interrupted)
    ));
    Ok(())
}
