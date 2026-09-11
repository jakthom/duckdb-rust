use super::*;
use duckdb_rust::{
    Date, Error,
    common::cast::{CastFunction, CastMode, CastRegistry, CastSpec},
    parallel::QueryContext,
};

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn strftime_retains_core_directives_precision_utc_and_calendar_boundaries() -> Result<()> {
    for batched in [false, true] {
        let mut c = DatabaseBuilder::new()
            .batch_size(2)
            .expressions(if batched {
                Arc::new(BatchedEvaluator)
            } else {
                Arc::new(ScalarEvaluator)
            })
            .build()?
            .connect();
        for (directive, expected) in [
            ("a", "Tue"),
            ("A", "Tuesday"),
            ("w", "2"),
            ("u", "2"),
            ("d", "29"),
            ("-d", "29"),
            ("b", "Feb"),
            ("h", "Feb"),
            ("B", "February"),
            ("m", "02"),
            ("-m", "2"),
            ("y", "00"),
            ("-y", "0"),
            ("Y", "2000"),
            ("G", "2000"),
            ("H", "15"),
            ("-H", "15"),
            ("I", "03"),
            ("-I", "3"),
            ("p", "PM"),
            ("M", "04"),
            ("-M", "4"),
            ("S", "05"),
            ("-S", "5"),
            ("n", "123456789"),
            ("f", "123456"),
            ("g", "123"),
            ("z", "+00"),
            ("Z", ""),
            ("j", "060"),
            ("-j", "60"),
            ("U", "09"),
            ("W", "09"),
            ("V", "09"),
            ("c", "2000-02-29 15:04:05"),
            ("x", "2000-02-29"),
            ("X", "15:04:05"),
            ("T", "15:04:05"),
            ("%", "%"),
        ] {
            let sql = format!(
                "SELECT strftime(v,'%{directive}'),strftime('%{directive}',v) FROM (VALUES (TIMESTAMP_NS '2000-02-29 15:04:05.123456789'),(NULL)) t(v)"
            );
            let result = c.query(&sql)?;
            assert_eq!(
                result
                    .columns
                    .iter()
                    .map(|c| c.data_type.clone())
                    .collect::<Vec<_>>(),
                vec![DataType::Varchar; 2]
            );
            assert_eq!(
                result.rows,
                vec![
                    vec![Value::Varchar(expected.into()); 2],
                    vec![Value::Null; 2]
                ],
                "{directive}"
            );
        }
        for (value, format, expected) in [
            ("DATE '0001-01-01 (BC)'", "%Y|%y|%G", "0000|00|7295"),
            ("DATE '0002-01-01 (BC)'", "%Y", "-1"),
            ("DATE '5881580-07-10'", "%Y-%m-%d", "5881580-07-10"),
            (
                "TIMESTAMP_NS '1969-12-31 23:59:59.999999999'",
                "%c.%n",
                "1969-12-31 23:59:59.999999999",
            ),
            (
                "TIMESTAMPTZ_NS '2000-01-02 03:04:05.123456789+02'",
                "%c.%n",
                "2000-01-02 01:04:05.123456789",
            ),
            (
                "TIMESTAMPTZ '2000-01-02 03:04:05+02'",
                "%c|%z|%Z",
                "2000-01-02 01:04:05|+00|",
            ),
            ("DATE 'infinity'", "literal", "infinity"),
            ("DATE '-infinity'", "%Y", "-infinity"),
            ("DATE 'epoch'", "雪 %% %Y 🦆 %%%%%%", "雪 % 1970 🦆 %%%"),
        ] {
            assert_eq!(
                c.query(&format!("SELECT strftime({value},'{format}')"))?
                    .rows,
                vec![vec![Value::Varchar(expected.into())]],
                "{value}"
            );
        }
        assert!(
            matches!(c.query("SELECT strftime(make_timestamp(-9223372036854775808),'literal')"),Err(Error::Conversion(message)) if message=="Date out of range in timestamp conversion")
        );
    }
    Ok(())
}

#[derive(Debug)]
struct TemplateCast(bool);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for TemplateCast {
    fn name(&self) -> &'static str {
        "selected-format-template"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.mode == CastMode::Implicit && spec.target == DataType::Varchar
    }
    fn cast(&self, _: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if self.0 {
            Err(Error::Resource("selected format cast failure".into()))
        } else {
            Ok(Value::Varchar("%Y-%m".into()))
        }
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn strftime_selected_formats_keep_required_constants_null_templates_and_prepared_cast_failures()
-> Result<()> {
    for failed in [false, true] {
        let enumeration = DataType::enumeration(vec!["template".into()])?;
        let mut casts = CastRegistry::builtins();
        casts.replace(
            CastSpec {
                source: enumeration.clone(),
                target: DataType::Varchar,
                mode: CastMode::Implicit,
            },
            Arc::new(TemplateCast(failed)),
        )?;
        let mut c = DatabaseBuilder::new().casts(casts).build()?.connect();
        let prepared = c.prepare("SELECT strftime($1,$2)")?;
        for result in [
            c.query(
                "SELECT {'text':strftime(DATE '2000-02-29','template'::ENUM('template'))}.text",
            ),
            c.execute_prepared(
                &prepared,
                &[
                    Value::Date(Date::from_ymd(2000, 2, 29)?),
                    Value::enumeration(&enumeration, 0)?,
                ],
            ),
        ] {
            if failed {
                assert!(matches!(result, Err(Error::Resource(_))));
            } else {
                assert_eq!(result?.rows, vec![vec![Value::Varchar("2000-02".into())]]);
            }
        }
    }
    let mut c = Database::memory()?.connect();
    for sql in [
        "SELECT strftime(NULL::DATE,'')",
        "SELECT strftime(NULL::DATE,'%Q')",
        "SELECT strftime(NULL::DATE,f) FROM (VALUES ('%Y')) t(f)",
        "SELECT strftime(CAST('bad' AS DATE),NULL)",
        "SELECT strftime(NULL::DATE,CAST('bad' AS INTEGER)::VARCHAR)",
    ] {
        assert_eq!(c.query(sql)?.rows, vec![vec![Value::Null]], "{sql}");
    }
    for (template, body) in [
        ("", "Empty format string"),
        ("%", "Trailing format character %"),
        ("%-", "Unrecognized format for strftime/strptime: %-"),
        ("%-Y", "Unrecognized format for strftime/strptime: %-Y"),
        ("%Q", "Unrecognized format for strftime/strptime: %Q"),
    ] {
        assert!(
            matches!(c.query(&format!("SELECT strftime(DATE 'epoch','{template}')")),Err(Error::InvalidInput(message)) if message==format!("Failed to parse format specifier {template}: {body}"))
        );
    }
    assert!(
        matches!(c.query("SELECT strftime(DATE 'epoch',f) FROM (VALUES ('%Y')) t(f)"),Err(Error::Bind(message)) if message=="The \"format\" argument in function \"strftime\" must be a constant expression")
    );
    assert_eq!(
        c.query("SELECT strftime(NULL,'%Y')")?.rows,
        vec![vec![Value::Null]]
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn formatted_values_cross_nested_keys_joins_windows_mutation_wal_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        for hashed in [false, true] {
            let path = directory
                .path()
                .join(format!("formatted-{batched}-{hashed}.duckdb"));
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
            c.execute("CREATE TABLE formatted(id INTEGER PRIMARY KEY,k VARCHAR UNIQUE,t TIMESTAMP_NS,p STRUCT(t TIMESTAMP_NS,s VARCHAR[]))")?;
            let insert=c.prepare("INSERT INTO formatted VALUES($1,strftime($2,'%c.%n'),$2,{'t':$2,'s':[strftime($2,'%x'),NULL::VARCHAR]})")?;
            for (id, text) in [
                (1, "2000-01-01 01:02:03.123456789"),
                (2, "2000-01-02 03:04:05.000000001"),
            ] {
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(id),
                        Value::Temporal(TemporalValue::parse(text, &DataType::TimestampNs)?),
                    ],
                )?;
            }
            let select = "SELECT id,k,strftime(p.t,'%c.%n'),p.s FROM formatted ORDER BY id";
            let before = c.query(select)?.rows;
            let lookup = c.prepare(
                "SELECT id FROM formatted WHERE k=strftime(CAST($1 AS TIMESTAMP_NS),$2)",
            )?;
            assert_eq!(
                c.execute_prepared(
                    &lookup,
                    &[
                        Value::Varchar("2000-01-02 03:04:05.000000001".into()),
                        Value::Varchar("%c.%n".into())
                    ]
                )?
                .rows,
                vec![vec![Value::Integer(2)]]
            );
            assert_eq!(c.query("SELECT count(*) FROM formatted a JOIN formatted b ON strftime(a.t,'%Y')=strftime(b.t,'%Y')")?.rows,vec![vec![Value::Integer(4)]]);
            assert_eq!(
                c.query(
                    "SELECT strftime(t,'%Y'),count(*) FROM formatted GROUP BY strftime(t,'%Y')"
                )?
                .rows,
                vec![vec![Value::Varchar("2000".into()), Value::Integer(2)]]
            );
            assert_eq!(c.query("SELECT row_number() OVER(PARTITION BY strftime(t,'%Y') ORDER BY strftime(t,'%c.%n')) FROM formatted ORDER BY id")?.rows,vec![vec![Value::Integer(1)],vec![Value::Integer(2)]]);
            assert!(matches!(
                c.execute("UPDATE formatted SET k=strftime(t,'%Q')"),
                Err(Error::InvalidInput(_))
            ));
            assert_eq!(c.query(select)?.rows, before);
            c.execute("BEGIN; UPDATE formatted SET k=strftime(t,'%Y%m%d'); DELETE FROM formatted WHERE id=2; ROLLBACK")?;
            assert_eq!(c.query(select)?.rows, before);
            drop(c);
            let mut c = open()?.connect();
            assert_eq!(c.query(select)?.rows, before);
            c.execute("UPDATE formatted SET k=strftime(t,'%Y%m%d') WHERE id=1")?;
            let committed = c.query(select)?.rows;
            c.checkpoint()?;
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?
                    .connect()
                    .query(select)?
                    .rows,
                committed
            );
        }
    }
    Ok(())
}
