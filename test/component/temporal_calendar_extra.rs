use super::*;
use duckdb_rust::{
    Date,
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
fn calendar_extracts_match_iso_boundaries_bce_aliases_and_domains() -> Result<()> {
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
                "SELECT d::VARCHAR,era(d),isoyear(d),week(d),weekofyear(d),weekday(d),
                        yearweek(d),julian(d)
                 FROM (VALUES
                    (DATE '2015-12-31'),(DATE '2016-01-01'),(DATE '2016-01-04'),
                    (DATE '2019-12-30'),(DATE '2021-01-03'),
                    (DATE '0001-01-01 (BC)'),(DATE '0001-12-31 (BC)'),
                    (DATE '0001-12-31')) dates(d)",
            )?;
            assert_eq!(
                text_rows(result),
                vec![
                    vec![
                        "2015-12-31",
                        "1",
                        "2015",
                        "53",
                        "53",
                        "4",
                        "201553",
                        "2457388"
                    ],
                    vec![
                        "2016-01-01",
                        "1",
                        "2015",
                        "53",
                        "53",
                        "5",
                        "201553",
                        "2457389"
                    ],
                    vec![
                        "2016-01-04",
                        "1",
                        "2016",
                        "1",
                        "1",
                        "1",
                        "201601",
                        "2457392"
                    ],
                    vec![
                        "2019-12-30",
                        "1",
                        "2020",
                        "1",
                        "1",
                        "1",
                        "202001",
                        "2458848"
                    ],
                    vec![
                        "2021-01-03",
                        "1",
                        "2020",
                        "53",
                        "53",
                        "0",
                        "202053",
                        "2459218"
                    ],
                    vec![
                        "0001-01-01 (BC)",
                        "0",
                        "-1",
                        "52",
                        "52",
                        "6",
                        "-152",
                        "1721060"
                    ],
                    vec![
                        "0001-12-31 (BC)",
                        "0",
                        "0",
                        "52",
                        "52",
                        "0",
                        "-52",
                        "1721425"
                    ],
                    vec!["0001-12-31", "1", "2", "1", "1", "1", "201", "1721790"],
                ]
            );
            let result = connection.query(
                "SELECT
                    typeof(era(DATE 'epoch')),typeof(julian(DATE 'epoch')),
                    julian(TIMESTAMP '2000-01-01 12:00:00'),
                    julian(TIMESTAMP_NS '2000-01-01 12:00:00.123456789'),
                    era(NULL::DATE),week(DATE 'infinity'),julian(TIMESTAMP '-infinity')",
            )?;
            assert_eq!(
                result.rows,
                vec![vec![
                    Value::Varchar("BIGINT".into()),
                    Value::Varchar("DOUBLE".into()),
                    Value::Double(2_451_545.5),
                    Value::Double(2_451_545.500_001_429),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ]]
            );
            for (part, expected) in [
                ("era", "1"),
                ("isoyear", "1970"),
                ("week", "1"),
                ("weeks", "1"),
                ("w", "1"),
                ("weekofyear", "1"),
                ("weekday", "4"),
                ("dow", "4"),
                ("dayofweek", "4"),
                ("yearweek", "197001"),
                ("julian", "2440588"),
                ("jd", "2440588"),
            ] {
                for function in ["date_part", "datepart"] {
                    let result =
                        connection.query(&format!("SELECT {function}('{part}',DATE 'epoch')"))?;
                    assert_eq!(result.rows[0][0].to_string(), expected, "{function} {part}");
                    assert_eq!(
                        result.columns[0].data_type,
                        if matches!(part, "julian" | "jd") {
                            DataType::Double
                        } else {
                            DataType::BigInt
                        },
                        "{function} {part}"
                    );
                }
            }
            let dynamic = connection.query(
                "SELECT p,typeof(date_part(p,DATE 'epoch')),date_part(p,DATE 'epoch')
                 FROM (VALUES ('week'),('julian')) parts(p)",
            )?;
            assert_eq!(
                dynamic.rows,
                vec![
                    vec![
                        Value::Varchar("week".into()),
                        Value::Varchar("DOUBLE".into()),
                        Value::Double(1.0),
                    ],
                    vec![
                        Value::Varchar("julian".into()),
                        Value::Varchar("DOUBLE".into()),
                        Value::Double(2_440_588.0),
                    ],
                ]
            );
            let prepared = connection.prepare(
                "SELECT era($1),isoyear($1),week($1),weekday($1),yearweek($1),julian($1)",
            )?;
            assert_eq!(
                connection
                    .execute_prepared(&prepared, &[Value::Date(Date::from_ymd(2016, 1, 1)?)])?
                    .rows,
                vec![vec![
                    Value::Integer(1),
                    Value::Integer(2015),
                    Value::Integer(53),
                    Value::Integer(5),
                    Value::Integer(201553),
                    Value::Double(2_457_389.0),
                ]]
            );
            for sql in [
                "SELECT era(NULL)",
                "SELECT week('2020-01-01')",
                "SELECT week(TIME '12:00:00')",
                "SELECT era(TIMESTAMPTZ '2000-01-01 00:00:00+00')",
                "SELECT julian(INTERVAL '1 day')",
            ] {
                assert!(
                    matches!(connection.query(sql), Err(Error::Bind(_))),
                    "{sql}"
                );
            }
            for sql in [
                "SELECT era(INTERVAL '1 year')",
                "SELECT isoyear(INTERVAL '1 year')",
                "SELECT week(INTERVAL '1 day')",
                "SELECT weekofyear(INTERVAL '1 day')",
                "SELECT weekday(INTERVAL '1 day')",
                "SELECT yearweek(INTERVAL '1 year')",
                "SELECT date_part('julian',INTERVAL '1 day')",
            ] {
                assert!(
                    matches!(connection.query(sql), Err(Error::NotImplemented(_))),
                    "{sql}"
                );
            }
        }
    }
    Ok(())
}

struct SelectedCalendarCast {
    calls: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    handle: Arc<Mutex<Option<InterruptHandle>>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for SelectedCalendarCast {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SelectedCalendarCast")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedCalendarCast {
    fn name(&self) -> &'static str {
        "selected-calendar-cast"
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
            return Err(Error::Resource("selected calendar cast failure".into()));
        }
        if self.interrupt.load(Ordering::SeqCst) {
            self.handle
                .lock()
                .unwrap()
                .as_ref()
                .expect("installed interrupt handle")
                .interrupt();
        }
        Ok(Value::Temporal(TemporalValue::Timestamp(
            946_684_800_000_000,
        )))
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn selected_timestamp_casts_reach_named_generic_defaults_and_cancellation() -> Result<()> {
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
            Arc::new(SelectedCalendarCast {
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
                    "SELECT week(v),date_part('julian',v)
                     FROM (VALUES (TIMESTAMP_NS 'epoch'),(TIMESTAMP_NS '2024-01-01')) rows(v)",
                )?
                .rows,
            vec![
                vec![Value::Integer(52), Value::Double(2_451_545.0)],
                vec![Value::Integer(52), Value::Double(2_451_545.0)],
            ]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        connection.execute(
            "CREATE TABLE selected_calendar(
                id INTEGER,
                value BIGINT DEFAULT week(TIMESTAMP_NS 'epoch')
            )",
        )?;
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        connection.execute("INSERT INTO selected_calendar(id) VALUES (1)")?;
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        assert_eq!(
            connection
                .query("SELECT value FROM selected_calendar")?
                .rows,
            vec![vec![Value::Integer(52)]]
        );
        fail.store(true, Ordering::SeqCst);
        assert!(matches!(
            connection.query("SELECT week(TIMESTAMP_NS 'epoch')"),
            Err(Error::Resource(message)) if message == "selected calendar cast failure"
        ));
        fail.store(false, Ordering::SeqCst);
        interrupt.store(true, Ordering::SeqCst);
        assert!(matches!(
            connection.query("SELECT date_part('week',TIMESTAMP_NS 'epoch')"),
            Err(Error::Interrupted)
        ));
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_defaults_and_mutations_cross_private_and_native_reopen() -> Result<()> {
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
                "calendar-extra-{format_index}-{}.db",
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
                    "CREATE TABLE calendar_extra(
                        id INTEGER PRIMARY KEY,
                        d DATE,
                        t TIMESTAMP,
                        iso BIGINT DEFAULT yearweek(DATE '2019-12-30'),
                        jd DOUBLE DEFAULT julian(TIMESTAMP '2000-01-01 12:00:00')
                    )",
                )?;
            }
            let committed;
            {
                let mut connection = open()?.connect();
                let insert =
                    connection.prepare("INSERT INTO calendar_extra(id,d,t) VALUES ($1,$2,$3)")?;
                connection.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(1),
                        Value::Date(Date::from_ymd(2021, 1, 3)?),
                        Value::Temporal(TemporalValue::Timestamp(0)),
                    ],
                )?;
                connection.execute(
                    "INSERT INTO calendar_extra VALUES
                     (2,DATE '2016-01-04',TIMESTAMP '2000-01-01 12:00:00',201601,2451545.5)",
                )?;
                let projection = "SELECT id,isoyear(d),week(d),weekday(d),yearweek(d),julian(t),iso,jd FROM calendar_extra ORDER BY id";
                let before = connection.query(projection)?.rows;
                assert!(matches!(
                    connection.execute("UPDATE calendar_extra SET iso=week(INTERVAL '1 day')"),
                    Err(Error::NotImplemented(_))
                ));
                assert_eq!(connection.query(projection)?.rows, before);
                connection.execute(
                    "BEGIN;
                     UPDATE calendar_extra SET iso=yearweek(d),jd=julian(t);
                     DELETE FROM calendar_extra WHERE id=2;
                     ROLLBACK",
                )?;
                assert_eq!(connection.query(projection)?.rows, before);
                connection.execute("UPDATE calendar_extra SET iso=yearweek(d),jd=julian(t)")?;
                committed = connection.query(projection)?.rows;
            }
            let mut connection = open()?.connect();
            assert_eq!(
                connection
                    .query("SELECT id,isoyear(d),week(d),weekday(d),yearweek(d),julian(t),iso,jd FROM calendar_extra ORDER BY id")?
                    .rows,
                committed
            );
            connection.checkpoint()?;
            drop(connection);
            assert_eq!(
                open()?
                    .connect()
                    .query("SELECT id,isoyear(d),week(d),weekday(d),yearweek(d),julian(t),iso,jd FROM calendar_extra ORDER BY id")?
                    .rows,
                committed
            );
        }
    }
    Ok(())
}
