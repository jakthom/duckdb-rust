use super::*;
use duckdb_rust::{
    common::cast::{CastFunction, CastMode, CastRegistry, CastSpec},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::{InterruptHandle, QueryContext},
};
use std::sync::Mutex;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn interval(months: i32, days: i32, micros: i64) -> Value {
    Value::Temporal(TemporalValue::Interval {
        months,
        days,
        micros,
    })
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn epoch_seconds_and_interval_normalization_match_rounding_ranges_and_optimizer_paths() -> Result<()>
{
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
            let result = connection.query(
                "SELECT
                    typeof(to_timestamp(0)), typeof(normalized_interval(NULL)),
                    epoch_us(to_timestamp(0.0000004)),
                    epoch_us(to_timestamp(0.0000005)),
                    epoch_us(to_timestamp(0.0000006)),
                    epoch_us(to_timestamp(0.0000015)),
                    epoch_us(to_timestamp(0.0000025)),
                    epoch_us(to_timestamp(-0.0000006)),
                    epoch_us(to_timestamp(-0.0000015)),
                    epoch_us(to_timestamp(-0.0000025)),
                    to_timestamp(NULL), normalized_interval(NULL)",
            )?;
            assert_eq!(
                result.rows,
                vec![vec![
                    Value::Varchar("TIMESTAMP WITH TIME ZONE".into()),
                    Value::Varchar("INTERVAL".into()),
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(1),
                    Value::Integer(2),
                    Value::Integer(2),
                    Value::Integer(-1),
                    Value::Integer(-2),
                    Value::Integer(-2),
                    Value::Null,
                    Value::Null,
                ]]
            );
            assert_eq!(
                connection
                    .query(
                        "SELECT
                            normalized_interval(INTERVAL '1 month -4 days'),
                            normalized_interval(INTERVAL '-1 month 4 days'),
                            normalized_interval(INTERVAL '29 days -1 microsecond'),
                            normalized_interval(INTERVAL '-29 days 1 microsecond')",
                    )?
                    .rows,
                vec![vec![
                    interval(0, 26, 0),
                    interval(-1, 4, 0),
                    interval(0, 28, 86_399_999_999),
                    interval(-1, 1, 1),
                ]]
            );
            assert_eq!(
                connection.query("SELECT normalized_interval(INTERVAL '-2147483648 months -2147483648 days -9223372036854775807 microseconds')::VARCHAR,normalized_interval(INTERVAL '2147483647 months 2147483647 days 9223372036854775807 microseconds')::VARCHAR")?.rows,
                vec![vec![
                    Value::Varchar("-178956970 years -8 months -2147483648 days -2562047788:00:54.775808".into()),
                    Value::Varchar("178956970 years 7 months 2147483647 days 2562047788:00:54.775807".into()),
                ]]
            );
            assert_eq!(
                connection.query("SELECT normalized_interval(i) FROM (VALUES (INTERVAL '30 days'),(INTERVAL '-1 microsecond'),(NULL::INTERVAL)) t(i)")?.rows,
                vec![
                    vec![interval(1, 0, 0)],
                    vec![interval(-1, 29, 86_399_999_999)],
                    vec![Value::Null],
                ]
            );
            let prepared =
                connection.prepare("SELECT epoch_us(to_timestamp($1)),normalized_interval($2)")?;
            assert_eq!(
                connection
                    .execute_prepared(&prepared, &[Value::Double(0.0000015), interval(0, 30, -1)])?
                    .rows,
                vec![vec![Value::Integer(2), interval(0, 29, 86_399_999_999)]]
            );
            assert_eq!(
                connection
                    .query("SELECT epoch_us(to_timestamp('-9223372036854.775'::DOUBLE))")?
                    .rows,
                vec![vec![Value::Integer(i128::from(i64::MIN))]]
            );
            for input in [
                "'NaN'::DOUBLE",
                "'Infinity'::DOUBLE",
                "'-Infinity'::DOUBLE",
                "1e300::DOUBLE",
                "'9223372036854.775'::DOUBLE",
            ] {
                assert!(
                    matches!(connection.query(&format!("SELECT to_timestamp({input})")), Err(Error::Conversion(message)) if message == "Epoch seconds out of range for TIMESTAMP WITH TIME ZONE"),
                    "{input}"
                );
            }
            assert_eq!(
                connection
                    .query(
                        "SELECT CASE WHEN false THEN to_timestamp(1e300::DOUBLE) ELSE NULL END",
                    )?
                    .rows,
                vec![vec![Value::Null]]
            );
            for sql in [
                "SELECT to_timestamp()",
                "SELECT to_timestamp(1,2)",
                "SELECT normalized_interval()",
                "SELECT normalized_interval(INTERVAL '1 day',INTERVAL '2 days')",
            ] {
                assert!(
                    matches!(connection.query(sql), Err(Error::Bind(_))),
                    "{sql}"
                );
            }
        }
    }
    Ok(())
}

struct SelectedEpochCast {
    calls: Arc<AtomicUsize>,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for SelectedEpochCast {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SelectedEpochCast")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedEpochCast {
    fn name(&self) -> &'static str {
        "selected-epoch-cast"
    }

    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && matches!(spec.target, DataType::Double | DataType::Interval)
            && matches!(spec.mode, CastMode::Implicit | CastMode::Explicit)
    }

    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let Value::Varchar(text) = value else {
            return Err(Error::Internal("selected epoch cast input".into()));
        };
        if text == "failure" {
            return Err(Error::Resource("selected epoch cast failure".into()));
        }
        if text == "interrupt" {
            self.interrupt
                .lock()
                .unwrap()
                .as_ref()
                .expect("installed connection interrupt")
                .interrupt();
        }
        Ok(match spec.target {
            DataType::Double => Value::Double(0.0000015),
            DataType::Interval => interval(1, -4, 0),
            _ => return Err(Error::Internal("selected epoch cast target".into())),
        })
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn selected_casts_survive_rows_parameters_defaults_and_cancellation() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let interrupt = Arc::new(Mutex::new(None));
            let selected = Arc::new(SelectedEpochCast {
                calls: calls.clone(),
                interrupt: interrupt.clone(),
            });
            let mut casts = CastRegistry::builtins();
            for target in [DataType::Double, DataType::Interval] {
                casts.replace(
                    CastSpec {
                        source: DataType::Varchar,
                        target: target.clone(),
                        mode: CastMode::Explicit,
                    },
                    selected.clone(),
                )?;
                casts.register(
                    CastSpec {
                        source: DataType::Varchar,
                        target,
                        mode: CastMode::Implicit,
                    },
                    selected.clone(),
                )?;
            }
            let mut connection = DatabaseBuilder::new()
                .casts(casts)
                .batch_size(2)
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            *interrupt.lock().unwrap() = Some(connection.interrupt_handle());
            connection.execute(
                "CREATE TABLE selected_epoch(
                    id INTEGER PRIMARY KEY,
                    source VARCHAR,
                    stamp TIMESTAMPTZ DEFAULT to_timestamp('default'),
                    span INTERVAL DEFAULT normalized_interval('default')
                )",
            )?;
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            connection.execute("INSERT INTO selected_epoch(id,source) VALUES (1,'row')")?;
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(
                connection.query("SELECT epoch_us(stamp),span,epoch_us(to_timestamp(source)),normalized_interval(source) FROM selected_epoch")?.rows,
                vec![vec![
                    Value::Integer(2),
                    interval(0, 26, 0),
                    Value::Integer(2),
                    interval(0, 26, 0),
                ]]
            );
            let prepared =
                connection.prepare("SELECT epoch_us(to_timestamp($1)),normalized_interval($2)")?;
            assert_eq!(
                connection
                    .execute_prepared(
                        &prepared,
                        &[
                            Value::Varchar("parameter".into()),
                            Value::Varchar("parameter".into())
                        ]
                    )?
                    .rows,
                vec![vec![Value::Integer(2), interval(0, 26, 0)]]
            );
            assert!(matches!(
                connection.query("SELECT to_timestamp('failure')"),
                Err(Error::Resource(message)) if message == "selected epoch cast failure"
            ));
            assert!(matches!(
                connection.query("SELECT normalized_interval('interrupt')"),
                Err(Error::Interrupted)
            ));
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn epoch_results_defaults_and_mutations_cross_private_and_native_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (index, format) in [
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
            let path = directory
                .path()
                .join(format!("epoch-extra-{index}-{}.db", expressions.name()));
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
                    "CREATE TABLE epoch_extra(
                        id INTEGER PRIMARY KEY,
                        seconds DOUBLE,
                        input INTERVAL,
                        stamp TIMESTAMPTZ DEFAULT to_timestamp(0.0000015),
                        span INTERVAL DEFAULT normalized_interval(INTERVAL '1 month -4 days')
                    )",
                )?;
            }
            let committed;
            {
                let mut connection = open()?.connect();
                let insert = connection
                    .prepare("INSERT INTO epoch_extra(id,seconds,input) VALUES ($1,$2,$3)")?;
                connection.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(1),
                        Value::Double(0.0000025),
                        interval(0, 30, -1),
                    ],
                )?;
                connection
                    .execute_prepared(&insert, &[Value::Integer(2), Value::Null, Value::Null])?;
                let projection = "SELECT id,epoch_us(to_timestamp(seconds)),normalized_interval(input),epoch_us(stamp),span FROM epoch_extra ORDER BY id";
                let before = connection.query(projection)?.rows;
                assert_eq!(
                    before,
                    vec![
                        vec![
                            Value::Integer(1),
                            Value::Integer(2),
                            interval(0, 29, 86_399_999_999),
                            Value::Integer(2),
                            interval(0, 26, 0),
                        ],
                        vec![
                            Value::Integer(2),
                            Value::Null,
                            Value::Null,
                            Value::Integer(2),
                            interval(0, 26, 0),
                        ],
                    ]
                );
                assert!(matches!(
                    connection.execute("UPDATE epoch_extra SET stamp=to_timestamp(seconds*1e300)"),
                    Err(Error::Conversion(message)) if message == "Epoch seconds out of range for TIMESTAMP WITH TIME ZONE"
                ));
                assert_eq!(connection.query(projection)?.rows, before);
                connection.execute("BEGIN; UPDATE epoch_extra SET stamp=to_timestamp(seconds),span=normalized_interval(input); DELETE FROM epoch_extra WHERE id=1; ROLLBACK")?;
                assert_eq!(connection.query(projection)?.rows, before);
                connection.execute("UPDATE epoch_extra SET stamp=to_timestamp(seconds),span=normalized_interval(input)")?;
                committed = connection.query(projection)?.rows;
            }
            let mut connection = open()?.connect();
            assert_eq!(
                connection.query("SELECT id,epoch_us(to_timestamp(seconds)),normalized_interval(input),epoch_us(stamp),span FROM epoch_extra ORDER BY id")?.rows,
                committed
            );
            connection.checkpoint()?;
            drop(connection);
            assert_eq!(
                open()?
                    .connect()
                    .query("SELECT id,epoch_us(to_timestamp(seconds)),normalized_interval(input),epoch_us(stamp),span FROM epoch_extra ORDER BY id")?
                    .rows,
                committed
            );
        }
    }
    Ok(())
}
