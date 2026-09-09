use std::{io::Read, sync::Arc};

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Date, Error, Result, Value,
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec, DateCast},
        type_registry::builtin_types,
    },
    execution::{
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        operator::join::{HashJoin, JoinAlgorithm, NestedLoopJoin},
        physical_plan::NativePhysicalPlanner,
    },
    parallel::QueryContext,
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::{
            DuckDbFormat,
            compression::{self, ScalarBitPackingDecoder},
        },
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

fn date(text: &str) -> Value {
    Value::Date(text.parse().expect("test date"))
}

#[test]
fn calendar_covers_gregorian_cycles_bce_and_physical_boundaries() -> Result<()> {
    for (text, days) in [
        ("1970-01-01", 0),
        ("1969-12-31", -1),
        ("2000-01-01", 10957),
        ("0001-01-01", -719162),
        ("0001-01-01 (BC)", -719528),
        ("5877642-06-25 (BC)", -2147483646),
        ("5881580-07-10", 2147483646),
        ("-infinity", -2147483647),
        ("infinity", 2147483647),
    ] {
        let value: Date = text.parse()?;
        assert_eq!(value.days(), days);
        assert_eq!(Date::from_days(days)?.to_string(), text);
        assert_eq!(
            serde_json::from_str::<Date>(&serde_json::to_string(&value).unwrap()).unwrap(),
            value
        );
    }
    assert!(Date::from_days(i32::MIN).is_err());
    assert!(serde_json::from_str::<Date>("-2147483648").is_err());
    assert!(serde_json::from_str::<Value>(r#"{"Date":-2147483648}"#).is_err());
    assert_eq!(Date::INFINITY.to_ymd(), None);
    assert_eq!(Date::NEG_INFINITY.to_ymd(), None);
    // Independently enumerate calendar days through two complete Gregorian
    // cycles, spanning year zero and non-leap centuries in both directions.
    let mut days = -865625; // -0400-01-01, 400 years before astronomical year 0.
    for year in -400..400 {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        for (month, length) in [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ]
        .into_iter()
        .enumerate()
        {
            for day in 1..=length {
                let ymd = (year, (month + 1) as u8, day);
                assert_eq!(Date::from_days(days)?.to_ymd(), Some(ymd));
                assert_eq!(Date::from_ymd(ymd.0, ymd.1, ymd.2)?.days(), days);
                days += 1;
            }
        }
    }
    for days in (0..10000_i32).map(|v| v.wrapping_mul(0x618225b1)) {
        let value = Date::from_days(days)?;
        assert_eq!(value.to_string().parse::<Date>()?, value);
    }
    Ok(())
}

#[test]
fn literals_casts_invalid_dates_and_lazy_errors_use_date_semantics() -> Result<()> {
    let mut c = Database::memory()?.connect();
    for (text, expected) in [
        ("epoch", "1970-01-01"),
        ("-epoch", "1970-01-01"),
        ("  INFINITY  ", "infinity"),
        ("0000-01-01", "0001-01-01 (BC)"),
        ("-0001-01-01", "0002-01-01 (BC)"),
        ("2024/1/2", "2024-01-02"),
        ("2024 1 2", "2024-01-02"),
        ("2024\\1\\2", "2024-01-02"),
        ("0001-1-1 (bc)", "0001-01-01 (BC)"),
    ] {
        let result = c.query(&format!("SELECT DATE '{text}', '{text}'::DATE::VARCHAR"))?;
        assert_eq!(result.columns[0].data_type, DataType::Date);
        assert_eq!(
            result.rows,
            vec![vec![date(expected), Value::Varchar(expected.into())]]
        );
    }
    for text in [
        "",
        "+2024-01-01",
        "2023-02-29",
        "1900-02-29",
        "2024-13-01",
        "2024-00-01",
        "2024-01-00",
        "2024-01-32",
        "2024/01-02",
        "5881580-07-11",
        "5877642-06-24 (BC)",
        "0000-01-01 (BC)",
        "-0001-01-01 (BC)",
        "99999999999999999999999-01-01",
        "2024-01-012",
        "infinityx",
    ] {
        assert!(
            matches!(
                c.query(&format!("SELECT DATE '{text}'")),
                Err(Error::Conversion(_))
            ),
            "{text}"
        );
        assert_eq!(
            c.query(&format!("SELECT TRY_CAST('{text}' AS DATE)"))?.rows,
            vec![vec![Value::Null]]
        );
    }
    assert_eq!(
        c.query("SELECT CASE WHEN false THEN DATE 'invalid' ELSE DATE '2000-02-29' END")?
            .rows,
        vec![vec![date("2000-02-29")]]
    );
    for sql in [
        "SELECT 0::DATE",
        "SELECT DATE 'epoch'::INTEGER",
        "SELECT sum(DATE 'epoch')",
        "SELECT DATE 'epoch'=0",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    Ok(())
}

#[test]
fn date_queries_share_keys_comparisons_and_null_rules_across_join_adapters() -> Result<()> {
    let joins: Vec<Arc<dyn JoinAlgorithm>> = vec![Arc::new(HashJoin), Arc::new(NestedLoopJoin)];
    for join in joins {
        let mut c = DatabaseBuilder::new()
            .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![join])))
            .build()?
            .connect();
        c.execute("CREATE TABLE t(d DATE); INSERT INTO t VALUES (DATE '2000-02-29'),(DATE '1970-01-01'),(DATE 'epoch'),(DATE '-infinity'),(DATE 'infinity'),(NULL)")?;
        assert_eq!(
            c.query("SELECT d,count(*) FROM t GROUP BY d ORDER BY d")?
                .rows,
            vec![
                vec![date("-infinity"), Value::Integer(1)],
                vec![date("epoch"), Value::Integer(2)],
                vec![date("2000-02-29"), Value::Integer(1)],
                vec![date("infinity"), Value::Integer(1)],
                vec![Value::Null, Value::Integer(1)],
            ]
        );
        assert_eq!(
            c.query("SELECT min(d),max(d),count(DISTINCT d) FROM t")?
                .rows,
            vec![vec![date("-infinity"), date("infinity"), Value::Integer(4)]]
        );
        assert_eq!(
            c.query("SELECT count(*) FROM t a JOIN t b ON a.d=b.d")?
                .rows,
            vec![vec![Value::Integer(7)]]
        );
        assert_eq!(c.query("SELECT nullif(DATE 'epoch',DATE '1970-01-01'), DATE '2000-02-29' IN (DATE 'epoch',DATE '2000-02-29')")?.rows,
            vec![vec![Value::Null, Value::Boolean(true)]]);
        assert_eq!(
            c.query("SELECT DISTINCT d FROM t UNION SELECT DATE '2000-02-29' ORDER BY d")?
                .rows
                .len(),
            5
        );
    }
    Ok(())
}

#[test]
fn date_defaults_indexes_and_mutations_survive_both_snapshot_formats() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let formats: Vec<Arc<dyn SnapshotFormat>> = vec![
        Arc::new(JsonSnapshotFormat),
        Arc::new(DuckDbFormat::default()),
    ];
    let indexes: Vec<Arc<dyn IndexFactory>> =
        vec![Arc::new(HashIndexFactory), Arc::new(BTreeIndexFactory)];
    for format in formats {
        for index in &indexes {
            let path = directory
                .path()
                .join(format!("{}-{}.db", format.name(), index.name()));
            let open = || {
                DatabaseBuilder::new()
                    .indexes(index.clone())
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format.clone(),
                    )?))
                    .build()
            };
            {
                let mut c = open()?.connect();
                c.execute("CREATE TABLE t(d DATE PRIMARY KEY DEFAULT DATE '2000-02-29', n DATE DEFAULT DATE '-infinity'); INSERT INTO t DEFAULT VALUES; INSERT INTO t(d) VALUES ('1970-01-01'),('0001-01-01 (BC)'),('5877642-06-25 (BC)'),('5881580-07-10'),('infinity'),('-infinity')")?;
            }
            let mut c = open()?.connect();
            assert!(matches!(
                c.execute("INSERT INTO t(d) VALUES ('2000-02-29')"),
                Err(Error::Constraint(_))
            ));
            assert!(matches!(
                c.execute("INSERT INTO t(d) VALUES (NULL)"),
                Err(Error::Constraint(_))
            ));
            c.execute("BEGIN; DELETE FROM t WHERE d=DATE '2000-02-29'; ROLLBACK; UPDATE t SET d='2004-02-29' WHERE d=DATE '2000-02-29'; INSERT INTO t DEFAULT VALUES")?;
            assert_eq!(
                c.query("SELECT d,n FROM t WHERE d=DATE '2004-02-29'")?.rows,
                vec![vec![date("2004-02-29"), date("-infinity")]]
            );
            drop(c);
            assert_eq!(
                open()?
                    .connect()
                    .query("SELECT count(*), min(d),max(d) FROM t")?
                    .rows,
                vec![vec![Value::Integer(8), date("-infinity"), date("infinity")]]
            );
        }
    }
    Ok(())
}

#[test]
fn independent_date_checkpoints_decode_through_both_bitpacking_adapters() -> Result<()> {
    for scalar in [false, true] {
        let mut decoders = compression::decoders();
        if scalar {
            decoders.replace(Arc::new(ScalarBitPackingDecoder))?;
        }
        let format = DuckDbFormat::new(decoders);
        for (name, count) in [
            ("dates_scalar", 10),
            ("dates_bitpacking", 125013),
            ("dates_rle", 10000),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("test/data/duckdb/{name}.duckdb.gz"));
            let mut bytes = Vec::new();
            flate2::read::GzDecoder::new(std::fs::File::open(path)?).read_to_end(&mut bytes)?;
            let snapshot = format.decode(bytes, builtin_types())?;
            use duckdb_rust::{
                catalog::{Catalog, TableName},
                storage::TableStorage,
            };
            let table = TableName::new("main", "t");
            assert_eq!(snapshot.table(&table)?.columns[1].data_type, DataType::Date);
            let rows = snapshot.scan(&table, &QueryContext::background())?;
            assert_eq!(rows.len(), count);
            for (i, (_, row)) in rows.iter().enumerate() {
                let expected = match name {
                    "dates_scalar" => [
                        date("-infinity"),
                        date("infinity"),
                        date("5877642-06-25 (BC)"),
                        date("5881580-07-10"),
                        date("0001-01-01 (BC)"),
                        date("0001-01-01"),
                        date("1969-12-31"),
                        date("1970-01-01"),
                        date("2000-02-29"),
                        Value::Null,
                    ][i]
                        .clone(),
                    "dates_bitpacking" if i % 29 == 0 => Value::Null,
                    "dates_bitpacking" => Value::Date(Date::from_days(-62091 + i as i32)?),
                    "dates_rle" if i % 1000 < 100 => Value::Null,
                    _ => Value::Date(Date::from_days(-3653 + (i / 100) as i32)?),
                };
                assert_eq!(row[1], expected, "{name} row {i}, scalar {scalar}");
                let constant = match name {
                    "dates_bitpacking" if i % 31 == 0 => Value::Null,
                    "dates_rle" => date("infinity"),
                    _ => date("2000-02-29"),
                };
                assert_eq!(row[2], constant);
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct RejectDateCast;
impl CastFunction for RejectDateCast {
    fn name(&self) -> &'static str {
        "test-date-resource-failure"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        DateCast.supports(spec)
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        Err(Error::Resource("selected date cast failed".into()))
    }
}

#[test]
fn date_literals_defaults_and_try_cast_honor_selected_adapters() -> Result<()> {
    let mut casts = CastRegistry::builtins();
    for mode in [CastMode::Assignment, CastMode::Explicit] {
        casts.replace(
            CastSpec {
                source: DataType::Varchar,
                target: DataType::Date,
                mode,
            },
            Arc::new(RejectDateCast),
        )?;
    }
    let mut c = DatabaseBuilder::new().casts(casts).build()?.connect();
    for sql in [
        "SELECT DATE 'epoch'",
        "SELECT CAST('epoch' AS DATE)",
        "SELECT TRY_CAST('epoch' AS DATE)",
        "CREATE TABLE defaults(d DATE DEFAULT DATE 'epoch')",
    ] {
        assert!(matches!(c.execute(sql), Err(Error::Resource(_))), "{sql}");
    }
    c.execute("CREATE TABLE t(d DATE)")?;
    assert!(matches!(
        c.execute("INSERT INTO t VALUES ('epoch')"),
        Err(Error::Resource(_))
    ));
    // Identity and parameter paths retain their own selected structural cast.
    assert_eq!(
        c.execute_params("SELECT $1::DATE", &[date("epoch")])?[0].rows,
        vec![vec![date("epoch")]]
    );
    Ok(())
}

#[test]
fn native_validity_cannot_promote_a_reserved_date_slot_to_a_value() -> Result<()> {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!("../data/duckdb/dates_scalar.duckdb.gz")[..])
        .read_to_end(&mut bytes)?;
    let prefix: Vec<_> = [-i32::MAX, i32::MAX, -2147483646, 2147483646, -719528_i32]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect();
    let positions: Vec<_> = bytes
        .windows(prefix.len())
        .enumerate()
        .filter_map(|(i, value)| (value == prefix).then_some(i))
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "locate the independent uncompressed DATE column"
    );
    let offset = positions[0];
    bytes[offset..offset + 4].copy_from_slice(&i32::MIN.to_le_bytes());
    // Keep the fixture's 256 KiB block checksum valid so validation reaches the
    // semantic fault: the column validity bit still declares this row present.
    let block = 12288 + (offset - 12288) / 262144 * 262144;
    let checksum = bytes[block + 8..block + 262144]
        .chunks_exact(8)
        .fold(5381, |sum, word| {
            sum ^ u64::from_le_bytes(word.try_into().unwrap()).wrapping_mul(0xbf58476d1ce4e5b9)
        });
    bytes[block..block + 8].copy_from_slice(&checksum.to_le_bytes());
    assert!(
        matches!(DuckDbFormat::default().decode(bytes, builtin_types()),
        Err(Error::Corrupt(message)) if message == "valid row has no decoded value")
    );
    Ok(())
}
