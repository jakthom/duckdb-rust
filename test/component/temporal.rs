use duckdb_rust::{DataType, Database, Result, TemporalValue, Value};
use duckdb_rust::{
    DatabaseBuilder,
    execution::{
        expression_executor::{BatchedEvaluator, ScalarEvaluator},
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        operator::join::{HashJoin, JoinAlgorithm, NestedLoopJoin},
        physical_plan::NativePhysicalPlanner,
    },
    storage::{
        checkpoint::{Durability, FileCheckpoint},
        duckdb::{
            DuckDbFormat,
            wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
        },
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
        logged::FileWal,
    },
};
use std::sync::Arc;

mod temporal_calendar;
mod temporal_difference;
mod temporal_minimum;
mod temporal_provenance;
mod temporal_variant;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn temporal_literals_keep_type_precision_nulls_and_canonical_comparison() -> Result<()> {
    assert_eq!(std::mem::size_of::<Value>(), 32);
    assert!(std::mem::size_of::<DataType>() <= 16);
    let mut c = Database::memory()?.connect();
    for (sql, expected_type, expected) in [
        ("TIME '24:00:00'", DataType::Time, "24:00:00"),
        ("TIME '12:34:56.1234567'", DataType::Time, "12:34:56.123456"),
        (
            "TIME_NS '12:34:56.123456789'",
            DataType::TimeNs,
            "12:34:56.123456789",
        ),
        ("TIMETZ '12:00:00+02'", DataType::TimeTz, "12:00:00+02"),
        (
            "TIMESTAMP '1969-12-31 23:59:59.9999999'",
            DataType::Timestamp,
            "1969-12-31 23:59:59.999999",
        ),
        (
            "TIMESTAMP_NS '1969-12-31 23:59:59.999999999'",
            DataType::TimestampNs,
            "1969-12-31 23:59:59.999999999",
        ),
        (
            "TIMESTAMPTZ '2000-01-01 12:00:00+02'",
            DataType::TimestampTz,
            "2000-01-01 10:00:00+00",
        ),
        (
            "INTERVAL '1.5 months'",
            DataType::Interval,
            "1 month 15 days",
        ),
        (
            "INTERVAL '1 day -24 hours'",
            DataType::Interval,
            "1 day -24:00:00",
        ),
    ] {
        let result = c.query(&format!("SELECT {sql}"))?;
        assert_eq!(result.columns[0].data_type, expected_type, "{sql}");
        assert_eq!(result.rows[0][0].to_string(), expected, "{sql}");
        assert_eq!(
            c.query(&format!("SELECT ({sql})::VARCHAR"))?.rows[0][0],
            Value::Varchar(expected.into())
        );
    }
    assert_eq!(c.query("SELECT INTERVAL '1 month' = INTERVAL '30 days', TIMETZ '13:00:00+01' = TIMETZ '12:00:00+00', TIMETZ '13:00:00+01' < TIMETZ '12:00:00+00', TRY_CAST('25:00' AS TIME)")?.rows, vec![vec![Value::Boolean(true),Value::Boolean(false),Value::Boolean(true),Value::Null]]);
    for invalid in [
        TemporalValue::Time(-1),
        TemporalValue::TimeNs(86_400_500_000_001),
        TemporalValue::TimeNs(i64::MIN),
        TemporalValue::TimeTz {
            micros: 0,
            offset: 57600,
        },
    ] {
        assert!(invalid.validate().is_err());
        assert!(!Value::Temporal(invalid).fits_type(&invalid.data_type()));
    }
    Ok(())
}
#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn interval_index_rejection_preserves_the_catalog_and_other_key_operators()
-> duckdb_rust::Result<()> {
    use duckdb_rust::{Database, Error, Value};
    let database = Database::memory()?;
    let mut connection = database.connect();
    for constraint in ["PRIMARY KEY", "UNIQUE"] {
        assert!(matches!(
            connection.execute(&format!("CREATE TABLE invalid(i INTERVAL {constraint})")),
            Err(Error::InvalidType(_))
        ));
    }
    connection.execute("CREATE TABLE invalid(i INTERVAL); INSERT INTO invalid VALUES (INTERVAL '1 month'),(INTERVAL '30 days')")?;
    assert_eq!(
        connection
            .query("SELECT count(DISTINCT i) FROM invalid")?
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn temporal_arithmetic_preserves_calendar_components_precision_and_errors() -> Result<()> {
    let mut c = Database::memory()?.connect();
    for (sql, expected) in [
        (
            "DATE '2024-01-31' + INTERVAL '1 month'",
            "2024-02-29 00:00:00",
        ),
        (
            "TIMESTAMP '2023-03-31 12:01:02.123456' - INTERVAL '1 month'",
            "2023-02-28 12:01:02.123456",
        ),
        ("TIME '24:00' + INTERVAL '0 seconds'", "00:00:00"),
        ("TIME '23:00' + INTERVAL '2 hours'", "01:00:00"),
        ("TIMETZ '00:00:00+02' - INTERVAL '1 hour'", "23:00:00+02"),
        ("TIMESTAMP '2000-01-02' - TIMESTAMP '2000-01-01'", "1 day"),
        ("DATE '2000-01-01' + TIME '12:00'", "2000-01-01 12:00:00"),
        (
            "DATE '2000-01-01' + TIMETZ '12:00:00+02'",
            "2000-01-01 10:00:00+00",
        ),
        (
            "-INTERVAL '1 month 2 days 03:04:05'",
            "-1 month -2 days -03:04:05",
        ),
        ("INTERVAL '1 month' * 1.5", "1 month 15 days"),
        ("INTERVAL '1 day' / 2", "12:00:00"),
        (
            "TIMESTAMP_NS '1969-12-31 23:59:59.999999999'::TIMESTAMP",
            "1970-01-01 00:00:00",
        ),
        ("TIMESTAMP 'infinity' + INTERVAL '1 month'", "infinity"),
    ] {
        assert_eq!(
            c.query(&format!("SELECT {sql}"))?.rows[0][0].to_string(),
            expected,
            "{sql}"
        );
    }
    for sql in [
        "SELECT TIMESTAMP 'infinity' - TIMESTAMP 'infinity'",
        "SELECT -INTERVAL '-2147483648 months'",
        "SELECT TIMESTAMP_NS 'epoch' + INTERVAL '999999999 days'",
        "SELECT TIME_NS '12:00' + INTERVAL '1 second'",
        "SELECT TIMESTAMPTZ_NS 'epoch' + INTERVAL '1 day'",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn temporal_reference_regressions_reject_clock_offsets_and_keep_development_ties() -> Result<()> {
    let mut c = Database::memory()?.connect();
    for sql in [
        "SELECT TIME '12:00+02'",
        "SELECT TIMETZ '12:00+02'",
        "SELECT INTERVAL '170141183460469231731687303715884105727:00:00'",
        "SELECT INTERVAL '99999999999999999999999999999999999999 days'",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    let result = c.query(
        "SELECT min(i),max(i) FROM (VALUES (INTERVAL '1 month'),(INTERVAL '30 days')) t(i)",
    )?;
    assert_eq!(
        result.rows[0]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["30 days", "30 days"]
    );
    let result=c.query("SELECT min(i) OVER(ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) FROM (VALUES (INTERVAL '1 month'),(INTERVAL '30 days')) t(i)")?;
    assert!(result.rows.iter().all(|r| r[0].to_string() == "30 days"));
    assert_eq!(
        c.query("SELECT typeof(TIMESTAMPTZ_NS 'epoch')")?.rows[0][0],
        Value::Varchar("TIMESTAMPTZ_NS".into())
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_temporals_cross_relational_vectors_parameters_and_windows() -> Result<()> {
    for batched in [false, true] {
        for batch_size in [1, 7, 2048] {
            for hashed in [false, true] {
                let join: Arc<dyn JoinAlgorithm> = if hashed {
                    Arc::new(HashJoin)
                } else {
                    Arc::new(NestedLoopJoin)
                };
                let mut c = DatabaseBuilder::new()
                    .batch_size(batch_size)
                    .expressions(if batched {
                        Arc::new(BatchedEvaluator)
                    } else {
                        Arc::new(ScalarEvaluator)
                    })
                    .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![join])))
                    .build()?
                    .connect();
                c.execute("CREATE TABLE t(id INTEGER, ts TIMESTAMP, span INTERVAL, clock TIMETZ); INSERT INTO t VALUES (1,TIMESTAMP 'epoch',INTERVAL '1 month',TIMETZ '12:00:00+01'),(2,TIMESTAMP 'epoch',INTERVAL '30 days',TIMETZ '12:00:00+01'),(3,TIMESTAMP '2000-01-01',INTERVAL '31 days',TIMETZ '11:00:00+00'),(4,NULL,NULL,NULL)")?;
                assert_eq!(
                    c.query("SELECT count(*) FROM t a JOIN t b ON a.span=b.span")?
                        .rows[0][0],
                    Value::Integer(5)
                );
                assert_eq!(
                    c.query("SELECT count(*) FROM t a JOIN t b ON a.clock=b.clock")?
                        .rows[0][0],
                    Value::Integer(5)
                );
                let result = c.query("SELECT span,count(*) FROM t GROUP BY span ORDER BY span")?;
                assert_eq!(result.rows.len(), 3);
                assert_eq!(result.rows[0][1], Value::Integer(2));
                let result=c.query("SELECT id, min(ts) OVER (PARTITION BY span), first_value(clock) OVER (ORDER BY clock ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t ORDER BY id")?;
                assert_eq!(result.rows.len(), 4);
                assert_eq!(
                    result.rows[0][1],
                    Value::Temporal(TemporalValue::Timestamp(0))
                );
                assert_eq!(
                    c.query("SELECT count(DISTINCT span), min(ts), max(ts) FROM t")?
                        .rows[0][0],
                    Value::Integer(2)
                );
                let statement = c.prepare("SELECT ts + $1 FROM t WHERE ts=$2 ORDER BY id")?;
                let values = [
                    Value::Temporal(TemporalValue::Interval {
                        months: 1,
                        days: 0,
                        micros: 0,
                    }),
                    Value::Temporal(TemporalValue::Timestamp(0)),
                ];
                let result = c.execute_prepared(&statement, &values)?;
                assert_eq!(result.rows.len(), 2);
                assert_eq!(result.rows[0][0].to_string(), "1970-02-01 00:00:00");
                let values = [
                    Value::Temporal(TemporalValue::Interval {
                        months: 0,
                        days: 1,
                        micros: 0,
                    }),
                    Value::Null,
                ];
                assert!(c.execute_prepared(&statement, &values)?.rows.is_empty());
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn temporal_mixed_schema_defaults_indexes_mutations_rollback_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let formats: Vec<Arc<dyn SnapshotFormat>> = vec![
        Arc::new(JsonSnapshotFormat),
        Arc::new(DuckDbFormat::default()),
    ];
    for format in formats {
        for hashed in [false, true] {
            let indexes: Arc<dyn IndexFactory> = if hashed {
                Arc::new(HashIndexFactory)
            } else {
                Arc::new(BTreeIndexFactory)
            };
            let path = directory
                .path()
                .join(format!("{}-{hashed}.db", format.name()));
            let open = || {
                DatabaseBuilder::new()
                    .indexes(indexes.clone())
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format.clone(),
                    )?))
                    .build()
            };
            {
                let mut c = open()?.connect();
                c.execute("CREATE TABLE t(ts TIMESTAMP PRIMARY KEY DEFAULT TIMESTAMP 'epoch',tm TIME DEFAULT TIME '12:00',ns TIME_NS DEFAULT TIME_NS '12:00:00.123456789',tz TIMETZ DEFAULT TIMETZ '12:00:00+02',sec TIMESTAMP_S DEFAULT TIMESTAMP_S 'epoch',ms TIMESTAMP_MS DEFAULT TIMESTAMP_MS 'epoch',nanos TIMESTAMP_NS DEFAULT TIMESTAMP_NS 'epoch',z TIMESTAMPTZ DEFAULT TIMESTAMPTZ 'epoch',zns TIMESTAMPTZ_NS DEFAULT TIMESTAMPTZ_NS 'epoch',iv INTERVAL DEFAULT INTERVAL '1 month 2 days 03:04:05',d DECIMAL(12,2) DEFAULT 1.25); INSERT INTO t DEFAULT VALUES")?;
                let statement = c.prepare("INSERT INTO t(ts,tm,iv,d) VALUES ($1,$2,$3,$4)")?;
                c.execute_prepared(
                    &statement,
                    &[
                        Value::Temporal(TemporalValue::parse("2000-01-01", &DataType::Timestamp)?),
                        Value::Null,
                        Value::Temporal(TemporalValue::Interval {
                            months: -1,
                            days: 30,
                            micros: -1,
                        }),
                        Value::Varchar("99.50".into()),
                    ],
                )?;
            }
            let mut c = open()?.connect();
            assert!(
                c.execute("INSERT INTO t(ts) VALUES (TIMESTAMP 'epoch')")
                    .is_err()
            );
            c.execute("BEGIN; UPDATE t SET iv=INTERVAL '1 year'; DELETE FROM t WHERE ts=TIMESTAMP 'epoch'; ROLLBACK; UPDATE t SET ts=TIMESTAMP '1970-01-02', iv=iv+INTERVAL '1 day' WHERE ts=TIMESTAMP 'epoch'")?;
            let before = c.query("SELECT * FROM t ORDER BY ts")?.rows;
            assert_eq!(before.len(), 2);
            assert_eq!(before[0][9].to_string(), "1 month 3 days 03:04:05");
            assert!(before[1][1].is_null());
            drop(c);
            assert_eq!(
                open()?.connect().query("SELECT * FROM t ORDER BY ts")?.rows,
                before
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn temporal_wal_replay_preserves_commits_and_discards_rolled_back_mutations() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("temporal.duckdb");
    Database::open(&path)?
        .connect()
        .execute("CREATE TABLE t(ts TIMESTAMP PRIMARY KEY,iv INTERVAL,tm TIME,zone TIMETZ)")?;
    let open = || {
        let checkpoint = FileCheckpoint::open(
            &path,
            OpenMode::ReadWrite,
            Arc::new(DuckDbFormat::default()),
        )?
        .with_recovery(Arc::new(DuckDbWalRecovery))?;
        let durability: Arc<dyn Durability> =
            Arc::new(FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?);
        DatabaseBuilder::new().durability(durability).build()
    };
    {
        let mut c = open()?.connect();
        c.execute("INSERT INTO t VALUES (TIMESTAMP 'epoch',INTERVAL '1 month',TIME '12:00',TIMETZ '13:00:00+02'); BEGIN; INSERT INTO t VALUES (TIMESTAMP '2000-01-01',NULL,NULL,NULL); ROLLBACK; UPDATE t SET iv=INTERVAL '30 days' WHERE ts=TIMESTAMP 'epoch'")?;
    }
    let mut c = open()?.connect();
    let rows = c.query("SELECT * FROM t")?.rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1].to_string(), "30 days");
    assert_eq!(rows[0][3].to_string(), "13:00:00+02");
    c.checkpoint()?;
    drop(c);
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT * FROM t")?
            .rows,
        rows
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_functions_extract_epoch_constructors_and_infinity_are_typed() -> Result<()> {
    let mut c = Database::memory()?.connect();
    for (sql, expected) in [
        ("make_date(2024,2,29)", "2024-02-29"),
        ("make_date(1)", "1970-01-02"),
        ("typeof(NULL::TIMESTAMP(0))", "TIMESTAMP_S"),
        ("typeof(NULL::TIMESTAMP(3))", "TIMESTAMP_MS"),
        ("typeof(NULL::TIMESTAMP(6))", "TIMESTAMP"),
        ("typeof(NULL::TIMESTAMP(10))", "TIMESTAMP_NS"),
        ("typeof(NULL::TIMESTAMP(3) WITH TIME ZONE)", "TIMESTAMP_MS"),
        ("TIMESTAMP_S '1969-12-31 23:59:59.5'", "1969-12-31 23:59:59"),
        ("TIMESTAMP_S '1970-01-01 00:00:00.5'", "1970-01-01 00:00:01"),
        (
            "TIMESTAMP_MS '1970-01-01 00:00:00.0005'",
            "1970-01-01 00:00:00.001",
        ),
        (
            "TIMESTAMP_NS '2000-01-01 23:59:59.999999500'::DATE",
            "2000-01-02",
        ),
        (
            "TIMESTAMP_NS '1969-12-31 23:59:59.999999500'::TIMESTAMP",
            "1969-12-31 23:59:59.999999",
        ),
        ("make_time(12,34,56.1234565)", "12:34:56.123457"),
        (
            "make_timestamp(2000,1,2,3,4,5.123456)",
            "2000-01-02 03:04:05.123456",
        ),
        ("make_timestamp(-1)", "1969-12-31 23:59:59.999999"),
        ("make_timestamp_ns(-1)", "1969-12-31 23:59:59.999999999"),
        ("make_timestamp_ms(-1)", "1969-12-31 23:59:59.999"),
        ("to_seconds(0.0000009)", "00:00:00.000001"),
        ("to_milliseconds(0.0009)", "00:00:00.000001"),
        (
            "to_years(2)+to_days(3)+to_hours(4)+to_minutes(5)+to_microseconds(6)",
            "2 years 3 days 04:05:00.000006",
        ),
        ("last_day(DATE '2024-02-01')", "2024-02-29"),
        ("dayname(DATE 'epoch')", "Thursday"),
        ("monthname(DATE 'epoch')", "January"),
        ("extract(year FROM DATE '0001-01-01 (BC)')", "0"),
        (
            "extract(microseconds FROM TIME '12:34:56.123456')",
            "56123456",
        ),
        ("year(INTERVAL '-13 months')", "-1"),
        ("month(INTERVAL '-13 months')", "-1"),
        ("hour(INTERVAL '35 hours')", "35"),
        ("epoch(INTERVAL '1 year')", "31557600"),
        ("epoch_us(INTERVAL '1 year')", "31104000000000"),
        ("epoch_ms(TIMESTAMP '1969-12-31 23:59:59.999500')", "-1"),
        ("epoch(TIME_NS '00:00:00.000000789')", "0"),
        ("epoch(TIMETZ '13:00:00+01')", "46800"),
        ("epoch_us(TIME_NS '00:00:00.000000001')", "0"),
        ("epoch_ms(-1)", "1969-12-31 23:59:59.999"),
        ("isinf(TIMESTAMP 'infinity')", "true"),
        ("isfinite(DATE 'epoch')", "true"),
        ("isfinite('NaN'::DOUBLE)", "false"),
        ("isinf('NaN'::DOUBLE)", "false"),
        ("isinf('-Infinity'::FLOAT)", "true"),
        ("epoch(TIMESTAMP 'infinity')", "NULL"),
        ("year(DATE '-infinity')", "NULL"),
        ("date_part('epoch',TIMESTAMP 'epoch')", "0"),
        (
            "TIMESTAMP_S '1969-12-31 23:59:59.999999'",
            "1970-01-01 00:00:00",
        ),
        (
            "TIMESTAMP_MS '1969-12-31 23:59:59.999999'",
            "1970-01-01 00:00:00",
        ),
        (
            "TIMESTAMPTZ '2000-01-01 00:00:00+23:59'",
            "1999-12-31 00:01:00+00",
        ),
        ("TIMETZ '12:00:00+02'::TIME", "12:00:00"),
        ("TIME '24:00:00'::TIMETZ", "24:00:00+00"),
        ("TIME '24:00:00'::TIME_NS", "24:00:00"),
        ("TIME_NS '24:00:00'::TIME", "24:00:00"),
        ("TIME_NS '00:00:00.000000500'::TIME", "00:00:00.000001"),
        (
            "TIMESTAMP_NS '1969-12-31 23:59:59.999999999'::TIME",
            "00:00:00",
        ),
        (
            "TIMESTAMP_NS '1969-12-31 23:59:59.999999999'::DATE",
            "1970-01-01",
        ),
        ("epoch(TIMESTAMP_NS '1969-12-31 23:59:59.999999999')", "0"),
        ("INTERVAL '1.9' YEAR", "1 year"),
        ("INTERVAL (-1.9) MONTH", "-1 month"),
        ("INTERVAL '1.5' SECOND", "00:00:01.5"),
        ("INTERVAL 1 WEEK", "7 days"),
        ("INTERVAL 1 QUARTER", "3 months"),
        ("INTERVAL 1 CENTURY", "100 years"),
        ("INTERVAL 1 DECADE", "10 years"),
        ("INTERVAL 1 MILLENNIUM", "1000 years"),
    ] {
        assert_eq!(
            c.query(&format!("SELECT {sql}"))?.rows[0][0].to_string(),
            expected,
            "{sql}"
        );
    }
    let result=c.query("SELECT typeof(date_part(p,t)), date_part(p,t) FROM (VALUES ('epoch',TIMESTAMP 'epoch'),('year',TIMESTAMP '2000-01-01')) v(p,t)")?;
    assert_eq!(
        result.rows[0],
        vec![Value::Varchar("DOUBLE".into()), Value::Double(0.0)]
    );
    assert_eq!(result.rows[1][1], Value::Double(2000.0));
    assert_eq!(
        c.query("SELECT typeof(date_part('year',DATE 'epoch'))")?
            .rows[0][0],
        Value::Varchar("BIGINT".into())
    );
    for sql in [
        "SELECT make_date(2023,2,29)",
        "SELECT make_time(25,0,0)",
        "SELECT to_years(2147483647)",
        "SELECT make_timestamp_ns('-9223372036854775808'::BIGINT)::VARCHAR",
        "SELECT make_date(DATE 'epoch')",
        "SELECT make_time(TIME '00:00')",
        "SELECT isfinite(INTERVAL '1 year')",
        "SELECT isinf(TIME '00:00')",
        "SELECT epoch_ms(TIME_NS '00:00:00.000999500')",
        "SELECT year(TIME '00:00')",
        "SELECT year(TIMESTAMPTZ 'epoch')",
        "SELECT epoch(TIMESTAMPTZ 'epoch')",
        "SELECT NULL::TIMESTAMP(11)",
        "SELECT NULL::TIMESTAMPTZ(3)",
        "SELECT NULL::TIME(3)",
        "SELECT INTERVAL 2147483648 DAY",
        "SELECT TIMESTAMPTZ 'epoch'::DATE",
        "SELECT TIMESTAMP_NS 'epoch'::TIMESTAMP_S",
        "SELECT TIMESTAMP_S '500000-01-01'",
        "SELECT TIMESTAMP_MS '500000-01-01'",
        "SELECT DATE '500000-01-01'::TIMESTAMP_S",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_functions_cross_nested_values_parameters_indexes_mutations_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let formats: Vec<Arc<dyn SnapshotFormat>> = vec![
        Arc::new(JsonSnapshotFormat),
        Arc::new(DuckDbFormat::default()),
    ];
    for format in formats {
        for batched in [false, true] {
            for hashed in [false, true] {
                let path = directory
                    .path()
                    .join(format!("calendar-{}-{batched}-{hashed}.db", format.name()));
                let open = || {
                    DatabaseBuilder::new()
                        .batch_size(2)
                        .expressions(if batched {
                            Arc::new(BatchedEvaluator)
                        } else {
                            Arc::new(ScalarEvaluator)
                        })
                        .indexes(if hashed {
                            Arc::new(HashIndexFactory)
                        } else {
                            Arc::new(BTreeIndexFactory)
                        })
                        .durability(Arc::new(FileCheckpoint::open(
                            &path,
                            OpenMode::ReadWrite,
                            format.clone(),
                        )?))
                        .build()
                };
                let mut c = open()?.connect();
                c.execute("CREATE TABLE events(id INTEGER PRIMARY KEY,ts TIMESTAMP_NS UNIQUE,payload STRUCT(occurred TIMESTAMP,budget DECIMAL(8,2)),samples TIMESTAMP[],span INTERVAL)")?;
                let insert=c.prepare("INSERT INTO events SELECT $1,$2,{'occurred':$2::TIMESTAMP,'budget':$3::DECIMAL(8,2)},[$2::TIMESTAMP,NULL],INTERVAL ($4) DAY")?;
                for (id, time, budget, days) in [
                    (1, "2000-01-31 23:59:59.999999500", "1.25", 1.9),
                    (2, "2000-02-01 00:00:00", "2.50", -1.9),
                ] {
                    c.execute_prepared(
                        &insert,
                        &[
                            Value::Integer(id),
                            Value::Temporal(TemporalValue::parse(time, &DataType::TimestampNs)?),
                            Value::Varchar(budget.into()),
                            Value::Double(days),
                        ],
                    )?;
                }
                c.execute_prepared(
                    &insert,
                    &[Value::Integer(3), Value::Null, Value::Null, Value::Null],
                )?;
                assert!(
                    c.execute("INSERT INTO events(id,ts) VALUES(4,TIMESTAMP_NS '2000-02-01')")
                        .is_err()
                );
                let projection = "SELECT id,year(ts),list_extract(samples,1),struct_extract(payload,'occurred')+span FROM events ORDER BY id";
                let rows = c.query(projection)?.rows;
                assert_eq!(rows[0][1], Value::Integer(2000));
                assert_eq!(rows[0][2].to_string(), "2000-02-01 00:00:00");
                assert_eq!(rows[0][3].to_string(), "2000-02-02 00:00:00");
                assert_eq!(rows[1][3].to_string(), "2000-01-31 00:00:00");
                assert!(rows[2][1..].iter().all(Value::is_null));
                assert_eq!(
                    c.query(
                        "SELECT count(*) FROM events a JOIN events b ON year(a.ts)=year(b.ts)"
                    )?
                    .rows,
                    vec![vec![Value::Integer(4)]]
                );
                assert_eq!(c.query("SELECT year(ts),sum(struct_extract(payload,'budget')) FROM events GROUP BY year(ts) ORDER BY year(ts)")?.rows[0],vec![Value::Integer(2000),Value::Decimal{value:375,width:38,scale:2}]);
                assert_eq!(c.query("SELECT id,epoch_us(min(struct_extract(payload,'occurred')) OVER (ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)) FROM events ORDER BY id")?.rows[0][1],Value::Integer(949363200000000));
                c.execute("BEGIN; UPDATE events SET span=INTERVAL 9 DAY; DELETE FROM events WHERE year(ts)=2000; ROLLBACK")?;
                assert_eq!(c.query(projection)?.rows, rows);
                c.execute(
                "UPDATE events SET samples=[make_timestamp(0),NULL],span=to_hours(2) WHERE id=1",
            )?;
                let committed = c.query("SELECT * FROM events ORDER BY id")?.rows;
                drop(c);
                assert_eq!(
                    open()?
                        .connect()
                        .query("SELECT * FROM events ORDER BY id")?
                        .rows,
                    committed
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedTemporalSyntaxFunction {
    name: &'static str,
    result: Value,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::function::ScalarFunction for SelectedTemporalSyntaxFunction {
    fn name(&self) -> &str {
        self.name
    }
    fn return_type(
        &self,
        _arguments: &[DataType],
        _types: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        Ok(if matches!(self.result, Value::Double(_)) {
            DataType::Double
        } else {
            DataType::BigInt
        })
    }
    fn evaluate(
        &self,
        _arguments: &[Value],
        query: &duckdb_rust::parallel::QueryContext,
    ) -> Result<Value> {
        query.check()?;
        Ok(self.result.clone())
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_syntax_uses_selected_scalar_adapters() -> Result<()> {
    let builtins = duckdb_rust::function::FunctionRegistry::builtins();
    let mut functions = duckdb_rust::function::FunctionRegistry::default();
    functions.register_scalar(Arc::new(SelectedTemporalSyntaxFunction {
        name: "date_part",
        result: Value::Integer(77),
    }))?;
    functions.register_scalar(Arc::new(SelectedTemporalSyntaxFunction {
        name: "trunc",
        result: Value::Double(2.0),
    }))?;
    functions.register_scalar(builtins.scalar("to_days")?)?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    let rows = c
        .query("SELECT EXTRACT(year FROM DATE '2000-01-01'),INTERVAL 9 DAY")?
        .rows;
    assert_eq!(rows[0][0], Value::Integer(77));
    assert_eq!(rows[0][1].to_string(), "2 days");
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn interval_text_components_aliases_rounding_and_ordered_overflow_cross_native_mutations()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("interval-text.duckdb");
    let mut c = Database::open(&path)?.connect();
    c.execute("CREATE TABLE spans(id INTEGER PRIMARY KEY,span INTERVAL)")?;
    let insert = c.prepare("INSERT INTO spans VALUES($1,CAST($2 AS INTERVAL))")?;
    for (id, (text, expected)) in [
        ("2y3mons4d ago", "-2 years -3 months -4 days"),
        ("@ 2y 3mons 4d", "2 years 3 months 4 days"),
        ("123.5", "00:02:03.5"),
        ("1.1quarters", "3 months 9 days"),
        ("1.1years", "1 year 1 month"),
        ("0.0000009 seconds", "00:00:00.000001"),
        ("1.9us", "00:00:00.000001"),
        ("1.9ms", "00:00:00.0019"),
        ("0.1 weeks", "16:48:00"),
        ("1:02:03 junk", "01:02:03"),
        ("1:02:03 4days", "01:02:03"),
        ("1:02:03 ago", "01:02:03"),
        ("1:", "01:00:00"),
    ]
    .into_iter()
    .enumerate()
    {
        c.execute_prepared(
            &insert,
            &[Value::Integer(id as i128), Value::Varchar(text.into())],
        )?;
        assert_eq!(
            c.query(&format!("SELECT span FROM spans WHERE id={id}"))?
                .rows[0][0]
                .to_string(),
            expected
        );
    }
    for text in [
        "+1 day",
        ".5 seconds",
        " @1 day",
        "2147483647 days 1 day -1 day",
        "9223372036854775807 us 1 us -1 us",
        "-2147483648months ago",
        "P1Y",
    ] {
        assert!(
            c.execute_prepared(&insert, &[Value::Integer(100), Value::Varchar(text.into())])
                .is_err(),
            "{text}"
        );
    }
    assert_eq!(
        c.query("SELECT count(*) FROM spans")?.rows[0][0],
        Value::Integer(13)
    );
    let rows = c.query("SELECT * FROM spans ORDER BY id")?.rows;
    c.execute("BEGIN; UPDATE spans SET span=CAST('1.1years' AS INTERVAL); DELETE FROM spans WHERE id=0; ROLLBACK")?;
    assert_eq!(c.query("SELECT * FROM spans ORDER BY id")?.rows, rows);
    drop(c);
    assert_eq!(
        Database::open(&path)?
            .connect()
            .query("SELECT * FROM spans ORDER BY id")?
            .rows,
        rows
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn interval_plural_units_and_conversion_diagnostics_keep_prepared_cast_semantics() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator)
            as Arc<dyn duckdb_rust::execution::expression_executor::ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut c = DatabaseBuilder::new()
            .expressions(evaluator)
            .build()?
            .connect();
        c.execute("CREATE TABLE spans(n INTEGER,i INTERVAL)")?;
        for (unit, expected) in [
            ("YEARS", "2 years"),
            ("MONTHS", "2 months"),
            ("DAYS", "2 days"),
            ("HOURS", "02:00:00"),
            ("MINUTES", "00:02:00"),
            ("SECONDS", "00:00:02"),
            ("MILLISECONDS", "00:00:00.002"),
            ("MICROSECONDS", "00:00:00.000002"),
            ("WEEKS", "14 days"),
            ("QUARTERS", "6 months"),
            ("DECADES", "20 years"),
            ("CENTURIES", "200 years"),
            ("MILLENNIA", "2000 years"),
        ] {
            let prepared =
                c.prepare(&format!("INSERT INTO spans VALUES(2,INTERVAL ($1) {unit})"))?;
            c.execute_prepared(&prepared, &[Value::Integer(2)])?;
            assert_eq!(
                c.query("SELECT i FROM spans")?.rows[0][0].to_string(),
                expected,
                "{unit}"
            );
            c.execute("DELETE FROM spans")?;
        }
        for (input, expected) in [
            ("   ", "Could not convert string"),
            ("AAAA", "Could not convert string"),
            ("47.210 5", "Could not convert string"),
            (
                "3 DOOPIEDOOS",
                "extract specifier \"DOOPIEDOOS\" not recognized",
            ),
            (
                "3 years 2 doy",
                "extract specifier \"doy\" not supported for interval",
            ),
            (
                "3 yearweek",
                "extract specifier \"yearweek\" not supported for interval",
            ),
            (
                "2147483648 days",
                "out of range for the destination type INT32",
            ),
            (
                "9223372036854775807us 1us",
                "interval value is out of range",
            ),
            (
                "-2147483648months ago",
                "AGO interval value is out of range",
            ),
        ] {
            let cast = c.prepare("SELECT CAST($1 AS INTERVAL)")?;
            let error = c
                .execute_prepared(&cast, &[Value::Varchar(input.into())])
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{input}: {error}");
            let try_cast = c.prepare("SELECT TRY_CAST($1 AS INTERVAL)")?;
            assert_eq!(
                c.execute_prepared(&try_cast, &[Value::Varchar(input.into())])?
                    .rows,
                vec![vec![Value::Null]]
            );
        }
        for qualifier in [
            "YEARS TO MONTHS",
            "DAYS TO HOURS",
            "HOURS TO MINUTES",
            "DECADES TO YEARS",
        ] {
            assert!(
                matches!(c.query(&format!("SELECT INTERVAL '2 10' {qualifier}")), Err(duckdb_rust::Error::Parse(message)) if message.contains("not supported"))
            );
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn clock_text_offsets_timestamp_fallback_and_suffixes_cross_native_wal_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        let path = directory.path().join(format!("clock-{batched}.duckdb"));
        Database::open(&path)?.connect().execute(
            "CREATE TABLE t(id INTEGER PRIMARY KEY,n TIME_NS,z TIMETZ,ts TIMESTAMPTZ_NS UNIQUE)",
        )?;
        let open = || {
            let checkpoint = FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(DuckDbFormat::default()),
            )?
            .with_recovery(Arc::new(DuckDbWalRecovery))?;
            DatabaseBuilder::new()
                .batch_size(1)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .durability(Arc::new(FileWal::new(
                    checkpoint,
                    Arc::new(DuckDbTransactionLog),
                )?))
                .build()
        };
        let mut c = open()?.connect();
        let insert = c.prepare("INSERT INTO t VALUES ($1,CAST($2 AS TIME_NS),CAST($3 AS TIMETZ),CAST($4 AS TIMESTAMPTZ_NS))")?;
        for (id, n, z, ts) in [
            (
                1,
                "2000-01-01 12:34:56.123456789 America/New_York",
                "2000-01-01 12:34:56+02",
                "2000 01 02 1:02:03.000000001 UTC",
            ),
            (
                2,
                "1:02:",
                "12:34:56+00:99:99junk",
                "2000-01-01 12:34:56+99:99:99",
            ),
            (
                3,
                "12:34:56.000000001ignored",
                "12:34:56 +02",
                "2000-01-01 1:",
            ),
        ] {
            c.execute_prepared(
                &insert,
                &[
                    Value::Integer(id),
                    Value::Varchar(n.into()),
                    Value::Varchar(z.into()),
                    Value::Varchar(ts.into()),
                ],
            )?;
        }
        let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
        assert_eq!(before[0][1].to_string(), "12:34:56.123456789");
        assert_eq!(before[0][2].to_string(), "10:34:56+00");
        assert_eq!(before[0][3].to_string(), "2000-01-02 01:02:03.000000001+00");
        assert_eq!(before[1][2].to_string(), "12:34:56+01:40:39");
        assert_eq!(before[1][3].to_string(), "1999-12-28 07:54:17+00");
        assert_eq!(
            c.query("SELECT count(*) FROM t a JOIN t b ON a.ts=b.ts")?
                .rows[0][0],
            Value::Integer(3)
        );
        c.execute("BEGIN; UPDATE t SET n=TIME_NS '1:',z=TIMETZ '1:02:'; DELETE FROM t WHERE id=2; ROLLBACK")?;
        assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
        drop(c);
        let mut c = open()?.connect();
        assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
        c.checkpoint()?;
        drop(c);
        let mut c = Database::open_read_only(&path)?.connect();
        assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
        for sql in [
            "SELECT TIMETZ '12:34:56Z'",
            "SELECT TIMETZ '12:34:56+2'",
            "SELECT TIMESTAMP '2000-01-01 12:34:56+000000'",
            "SELECT TIMESTAMP '2000-01-01 '",
            "SELECT TIMESTAMPTZ '2000-01-01 12:34:56 America/New_York'",
            "SELECT TIMESTAMP '2000-01-01 12:34:56  UTC'",
        ] {
            assert!(c.query(sql).is_err(), "{sql}");
        }
    }
    Ok(())
}

#[path = "../obligations/clock_domain.rs"]
mod clock_domain;

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn clock_physical_domain_crosses_casts_nested_checkpoints_keys_and_calendar_rollover() -> Result<()>
{
    clock_domain::run(false)
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn clock_physical_domain_crosses_nested_native_wal_keys_mutations_and_reopen() -> Result<()> {
    clock_domain::run(true)
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn interval_cast_categories_remain_exact_while_try_cast_suppresses_only_local_input_failures()
-> Result<()> {
    use duckdb_rust::Error;
    for batched in [false, true] {
        let mut c = DatabaseBuilder::new()
            .batch_size(1)
            .expressions(if batched {
                Arc::new(BatchedEvaluator)
            } else {
                Arc::new(ScalarEvaluator)
            })
            .build()?
            .connect();
        let strict = c.prepare("SELECT CAST($1 AS INTERVAL)")?;
        let tolerant = c.prepare("SELECT TRY_CAST($1 AS INTERVAL)")?;
        for text in [
            "2147483648 days",
            "-2147483649 months",
            "9223372036854775808us",
        ] {
            let input = [Value::Varchar(text.into())];
            let error = c.execute_prepared(&strict, &input).unwrap_err();
            assert!(matches!(error, Error::InvalidInput(_)), "{text}: {error}");
            assert_eq!(
                c.execute_prepared(&tolerant, &input)?.rows,
                vec![vec![Value::Null]]
            );
        }
        for text in [
            "9223372036854775807us 1us",
            "-2147483648 months ago",
            "9223372036854775807 hours",
            "2147483647 months 0.1 years",
        ] {
            let input = [Value::Varchar(text.into())];
            let error = c.execute_prepared(&strict, &input).unwrap_err();
            assert!(matches!(error, Error::OutOfRange(_)), "{text}: {error}");
            assert_eq!(
                c.execute_prepared(&tolerant, &input)?.rows,
                vec![vec![Value::Null]]
            );
        }
        assert!(matches!(
            c.query("SELECT TRY_CAST(CAST('2147483648 days' AS INTERVAL) AS VARCHAR)"),
            Err(Error::InvalidInput(_))
        ));
        assert!(
            c.query("SELECT TRY_CAST(make_time(25,0,0) AS VARCHAR)")
                .is_err()
        );
        assert_eq!(
            c.query("SELECT TRY_CAST(['1 day','2147483648 days','9223372036854775807us 1us'] AS INTERVAL[])::VARCHAR")?.rows,
            vec![vec![Value::Varchar("[1 day, NULL, NULL]".into())]]
        );
        assert!(matches!(
            c.query("SELECT CAST(['1 day','2147483648 days'] AS INTERVAL[])"),
            Err(Error::InvalidInput(_))
        ));
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_text_errors_distinguish_format_range_offsets_and_precision_without_swallowing_children()
-> Result<()> {
    use duckdb_rust::Error;
    for batched in [false, true] {
        let mut c = DatabaseBuilder::new()
            .batch_size(1)
            .expressions(if batched {
                Arc::new(BatchedEvaluator)
            } else {
                Arc::new(ScalarEvaluator)
            })
            .build()?
            .connect();
        for (kind, input, message) in [
            ("TIME", "50:42:04.500", "time field value out of range"),
            ("TIME_NS", "14-42-04", "time field value out of range"),
            ("TIMETZ", "12:00:00+99", "time field value out of range"),
            ("TIMESTAMP", "blabla", "invalid timestamp field format"),
            (
                "TIMESTAMP",
                "1900-1-1 00:99:23",
                "invalid timestamp field format",
            ),
            (
                "TIMESTAMP",
                "1900-1-1 59:59:23",
                "timestamp field value out of range",
            ),
            (
                "TIMESTAMP",
                "1993-20-14 00:00:00",
                "timestamp field value out of range",
            ),
            (
                "TIMESTAMPTZ",
                "1900-02-29",
                "timestamp field value out of range",
            ),
            (
                "TIMESTAMP",
                "294247-01-10 04:00:54.775807",
                "timestamp field value out of range",
            ),
            (
                "TIMESTAMP",
                "2000-01-01 12:00:00+2",
                "has a timestamp that is not UTC",
            ),
            (
                "TIMESTAMPTZ",
                "2000-01-01 12:00:00 America/New_York",
                "has a timestamp that is not UTC",
            ),
            (
                "TIMESTAMP_NS",
                "2262-04-11 23:47:16.854775807",
                "Could not convert string",
            ),
            ("TIMESTAMP_S", "1900-02-29", "Could not convert string"),
            ("TIMESTAMP_MS", "blabla", "Could not convert string"),
            (
                "TIMESTAMPTZ_NS",
                "2262-04-11 23:47:16.854775807",
                "invalid timestamp field format",
            ),
        ] {
            let strict = c.prepare(&format!("SELECT CAST($1 AS {kind})"))?;
            let tolerant = c.prepare(&format!("SELECT TRY_CAST($1 AS {kind})"))?;
            let input = [Value::Varchar(input.into())];
            assert!(
                matches!(c.execute_prepared(&strict, &input), Err(Error::Conversion(error)) if error.contains(message)),
                "{kind}: {input:?}"
            );
            assert_eq!(
                c.execute_prepared(&tolerant, &input)?.rows,
                vec![vec![Value::Null]]
            );
        }
        assert_eq!(c.query("SELECT TIMESTAMP 'inf'::VARCHAR,TIMESTAMPTZ_NS '-INF'::VARCHAR,TRY_CAST('inf ' AS TIMESTAMP),TRY_CAST('inf' AS TIME)")?.rows, vec![vec![Value::Varchar("infinity".into()), Value::Varchar("-infinity".into()), Value::Null, Value::Null]]);
        assert!(
            c.query("SELECT TRY_CAST(CAST('blabla' AS TIMESTAMP) AS VARCHAR)")
                .is_err()
        );
        let statement = c.prepare("SELECT CAST($1 AS TIMESTAMP)")?;
        let error = c
            .execute_prepared(&statement, &[Value::Varchar("🌏".repeat(100_000))])
            .unwrap_err();
        assert!(error.to_string().len() < 1024);
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_timestamp_intermediate_overflow_preserves_mutations_indexes_wal_and_reopen()
-> Result<()> {
    use duckdb_rust::Error;
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        let path = directory
            .path()
            .join(format!("calendar-boundary-{batched}.duckdb"));
        let open = || {
            let checkpoint = FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(DuckDbFormat::default()),
            )?
            .with_recovery(Arc::new(DuckDbWalRecovery))?;
            DatabaseBuilder::new()
                .batch_size(1)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .durability(Arc::new(FileWal::new(
                    checkpoint,
                    Arc::new(DuckDbTransactionLog),
                )?))
                .build()
        };
        let mut c = open()?.connect();
        c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,t TIMESTAMP UNIQUE); INSERT INTO t VALUES(1,TIMESTAMP '290309-12-22 (BC) 00:00:00'),(2,TIMESTAMP '294247-01-10 04:00:54.775806')")?;
        let before = c.query("SELECT * FROM t ORDER BY t")?.rows;
        for (sql, category, message) in [
            (
                "SELECT TIMESTAMP '290309-12-21 (BC) 23:59:59.999999'",
                0,
                "timestamp field value out of range",
            ),
            (
                "SELECT DATE '290309-12-21 (BC)'+TIME '23:59:59.999999'",
                1,
                "Timestamp out of range",
            ),
            (
                "SELECT DATE '294247-01-10'+TIMETZ '04:00:54.775807+01'",
                1,
                "Timestamp with time zone out of range",
            ),
            (
                "SELECT make_timestamp(-290308,12,21,23,59,59.999999)",
                0,
                "Date and time not in timestamp range",
            ),
            (
                "SELECT DATE '290309-12-21 (BC)'+INTERVAL '1day'",
                0,
                "Date and time not in timestamp range",
            ),
            (
                "UPDATE t SET t=t-INTERVAL '1us' WHERE id=1",
                0,
                "Date and time not in timestamp range",
            ),
            (
                "UPDATE t SET t=t+INTERVAL '1us' WHERE id=2",
                0,
                "Date and time not in timestamp range",
            ),
            (
                "UPDATE t SET t=t-INTERVAL '1month' WHERE id=1",
                0,
                "Date and time not in timestamp range",
            ),
            (
                "UPDATE t SET t=t+INTERVAL '1month' WHERE id=2",
                0,
                "Date and time not in timestamp range",
            ),
        ] {
            let error = c.query(sql).unwrap_err();
            assert!(
                if category == 0 {
                    matches!(error, Error::Conversion(_))
                } else {
                    matches!(error, Error::OutOfRange(_))
                },
                "{sql}: {error}"
            );
            assert!(error.to_string().contains(message), "{sql}: {error}");
            assert_eq!(c.query("SELECT * FROM t ORDER BY t")?.rows, before);
        }
        let raw = c.prepare("SELECT CAST($1 AS TIMESTAMP)+INTERVAL '1day'")?;
        assert!(
            matches!(c.execute_prepared(&raw, &[Value::Temporal(TemporalValue::Timestamp(-i64::MAX+1))]), Err(Error::Conversion(message)) if message == "Date out of range in timestamp conversion")
        );
        assert_eq!(
            c.query("SELECT t+INTERVAL '-1day 24hours' FROM t ORDER BY t")?
                .rows,
            before
                .iter()
                .map(|row| vec![row[1].clone()])
                .collect::<Vec<_>>()
        );
        assert_eq!(
            c.query("SELECT count(*) FROM t a JOIN t b ON a.t=b.t")?
                .rows,
            vec![vec![Value::Integer(2)]]
        );
        assert_eq!(
            c.query("SELECT min(t) OVER(),max(t) OVER() FROM t ORDER BY t")?
                .rows,
            vec![vec![before[0][1].clone(), before[1][1].clone()]; 2]
        );
        c.execute(
            "BEGIN; UPDATE t SET t=t+INTERVAL '1us' WHERE id=1; DELETE FROM t WHERE id=2; ROLLBACK",
        )?;
        drop(c);
        let mut c = open()?.connect();
        assert_eq!(c.query("SELECT * FROM t ORDER BY t")?.rows, before);
        c.execute("UPDATE t SET t=t+INTERVAL '1us' WHERE id=1; UPDATE t SET t=t-INTERVAL '1us' WHERE id=2")?;
        let after = c.query("SELECT * FROM t ORDER BY t")?.rows;
        drop(c);
        let mut c = open()?.connect();
        assert_eq!(c.query("SELECT * FROM t ORDER BY t")?.rows, after);
        c.checkpoint()?;
        drop(c);
        assert_eq!(
            Database::open_read_only(&path)?
                .connect()
                .query("SELECT * FROM t ORDER BY t")?
                .rows,
            after
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_diagnostic_display_does_not_change_physical_validity_or_fatal_sql_rendering()
-> Result<()> {
    use duckdb_rust::Error;
    for raw in [
        TemporalValue::TimestampS(i64::MAX - 1),
        TemporalValue::TimestampS(-i64::MAX + 1),
        TemporalValue::TimestampMs(i64::MAX - 1),
        TemporalValue::TimestampMs(-i64::MAX + 1),
        TemporalValue::Timestamp(-i64::MAX + 1),
        TemporalValue::TimestampNs(-i64::MAX + 1),
    ] {
        raw.validate()?;
        assert!(raw.check_text_renderable().is_err());
        assert!(!raw.to_string().is_empty());
    }
    // Even an invalid public diagnostic payload must not overflow abs(i32::MIN).
    assert!(
        !TemporalValue::TimeTz {
            micros: i64::MIN,
            offset: i32::MIN
        }
        .to_string()
        .is_empty()
    );
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        let path = directory
            .path()
            .join(format!("raw-timestamp-{batched}.duckdb"));
        let open = || {
            let checkpoint = FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(DuckDbFormat::default()),
            )?
            .with_recovery(Arc::new(DuckDbWalRecovery))?;
            DatabaseBuilder::new()
                .batch_size(1)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .durability(Arc::new(FileWal::new(
                    checkpoint,
                    Arc::new(DuckDbTransactionLog),
                )?))
                .build()
        };
        let mut c = open()?.connect();
        c.execute("CREATE TABLE raw(id INTEGER PRIMARY KEY,t TIMESTAMP_S UNIQUE)")?;
        let insert = c.prepare("INSERT INTO raw VALUES($1,CAST($2 AS TIMESTAMP_S))")?;
        let original = Value::Temporal(TemporalValue::TimestampS(i64::MAX - 1));
        c.execute_prepared(&insert, &[Value::Integer(1), original.clone()])?;
        assert_eq!(
            c.query("SELECT t FROM raw")?.rows,
            vec![vec![original.clone()]]
        );
        for sql in [
            "SELECT CAST(t AS VARCHAR) FROM raw",
            "SELECT TRY_CAST(t AS VARCHAR) FROM raw",
            "SELECT CAST(make_timestamp(-9223372036854775806) AS VARCHAR)",
            "SELECT TRY_CAST(make_timestamp_ns(-9223372036854775806) AS VARCHAR)",
            "SELECT CAST([t] AS VARCHAR) FROM raw",
            "SELECT TRY_CAST({'t':t} AS VARCHAR) FROM raw",
            "SELECT TRY_CAST(t::VARIANT AS VARCHAR) FROM raw",
        ] {
            assert!(
                matches!(c.query(sql), Err(Error::Internal(message)) if message.contains("not marked as fallible")),
                "{sql}"
            );
        }
        assert_eq!(
            c.query("SELECT count(*) FROM raw a JOIN raw b ON a.t=b.t")?
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        c.execute("BEGIN; DELETE FROM raw; ROLLBACK")?;
        drop(c);
        let mut c = open()?.connect();
        assert_eq!(
            c.query("SELECT t FROM raw")?.rows,
            vec![vec![original.clone()]]
        );
        assert!(c.query("SELECT TRY_CAST(t AS VARCHAR) FROM raw").is_err());
        c.checkpoint()?;
        drop(c);
        assert_eq!(
            Database::open_read_only(&path)?
                .connect()
                .query("SELECT t FROM raw")?
                .rows,
            vec![vec![original]]
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn text_result_transports_reject_unrenderable_temporals_before_emitting_diagnostic_fallbacks()
-> Result<()> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    for json in [false, true] {
        for sql in [
            "SELECT make_timestamp(-9223372036854775806)",
            "SELECT {'t':make_timestamp_ns(-9223372036854775806)}",
        ] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_duckdb-rust"));
            if json {
                command.arg("--json");
            }
            let output = command.args(["-c", sql]).output()?;
            assert!(!output.status.success(), "{sql}");
            assert!(output.stdout.is_empty(), "must reject before headers/rows");
            assert!(String::from_utf8_lossy(&output.stderr).contains("Date out of range"));
        }
    }
    let mut worker = Command::new(env!("CARGO_BIN_EXE_duckdb-rust-test-worker"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut input = worker.stdin.take().unwrap();
    for sql in [
        "SELECT make_timestamp(-9223372036854775806)",
        "SELECT [make_timestamp_ns(-9223372036854775806)]",
        "SELECT 42",
    ] {
        writeln!(
            input,
            "{}",
            serde_json::json!({"operation":"query", "sql":sql})
        )?;
    }
    drop(input);
    let output = worker.wait_with_output()?;
    assert!(output.status.success());
    let rows = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["ok"], false);
    assert_eq!(rows[1]["ok"], false);
    assert_eq!(rows[2]["ok"], true);
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nanosecond_text_checks_scale_before_remainder_without_narrowing_epoch_values() -> Result<()> {
    use duckdb_rust::Error;
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        let path = directory
            .path()
            .join(format!("nanosecond-product-{batched}.duckdb"));
        let open = || {
            let checkpoint = FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(DuckDbFormat::default()),
            )?
            .with_recovery(Arc::new(DuckDbWalRecovery))?;
            DatabaseBuilder::new()
                .batch_size(1)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .durability(Arc::new(FileWal::new(
                    checkpoint,
                    Arc::new(DuckDbTransactionLog),
                )?))
                .build()
        };
        let mut c = open()?.connect();
        for kind in ["TIMESTAMP_NS", "TIMESTAMPTZ_NS"] {
            let strict = c.prepare(&format!("SELECT epoch_ns(CAST($1 AS {kind}))"))?;
            let tolerant = c.prepare(&format!("SELECT epoch_ns(TRY_CAST($1 AS {kind}))"))?;
            for fraction in ["145224194", "145224500", "145224999"] {
                let input = [Value::Varchar(format!("1677-09-21 00:12:43.{fraction}"))];
                assert!(matches!(
                    c.execute_prepared(&strict, &input),
                    Err(Error::Conversion(_))
                ));
                assert_eq!(
                    c.execute_prepared(&tolerant, &input)?.rows,
                    vec![vec![Value::Null]]
                );
            }
            for (fraction, ticks) in [
                ("145225000", -9_223_372_036_854_775_000_i128),
                ("145225001", -9_223_372_036_854_774_999),
            ] {
                assert_eq!(
                    c.execute_prepared(
                        &strict,
                        &[Value::Varchar(format!("1677-09-21 00:12:43.{fraction}"))]
                    )?
                    .rows,
                    vec![vec![Value::Integer(ticks)]]
                );
            }
        }
        assert_eq!(c.query("SELECT epoch_ns(make_timestamp_ns(-9223372036854775806)),TIME_NS '1677-09-21 00:12:43.145224194'")?.rows, vec![vec![Value::Integer(-9_223_372_036_854_775_806), Value::Temporal(TemporalValue::TimeNs(763_145_224_194))]]);
        c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,t TIMESTAMP_NS UNIQUE); INSERT INTO t VALUES(1,TIMESTAMP_NS '1677-09-21 00:12:43.145225000')")?;
        let before = c.query("SELECT * FROM t")?.rows;
        assert!(
            c.execute("UPDATE t SET t=TIMESTAMP_NS '1677-09-21 00:12:43.145224999'")
                .is_err()
        );
        assert_eq!(c.query("SELECT * FROM t")?.rows, before);
        c.execute("BEGIN; DELETE FROM t; ROLLBACK")?;
        drop(c);
        let mut c = open()?.connect();
        assert_eq!(c.query("SELECT * FROM t")?.rows, before);
        assert_eq!(
            c.query("SELECT count(*) FROM t WHERE t=make_timestamp_ns(-9223372036854775000)")?
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        c.checkpoint()?;
        drop(c);
        assert_eq!(
            Database::open_read_only(&path)?
                .connect()
                .query("SELECT * FROM t")?
                .rows,
            before
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nanosecond_and_timetz_keys_keep_exact_overloads_nulls_and_fatal_boundaries() -> Result<()> {
    use duckdb_rust::Error;
    for batched in [false, true] {
        let mut c = DatabaseBuilder::new()
            .batch_size(1)
            .expressions(if batched {
                Arc::new(BatchedEvaluator)
            } else {
                Arc::new(ScalarEvaluator)
            })
            .build()?
            .connect();
        for (input, expected) in [
            ("DATE '2000-01-01'", 0),
            ("TIME '12:00:59.123456'", 59_123_456_000),
            ("TIME_NS '24:00:00.000000999'", 999),
            ("make_time(23,59,60.5)::TIME_NS", 500_000_000),
            ("TIMETZ '23:59:59.999999+02'", 59_999_999_000),
            ("INTERVAL '-1us'", -1000),
            ("TIMESTAMP_S '1969-12-31 23:59:59'", 59_000_000_000),
            ("TIMESTAMP_MS '1969-12-31 23:59:59.999'", 59_999_000_000),
            (
                "TIMESTAMP_NS '1969-12-31 23:59:59.999999999'",
                59_999_999_999,
            ),
            (
                "TIMESTAMPTZ_NS '1969-12-31 23:59:59.123456789+02'",
                59_123_456_789,
            ),
            (
                "TIMESTAMPTZ '1969-12-31 23:59:59.123456+02'",
                59_123_456_000,
            ),
            ("make_timestamp(-9223372036854775806)", 5_224_194_000),
        ] {
            let result = c.query(&format!("SELECT nanosecond({input})"))?;
            assert_eq!(result.columns[0].data_type, DataType::BigInt);
            assert_eq!(result.rows, vec![vec![Value::Integer(expected)]], "{input}");
        }
        for kind in [
            "DATE",
            "TIME",
            "TIME_NS",
            "TIMETZ",
            "TIMESTAMP",
            "TIMESTAMP_S",
            "TIMESTAMP_MS",
            "TIMESTAMP_NS",
            "TIMESTAMPTZ",
            "TIMESTAMPTZ_NS",
            "INTERVAL",
        ] {
            assert_eq!(
                c.query(&format!("SELECT nanosecond(NULL::{kind})"))?.rows,
                vec![vec![Value::Null]]
            );
        }
        for kind in [
            "DATE",
            "TIMESTAMP",
            "TIMESTAMP_NS",
            "TIMESTAMPTZ",
            "TIMESTAMPTZ_NS",
        ] {
            assert_eq!(
                c.query(&format!(
                    "SELECT nanosecond({kind} 'infinity'),nanosecond({kind} '-infinity')"
                ))?
                .rows,
                vec![vec![Value::Null, Value::Null]]
            );
        }
        for sql in [
            "SELECT nanosecond(NULL)",
            "SELECT nanosecond(42)",
            "SELECT timetz_byte_comparable(TIME '12:00:00')",
        ] {
            assert!(matches!(c.query(sql), Err(Error::Bind(_))), "{sql}");
        }
        assert!(c.query("SELECT nanoseconds(TIME '12:00:00')").is_err());
        assert!(matches!(
            c.query("SELECT date_part('nanosecond',TIME_NS '12:34:56.123456789')"),
            Err(Error::Conversion(_))
        ));
        let strict = c.prepare("SELECT nanosecond(CAST($1 AS TIMESTAMP_NS))")?;
        let tolerant =
            c.prepare("SELECT TRY_CAST(nanosecond(CAST($1 AS TIMESTAMP_NS)) AS VARCHAR)")?;
        for prepared in [&strict, &tolerant] {
            assert!(
                matches!(c.execute_prepared(prepared, &[Value::Temporal(TemporalValue::TimestampNs(-i64::MAX+1))]), Err(Error::Internal(message)) if message.contains("\"\"nanosecond\"\"") && message.contains("Date out of range in timestamp_ns conversion"))
            );
        }
        let key = c.query("SELECT timetz_byte_comparable(TIMETZ '12:00:00+00'),timetz_byte_comparable(NULL),timetz_byte_comparable(make_time(23,59,60.5)::TIMETZ)")?;
        assert!(
            key.columns
                .iter()
                .all(|column| column.data_type == DataType::UBigInt)
        );
        assert_eq!(
            key.rows,
            vec![vec![
                Value::Unsigned(1_691_126_595_584_057_599),
                Value::Null,
                Value::Unsigned(2_415_910_715_392_057_599)
            ]]
        );
        let key_statement = c.prepare("SELECT timetz_byte_comparable(CAST($1 AS TIMETZ))")?;
        let mut keys = Vec::new();
        for micros in [0, 1, 86_399_999_999, 86_400_000_000, 86_400_500_000] {
            for offset in [-57599, -1, 0, 1, 57599] {
                let clock = TemporalValue::TimeTz { micros, offset };
                let result = c.execute_prepared(&key_statement, &[Value::Temporal(clock)])?;
                let Value::Unsigned(key) = result.rows[0][0] else {
                    panic!("TIMETZ key lost its selected unsigned result");
                };
                assert!(key <= u128::from(u64::MAX));
                keys.push((clock, key));
            }
        }
        for (left, left_key) in &keys {
            for (right, right_key) in &keys {
                assert_eq!(left.compare(*right)?, left_key.cmp(right_key));
                assert_eq!(left == right, left_key == right_key);
            }
        }
        for (spelling, expected_type) in [
            ("DATETIME", DataType::Timestamp),
            ("DATETIME(0)", DataType::TimestampS),
            ("DATETIME(3)", DataType::TimestampMs),
            ("DATETIME(6)", DataType::Timestamp),
            ("DATETIME(9)", DataType::TimestampNs),
            ("TIMESTAMP_US", DataType::Timestamp),
        ] {
            let result = c.query(&format!(
                "SELECT {spelling} '2000-01-01 12:00:00.123456789',CAST(NULL AS {spelling})"
            ))?;
            assert!(
                result
                    .columns
                    .iter()
                    .all(|column| column.data_type == expected_type),
                "{spelling}"
            );
            assert!(result.rows[0][1].is_null());
        }
        for precision in [10, 11] {
            assert!(
                matches!(c.query(&format!("SELECT NULL::DATETIME({precision})")), Err(Error::Bind(message)) if message.contains("TIMESTAMP only supports until nano-second precision (9)"))
            );
        }
        assert!(
            matches!(c.query("SELECT NULL::TIMESTAMP_US(3)"), Err(Error::Bind(message)) if message.contains("does not take any type parameters"))
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_function_keys_cross_prepared_nested_execution_and_native_recovery() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        for hashed in [false, true] {
            let path = directory
                .path()
                .join(format!("temporal-function-keys-{batched}-{hashed}.duckdb"));
            let open = || {
                let checkpoint = FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    Arc::new(DuckDbFormat::default()),
                )?
                .with_recovery(Arc::new(DuckDbWalRecovery))?;
                DatabaseBuilder::new()
                    .batch_size(1)
                    .expressions(if batched {
                        Arc::new(BatchedEvaluator)
                    } else {
                        Arc::new(ScalarEvaluator)
                    })
                    .indexes(if hashed {
                        Arc::new(HashIndexFactory)
                    } else {
                        Arc::new(BTreeIndexFactory)
                    })
                    .durability(Arc::new(FileWal::new(
                        checkpoint,
                        Arc::new(DuckDbTransactionLog),
                    )?))
                    .build()
            };
            let mut c = open()?.connect();
            c.execute("CREATE TABLE events(id INTEGER PRIMARY KEY,ts DATETIME(9) UNIQUE,legacy TIMESTAMP_US,z TIMETZ,payload STRUCT(ts TIMESTAMP_NS,clock TIMETZ),extracted BIGINT,k UBIGINT UNIQUE)")?;
            let insert = c.prepare("INSERT INTO events SELECT $1,$2::TIMESTAMP_NS,$2::TIMESTAMP_NS::TIMESTAMP_US,$3::TIMETZ,{'ts':$2::TIMESTAMP_NS,'clock':$3::TIMETZ},nanosecond($2::TIMESTAMP_NS),timetz_byte_comparable($3::TIMETZ)")?;
            for (id, time, clock) in [
                (1, "1969-12-31 23:59:59.999999999", "13:00:00+01"),
                (2, "2000-01-01 12:00:59.123456789", "12:00:00+00"),
                (
                    3,
                    "2000-01-02 12:00:59.123456789",
                    "23:59:59.999999-15:59:59",
                ),
            ] {
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(id),
                        Value::Temporal(TemporalValue::parse(time, &DataType::TimestampNs)?),
                        Value::Temporal(TemporalValue::parse(clock, &DataType::TimeTz)?),
                    ],
                )?;
            }
            c.execute_prepared(&insert, &[Value::Integer(4), Value::Null, Value::Null])?;
            let projection = "SELECT id,nanosecond(ts),nanosecond(struct_extract(payload,'ts')),extracted,k,timetz_byte_comparable(z) FROM events ORDER BY id";
            let before = c.query(projection)?.rows;
            for row in &before {
                assert_eq!(row[1], row[2]);
                assert_eq!(row[1], row[3]);
                assert_eq!(row[4], row[5]);
            }
            assert_eq!(before[0][1], Value::Integer(59_999_999_999));
            assert_eq!(c.query("SELECT count(*) FROM events a JOIN events b ON nanosecond(a.ts)=nanosecond(b.ts)")?.rows, vec![vec![Value::Integer(5)]]);
            assert_eq!(c.query("SELECT nanosecond(ts),count(*) FROM events GROUP BY nanosecond(ts) ORDER BY nanosecond(ts)")?.rows, vec![vec![Value::Integer(59_123_456_789),Value::Integer(2)],vec![Value::Integer(59_999_999_999),Value::Integer(1)],vec![Value::Null,Value::Integer(1)]]);
            assert_eq!(
                c.query("SELECT id FROM events ORDER BY z")?.rows,
                c.query("SELECT id FROM events ORDER BY k")?.rows
            );
            assert_eq!(
                c.query(
                    "SELECT count(*) FROM events a JOIN events b ON true WHERE (a.z<b.z)<>(a.k<b.k)"
                )?
                .rows,
                vec![vec![Value::Integer(0)]]
            );
            assert_eq!(c.query("SELECT max(nanosecond(ts)) OVER (ORDER BY k ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM events ORDER BY k")?.rows, vec![vec![Value::Integer(59_999_999_999)];4]);
            assert!(
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(5),
                        Value::Temporal(TemporalValue::TimestampNs(500)),
                        Value::Temporal(TemporalValue::parse("12:00:00+00", &DataType::TimeTz)?)
                    ]
                )
                .is_err()
            );
            assert_eq!(c.query(projection)?.rows, before);
            c.execute("BEGIN; UPDATE events SET extracted=nanosecond(make_timestamp_ns(500)) WHERE id=1; DELETE FROM events WHERE k=timetz_byte_comparable(TIMETZ '12:00:00+00'); ROLLBACK")?;
            assert_eq!(c.query(projection)?.rows, before);
            c.execute("UPDATE events SET ts=make_timestamp_ns(500),extracted=nanosecond(make_timestamp_ns(500)),payload={'ts':make_timestamp_ns(500),'clock':TIMETZ '13:00:00+01'} WHERE id=1")?;
            let committed = c.query(projection)?.rows;
            assert_eq!(committed[0][1], Value::Integer(500));
            drop(c);
            let mut c = open()?.connect();
            assert_eq!(c.query(projection)?.rows, committed);
            assert_eq!(
                c.query(
                    "SELECT id FROM events WHERE k=timetz_byte_comparable(TIMETZ '12:00:00+00')"
                )?
                .rows,
                vec![vec![Value::Integer(2)]]
            );
            c.checkpoint()?;
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?
                    .connect()
                    .query(projection)?
                    .rows,
                committed
            );
        }
    }
    Ok(())
}
