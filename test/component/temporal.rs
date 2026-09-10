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
        TemporalValue::TimeNs(86_400_000_000_001),
        TemporalValue::Timestamp(i64::MIN),
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
        ("epoch(TIMETZ '13:00:00+01')", "46800"),
        ("epoch_us(TIME_NS '00:00:00.000000001')", "0"),
        ("epoch_ms(-1)", "1969-12-31 23:59:59.999"),
        ("isinf(TIMESTAMP 'infinity')", "true"),
        ("isfinite(INTERVAL '1 year')", "true"),
        ("epoch(TIMESTAMP 'infinity')", "NULL"),
        ("year(DATE '-infinity')", "NULL"),
        ("date_part('epoch',TIMESTAMP 'epoch')", "0"),
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
        "SELECT make_timestamp_ns('-9223372036854775808'::BIGINT)",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    Ok(())
}
