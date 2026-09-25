use super::*;
use duckdb_rust::{
    common::cast::{CastFunction, CastMode, CastRegistry, CastSpec},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::{InterruptHandle, QueryContext},
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn text_rows(result: duckdb_rust::QueryResult) -> Vec<Vec<String>> {
    result
        .rows
        .into_iter()
        .map(|row| row.into_iter().map(|value| value.to_string()).collect())
        .collect()
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixed_offset_timezone_matches_core_domains_and_relocation() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut connection = DatabaseBuilder::new()
                .batch_size(2)
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            assert_eq!(
                text_rows(connection.query(
                    "SELECT timezone(TIMESTAMP '2000-01-01'),
                            timezone(TIME '01:02:03'),
                            timezone(TIME_NS '01:02:03.123456789'),
                            timezone(z),timezone_hour(z),timezone_minute(z)
                     FROM (VALUES
                         (TIMETZ '01:02:03+05:30'),
                         (TIMETZ '01:02:03-05:30'),
                         (TIMETZ '01:02:03+15:59:59'),
                         (TIMETZ '01:02:03-15:59:59')) rows(z)"
                )?),
                vec![
                    vec!["0", "0", "0", "19800", "5", "30"],
                    vec!["0", "0", "0", "-19800", "-5", "-30"],
                    vec!["0", "0", "0", "57599", "15", "59"],
                    vec!["0", "0", "0", "-57599", "-15", "-59"],
                ]
            );
            assert_eq!(
                text_rows(connection.query(
                    "SELECT
                        date_part('timezone',TIMETZ '01:02:03+05:30'),
                        datepart('timezone_hour',TIMETZ '01:02:03-05:30'),
                        date_part('timezone_minute',TIMETZ '01:02:03-05:30'),
                        datepart('timezone',TIMESTAMP '2000-01-01')"
                )?),
                vec![vec!["19800", "-5", "-30", "0"]]
            );
            assert_eq!(
                text_rows(connection.query(
                    "SELECT
                        timezone(INTERVAL '2 hours', TIMETZ '10:00:00+03'),
                        timezone(INTERVAL '-5 hours 30 minutes', TIMETZ '01:00:00+14'),
                        timezone(INTERVAL '1 day 00:00:00.123456', TIMETZ '23:59:59.999999+00'),
                        timezone(INTERVAL '1 month', TIMETZ '00:00:00+00'),
                        timezone(INTERVAL '2 hours 00:00:00.500001', TIMETZ '10:00:00+03'),
                        timezone(INTERVAL '15 hours 59 minutes 59.999999 seconds',TIMETZ '00:00:00+00'),
                        timezone(INTERVAL '-15 hours -59 minutes -59.999999 seconds',TIMETZ '00:00:00+00')"
                )?),
                vec![vec![
                    "09:00:00+02",
                    "06:30:00-04:30",
                    "00:00:00.123455+00",
                    "00:00:00+00",
                    "09:00:00.500001+02",
                    "15:59:59.999999+15:59:59",
                    "08:00:00.000001-15:59:59",
                ]]
            );
            assert_eq!(
                connection
                    .query(
                        "SELECT
                            timezone(NULL::DATE),timezone(DATE 'infinity'),
                            timezone(NULL::INTERVAL),timezone(NULL::TIME),
                            timezone(NULL::TIMETZ),
                            timezone(INTERVAL '2 hours',NULL::TIMETZ),
                            timezone(NULL::INTERVAL,TIMETZ '10:00:00+03')",
                    )?
                    .rows,
                vec![vec![Value::Null; 7]]
            );
            for name in ["timezone", "timezone_hour", "timezone_minute"] {
                for (domain, unit) in [("DATE 'epoch'", "date"), ("INTERVAL '1 day'", "interval")] {
                    let sql = format!("SELECT {name}({domain})");
                    assert!(
                        matches!(
                            connection.query(&sql),
                            Err(Error::NotImplemented(message))
                                if message == format!("\"{unit}\" units \"{name}\" not recognized")
                        ),
                        "{sql}"
                    );
                }
                assert_eq!(
                    connection
                        .query(&format!("SELECT {name}(DATE 'infinity')"))?
                        .rows,
                    vec![vec![Value::Null]],
                );
                for alias in ["date_part", "datepart"] {
                    for (domain, unit) in
                        [("DATE 'epoch'", "date"), ("INTERVAL '1 day'", "interval")]
                    {
                        let sql = format!("SELECT {alias}('{name}',{domain})");
                        assert!(
                            matches!(
                                connection.query(&sql),
                                Err(Error::NotImplemented(message))
                                    if message == format!("\"{unit}\" units \"{name}\" not recognized")
                            ),
                            "{sql}"
                        );
                    }
                    assert_eq!(
                        connection
                            .query(&format!("SELECT {alias}('{name}',DATE 'infinity')"))?
                            .rows,
                        vec![vec![Value::Null]],
                    );
                }
            }
            for sql in [
                "SELECT timezone(NULL)",
                "SELECT timezone('10:00:00+03')",
                "SELECT timezone(CAST('10:00:00+03' AS VARCHAR))",
            ] {
                assert!(
                    matches!(connection.query(sql), Err(Error::Bind(_))),
                    "{sql}"
                );
            }
            for sql in [
                "SELECT timezone(TIMESTAMPTZ '2000-01-01+00')",
                "SELECT timezone('UTC',TIMESTAMP '2000-01-01')",
                "SELECT timezone('UTC',TIMESTAMPTZ '2000-01-01+00')",
                "SELECT timezone('UTC',TIMETZ '10:00:00+03')",
            ] {
                assert!(
                    matches!(connection.query(sql), Err(Error::Unsupported(_))),
                    "{sql}"
                );
            }
            for offset in ["16 hours", "-16 hours"] {
                let sql = format!("SELECT timezone(INTERVAL '{offset}',TIMETZ '00:00:00+00')");
                assert!(
                    matches!(connection.query(&sql), Err(Error::OutOfRange(_))),
                    "{sql}"
                );
            }
            let prepared = connection.prepare(
                "SELECT timezone($1),timezone_hour($1),timezone_minute($1),timezone($2,$1)",
            )?;
            assert_eq!(
                text_rows(connection.execute_prepared(
                    &prepared,
                    &[
                        Value::Temporal(TemporalValue::TimeTz {
                            micros: 36_000_000_000,
                            offset: 10_800,
                        }),
                        Value::Temporal(TemporalValue::Interval {
                            months: 0,
                            days: 0,
                            micros: 7_200_000_000,
                        }),
                    ],
                )?),
                vec![vec!["10800", "3", "0", "09:00:00+02"]]
            );
        }
    }
    Ok(())
}

struct SelectedTimezoneCast {
    calls: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    handle: Arc<Mutex<Option<InterruptHandle>>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for SelectedTimezoneCast {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SelectedTimezoneCast")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedTimezoneCast {
    fn name(&self) -> &'static str {
        "selected-timezone-cast"
    }

    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::TimestampNs
            && spec.target == DataType::Timestamp
            && spec.mode == CastMode::Implicit
    }

    fn cast(&self, _: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Resource("selected timezone cast failure".into()));
        }
        if self.interrupt.load(Ordering::SeqCst) {
            self.handle
                .lock()
                .unwrap()
                .as_ref()
                .expect("installed interrupt handle")
                .interrupt();
        }
        Ok(Value::Temporal(TemporalValue::Timestamp(0)))
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn selected_timezone_casts_survive_batches_defaults_failures_and_cancellation() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(false));
        let interrupt = Arc::new(AtomicBool::new(false));
        let handle = Arc::new(Mutex::new(None));
        let mut casts = CastRegistry::builtins();
        casts.replace(
            CastSpec {
                source: DataType::TimestampNs,
                target: DataType::Timestamp,
                mode: CastMode::Implicit,
            },
            Arc::new(SelectedTimezoneCast {
                calls: calls.clone(),
                fail: fail.clone(),
                interrupt: interrupt.clone(),
                handle: handle.clone(),
            }),
        )?;
        let mut connection = DatabaseBuilder::new()
            .casts(casts)
            .batch_size(2)
            .expressions(expressions)
            .build()?
            .connect();
        *handle.lock().unwrap() = Some(connection.interrupt_handle());
        assert_eq!(
            connection
                .query(
                    "SELECT timezone(v) FROM (VALUES
                        (TIMESTAMP_NS 'epoch'),(TIMESTAMP_NS '2000-01-01'),
                        (TIMESTAMP_NS '2001-01-01')) rows(v)",
                )?
                .rows,
            vec![vec![Value::Integer(0)]; 3]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        connection.execute(
            "CREATE TABLE selected_timezone(
                id INTEGER PRIMARY KEY,
                value BIGINT DEFAULT timezone(TIMESTAMP_NS 'epoch'))",
        )?;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        connection.execute("INSERT INTO selected_timezone(id) VALUES (1)")?;
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        fail.store(true, Ordering::SeqCst);
        assert!(matches!(
            connection.query("SELECT timezone(TIMESTAMP_NS 'epoch')"),
            Err(Error::Resource(message)) if message == "selected timezone cast failure"
        ));
        fail.store(false, Ordering::SeqCst);
        interrupt.store(true, Ordering::SeqCst);
        assert!(matches!(
            connection.query("SELECT timezone_hour(TIMESTAMP_NS 'epoch')"),
            Err(Error::Interrupted)
        ));
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn timezone_defaults_mutations_and_private_native_reopen_are_atomic() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (format_index, format) in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ]
    .into_iter()
    .enumerate()
    {
        for expressions in [
            Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
            Arc::new(BatchedEvaluator),
        ] {
            let path = directory.path().join(format!(
                "timezone-core-{format_index}-{}.db",
                expressions.name()
            ));
            let open = || {
                DatabaseBuilder::new()
                    .batch_size(2)
                    .expressions(expressions.clone())
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format.clone(),
                    )?))
                    .build()
            };
            {
                let mut connection = open()?.connect();
                connection.execute(
                    "CREATE TABLE timezone_core(
                        id INTEGER PRIMARY KEY,
                        source TIMETZ,
                        shifted TIMETZ DEFAULT timezone(
                            INTERVAL '2 hours',TIMETZ '10:00:00+03'))",
                )?;
                connection.execute(
                    "CREATE TABLE invalid_timezone_default(
                        id INTEGER,
                        shifted TIMETZ DEFAULT timezone(
                            INTERVAL '16 hours',TIMETZ '00:00:00+00'))",
                )?;
                assert!(matches!(
                    connection.execute("INSERT INTO invalid_timezone_default(id) VALUES (1)"),
                    Err(Error::OutOfRange(_))
                ));
                assert_eq!(
                    connection
                        .query("SELECT count(*) FROM invalid_timezone_default")?
                        .rows,
                    vec![vec![Value::Integer(0)]]
                );
                connection.execute(
                    "INSERT INTO timezone_core(id,source) VALUES
                        (1,TIMETZ '01:00:00+05:30'),
                        (2,TIMETZ '02:00:00-05:30')",
                )?;
            }
            let committed;
            {
                let mut connection = open()?.connect();
                let projection = "SELECT id,source,shifted,timezone(source),timezone_hour(source),timezone_minute(source) FROM timezone_core ORDER BY id";
                let before = connection.query(projection)?.rows;
                assert!(matches!(
                    connection.execute(
                        "UPDATE timezone_core SET shifted=timezone(
                            INTERVAL '16 hours',source)"
                    ),
                    Err(Error::OutOfRange(_))
                ));
                assert_eq!(connection.query(projection)?.rows, before);
                connection.execute(
                    "BEGIN;
                     UPDATE timezone_core SET shifted=timezone(INTERVAL '-4 hours',source);
                     ROLLBACK",
                )?;
                assert_eq!(connection.query(projection)?.rows, before);
                connection.execute(
                    "UPDATE timezone_core SET shifted=timezone(INTERVAL '-4 hours',source)",
                )?;
                committed = connection.query(projection)?.rows;
            }
            let mut connection = open()?.connect();
            assert_eq!(
                connection
                    .query("SELECT id,source,shifted,timezone(source),timezone_hour(source),timezone_minute(source) FROM timezone_core ORDER BY id")?
                    .rows,
                committed
            );
            connection.checkpoint()?;
            drop(connection);
            assert_eq!(
                open()?
                    .connect()
                    .query("SELECT id,source,shifted,timezone(source),timezone_hour(source),timezone_minute(source) FROM timezone_core ORDER BY id")?
                    .rows,
                committed
            );
        }
    }
    Ok(())
}
