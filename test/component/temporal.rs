use duckdb_rust::{DataType, Database, Result, TemporalValue, Value};

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
    assert_eq!(c.query("SELECT INTERVAL '1 month' = INTERVAL '30 days', TIMETZ '13:00+01' = TIMETZ '12:00+00', TIMETZ '13:00+01' < TIMETZ '12:00+00', TRY_CAST('25:00' AS TIME)")?.rows, vec![vec![Value::Boolean(true),Value::Boolean(false),Value::Boolean(true),Value::Null]]);
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
