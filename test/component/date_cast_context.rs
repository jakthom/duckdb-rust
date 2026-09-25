use super::*;
use duckdb_rust::{
    execution::expression_executor::{BatchedEvaluator, ScalarEvaluator},
    optimizer::IdentityOptimizer,
    storage::{
        duckdb::wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
        logged::FileWal,
    },
};

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn date_cast_source_policy_keeps_complete_calendar_api_and_validates_sql_suffixes() -> Result<()> {
    let cases = [
        ("1-1-1", Some("0001-01-01"), None),
        ("01-1-1", Some("0001-01-01"), Some("0001-01-01")),
        ("-1-1-1", Some("0002-01-01 (BC)"), None),
        ("-01-1-1", Some("0002-01-01 (BC)"), Some("0002-01-01 (BC)")),
        ("1-1-1 (BC)", Some("0001-01-01 (BC)"), None),
        (
            "01-1-1 (BC)",
            Some("0001-01-01 (BC)"),
            Some("0001-01-01 (BC)"),
        ),
        ("\t1-1-1", Some("0001-01-01"), None),
        (
            "000000000000001-1-1",
            Some("0001-01-01"),
            Some("0001-01-01"),
        ),
        ("2000-01-01 12:34:56", Some("2000-01-01"), None),
        ("2000-01-01T1:2", Some("2000-01-01"), None),
        ("2000-01-01 24:00:00", Some("2000-01-01"), None),
        ("2000-01-01 24:00:00.000001", None, None),
        (
            "2000-01-01 12:34:56 Europe/Amsterdam",
            Some("2000-01-01"),
            None,
        ),
        (
            "5881580-07-10 24:00:00.000000999",
            Some("5881580-07-10"),
            None,
        ),
        (
            "5877642-06-25 (BC) 24:00:00",
            Some("5877642-06-25 (BC)"),
            None,
        ),
        (
            "290309-12-21 (BC) 23:59:59.999999",
            Some("290309-12-21 (BC)"),
            None,
        ),
        ("5881580-07-10 25:00:00", None, None),
        ("5881580-07-11 12:00:00", None, None),
        ("2000-01-01junk", None, None),
        ("2000-01-011", None, None),
        ("2000-01-01\t", Some("2000-01-01"), Some("2000-01-01")),
        ("\tinfinity\t", Some("infinity"), Some("infinity")),
        ("inf ", None, None),
        ("1000000000-01-01", None, None),
        ("2147483648-01-01", None, None),
    ];
    for batched in [false, true] {
        let mut c = DatabaseBuilder::new()
            .optimizer(Arc::new(IdentityOptimizer))
            .batch_size(2)
            .expressions(if batched {
                Arc::new(BatchedEvaluator)
            } else {
                Arc::new(ScalarEvaluator)
            })
            .build()?
            .connect();
        c.execute("CREATE TABLE date_text(id INTEGER,s VARCHAR)")?;
        let insert = c.prepare("INSERT INTO date_text VALUES ($1,$2)")?;
        let expected: Vec<Vec<Value>> = cases
            .iter()
            .enumerate()
            .map(|(i, (text, ordinary, variant))| {
                c.execute_prepared(
                    &insert,
                    &[Value::Integer(i as i128), Value::Varchar((*text).into())],
                )?;
                Ok(vec![
                    ordinary.map_or(Value::Null, date),
                    variant.map_or(Value::Null, date),
                ])
            })
            .collect::<Result<_>>()?;
        assert_eq!(
            c.query(
                "SELECT TRY_CAST(s AS DATE),TRY_CAST(s::VARIANT AS DATE) FROM date_text ORDER BY id"
            )?
            .rows,
            expected
        );
        assert_eq!(c.query("SELECT TRY_CAST(['2000-01-01 12:34:56','5881580-07-10 24:00:00'] AS DATE[])::VARCHAR,TRY_CAST(['01-1-1','1-1-1']::VARIANT AS DATE[]),TRY_CAST({'d':'1-1-1'}::VARIANT AS STRUCT(d DATE)),CAST({'d':'01-1-1'}::VARIANT AS STRUCT(d DATE))::VARCHAR")?.rows,vec![vec![Value::Varchar("[2000-01-01, 5881580-07-10]".into()),Value::Null,Value::Null,Value::Varchar("{'d': 0001-01-01}".into())]]);
        for input in ["1-1-1junk", "2000-01-01 12:34:56+02junk", "inf "] {
            let error = c
                .query(&format!("SELECT CAST('{input}' AS DATE)"))
                .unwrap_err();
            assert!(
                matches!(error,Error::Conversion(message) if message == format!("invalid date field format: \"{input}\", expected format is (YYYY-MM-DD)"))
            );
        }
        for input in [
            "2000-02-30",
            "1000000000-01-01",
            "2147483648-01-01",
            "5881580-07-10 25:00:00",
        ] {
            let error = c
                .query(&format!("SELECT CAST('{input}' AS DATE)"))
                .unwrap_err();
            assert!(
                matches!(error,Error::Conversion(message) if message == format!("date field value out of range: \"{input}\""))
            );
        }
        for input in ["1-1-1", "2000-01-01 12:34:56", "2000-02-30"] {
            let error = c
                .query(&format!("SELECT CAST('{input}'::VARIANT AS DATE)"))
                .unwrap_err();
            assert!(
                matches!(error,Error::Conversion(message) if message == format!("Can't convert VARIANT(VARCHAR) value '{input}' to 'DATE'"))
            );
        }
        for text in ["2000-01-01 12:34:56", "5881580-07-10 24:00:00"] {
            assert!(
                text.parse::<Date>().is_err(),
                "calendar-only API must remain fully consuming"
            );
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn date_source_context_crosses_prepared_nested_indexes_wal_rollback_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        for hashed in [false, true] {
            let path = directory
                .path()
                .join(format!("date-source-context-{batched}-{hashed}.duckdb"));
            let open = || {
                let checkpoint = FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    Arc::new(DuckDbFormat::default()),
                )?
                .with_recovery(Arc::new(DuckDbWalRecovery))?;
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
                    .durability(Arc::new(FileWal::new(
                        checkpoint,
                        Arc::new(DuckDbTransactionLog),
                    )?))
                    .build()
            };
            let mut c = open()?.connect();
            c.execute("CREATE TABLE date_calls(k DATE UNIQUE,d DATE DEFAULT date('2000-01-01 24:00:00'),p STRUCT(d DATE))")?;
            let call_insert =
                c.prepare("INSERT INTO date_calls(k,p) VALUES (DATE($1),{'d':DATE($1)})")?;
            c.execute_prepared(
                &call_insert,
                &[Value::Varchar("2001-01-01 24:00:00".into())],
            )?;
            let call_rows = vec![vec![
                date("2001-01-01"),
                date("2000-01-01"),
                date("2001-01-01"),
            ]];
            assert_eq!(c.query("SELECT k,d,p.d FROM date_calls")?.rows, call_rows);
            c.execute("BEGIN; UPDATE date_calls SET k=DATE('2002-01-01'); ROLLBACK")?;
            assert_eq!(c.query("SELECT k,d,p.d FROM date_calls")?.rows, call_rows);
            assert!(matches!(
                c.execute_prepared(
                    &call_insert,
                    &[Value::Varchar("2001-01-01 25:00:00".into())]
                ),
                Err(Error::Conversion(_))
            ));
            assert_eq!(c.query("SELECT k,d,p.d FROM date_calls")?.rows, call_rows);
            c.execute("CREATE TABLE calendar(k DATE UNIQUE,d DATE DEFAULT DATE '2000-01-01 12:34:56',p STRUCT(d DATE))")?;
            let insert = c.prepare("INSERT INTO calendar(k,p) VALUES (CAST($1 AS DATE),CAST({'d':$2}::VARIANT AS STRUCT(d DATE)))")?;
            for (input, canonical) in [
                ("5881580-07-10 24:00:00", "5881580-07-10"),
                ("5877642-06-25 (BC) 24:00:00", "5877642-06-25 (BC)"),
                ("2000-01-01 12:34:56", "2000-01-01"),
            ] {
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Varchar(input.into()),
                        Value::Varchar(canonical.into()),
                    ],
                )?;
            }
            let projection = "SELECT k,d,p.d FROM calendar ORDER BY k";
            let before = c.query(projection)?.rows;
            for row in &before {
                assert_eq!(row[0], row[2]);
                assert_eq!(row[1], date("2000-01-01"));
            }
            assert_eq!(
                c.query("SELECT count(*) FROM calendar a JOIN calendar b ON a.k=b.k")?
                    .rows,
                vec![vec![Value::Integer(3)]]
            );
            assert_eq!(
                c.query("SELECT p.d,count(*) FROM calendar GROUP BY p.d ORDER BY p.d")?
                    .rows,
                before
                    .iter()
                    .map(|row| vec![row[0].clone(), Value::Integer(1)])
                    .collect::<Vec<_>>()
            );
            assert_eq!(c.query("SELECT min(k) OVER (ORDER BY k ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM calendar ORDER BY k")?.rows,vec![vec![date("5877642-06-25 (BC)")];3]);
            assert!(matches!(
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Varchar("5881580-07-10 12:34:56".into()),
                        Value::Varchar("5881580-07-10".into())
                    ]
                ),
                Err(Error::Constraint(_))
            ));
            for (input, payload) in [
                ("2000-01-02 25:00:00", "2000-01-02"),
                ("2000-01-02 12:34:56", "1-1-1"),
            ] {
                assert!(matches!(
                    c.execute_prepared(
                        &insert,
                        &[Value::Varchar(input.into()), Value::Varchar(payload.into())]
                    ),
                    Err(Error::Conversion(_))
                ));
                assert_eq!(c.query(projection)?.rows, before);
            }
            c.execute("BEGIN; UPDATE calendar SET k=DATE '2000-01-02 24:00:00' WHERE k=DATE '2000-01-01'; DELETE FROM calendar WHERE k=DATE '5881580-07-10'; ROLLBACK")?;
            assert_eq!(c.query(projection)?.rows, before);
            assert!(
                c.execute("UPDATE calendar SET k=DATE '2000-01-01 25:00:00'")
                    .is_err()
            );
            assert_eq!(c.query(projection)?.rows, before);
            c.execute("UPDATE calendar SET k='2000-01-02 24:00:00',p=CAST({'d':'2000-01-02'}::VARIANT AS STRUCT(d DATE)) WHERE k=DATE '2000-01-01'")?;
            let committed = c.query(projection)?.rows;
            drop(c);
            let mut c = open()?.connect();
            assert_eq!(c.query(projection)?.rows, committed);
            let call_lookup = c.prepare("SELECT k,d,p.d FROM date_calls WHERE k=DATE($1)")?;
            assert_eq!(
                c.execute_prepared(
                    &call_lookup,
                    &[Value::Varchar("2001-01-01 24:00:00".into())]
                )?
                .rows,
                call_rows
            );
            assert_eq!(
                c.query("SELECT p.d FROM calendar WHERE k=DATE '2000-01-02 24:00:00'")?
                    .rows,
                vec![vec![date("2000-01-02")]]
            );
            c.checkpoint()?;
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?
                    .connect()
                    .query("SELECT k,d,p.d FROM date_calls")?
                    .rows,
                call_rows
            );
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
