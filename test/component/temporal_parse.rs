use super::*;
use duckdb_rust::{
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn timestamp(text: &str, kind: &DataType) -> Result<Value> {
    Ok(Value::Temporal(TemporalValue::parse(text, kind)?))
}

#[derive(Debug)]
struct SelectedParseFormatCast(bool);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedParseFormatCast {
    fn name(&self) -> &'static str {
        "selected-strptime-format"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.target == DataType::Varchar && spec.mode == CastMode::Implicit
    }
    fn cast(&self, _: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if self.0 {
            Err(Error::Resource("selected strptime format failure".into()))
        } else {
            Ok(Value::Varchar("%Y-%m-%d".into()))
        }
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn strptime_parses_core_directives_and_selects_precision_and_zone_types() -> Result<()> {
    for batched in [false, true] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .batch_size(2)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .optimizer(optimizer)
                .build()?
                .connect();
            let result = c.query(
                "SELECT
               strptime('1-2-3 11:04:05.123456 PM','%Y-%m-%d %I:%M:%S.%f %p'),
               strptime('2000-02-29 15:04:05.123456789','%Y-%m-%d %H:%M:%S.%n'),
               strptime('2000-02-29 15:04:05.123+02:30','%Y-%m-%d %H:%M:%S.%g%z'),
               strptime('2000-02-29 15:04:05.123456789-01:02:03','%Y-%m-%d %H:%M:%S.%n%z')",
            )?;
            assert_eq!(
                result
                    .columns
                    .iter()
                    .map(|column| column.data_type.clone())
                    .collect::<Vec<_>>(),
                vec![
                    DataType::Timestamp,
                    DataType::TimestampNs,
                    DataType::TimestampTz,
                    DataType::TimestampTzNs,
                ]
            );
            assert_eq!(
                result.rows,
                vec![vec![
                    timestamp("0001-02-03 23:04:05.123456", &DataType::Timestamp)?,
                    timestamp("2000-02-29 15:04:05.123456789", &DataType::TimestampNs)?,
                    timestamp("2000-02-29 12:34:05.123", &DataType::TimestampTz)?,
                    timestamp("2000-02-29 16:06:08.123456789", &DataType::TimestampTzNs)?,
                ]]
            );

            assert_eq!(
                c.query(
                    "SELECT
                   strptime('Tuesday 29 February 2000','%A %d %B %Y'),
                   strptime('2000-060','%Y-%j'),
                   strptime('2020-01-1','%G-%V-%u'),
                   strptime('2000-09-2','%Y-%U-%w'),
                   strptime('2000-09-1','%Y-%W-%w'),
                   strptime('PM','%p'),
                   strptime('UTC','%Z')",
                )?
                .rows,
                vec![vec![
                    timestamp("2000-02-29", &DataType::Timestamp)?,
                    timestamp("2000-02-29", &DataType::Timestamp)?,
                    timestamp("2019-12-30", &DataType::Timestamp)?,
                    timestamp("2000-02-29", &DataType::Timestamp)?,
                    timestamp("2000-02-28", &DataType::Timestamp)?,
                    timestamp("1900-01-01 12:00:00", &DataType::Timestamp)?,
                    timestamp("1900-01-01", &DataType::Timestamp)?,
                ]]
            );
            assert_eq!(
            c.query(
                "SELECT strptime(v,'%Y-%m-%d') FROM (VALUES ('2000-01-01'),(NULL),('2000-01-03')) t(v)"
            )?
            .rows,
            vec![
                vec![timestamp("2000-01-01", &DataType::Timestamp)?],
                vec![Value::Null],
                vec![timestamp("2000-01-03", &DataType::Timestamp)?],
            ]
            );
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn strptime_constant_lists_nulls_prepared_arguments_and_error_boundary_match_core() -> Result<()> {
    let enumeration = DataType::enumeration(vec!["template".into()])?;
    for failed in [false, true] {
        let mut casts = CastRegistry::builtins();
        casts.replace(
            CastSpec {
                source: enumeration.clone(),
                target: DataType::Varchar,
                mode: CastMode::Implicit,
            },
            Arc::new(SelectedParseFormatCast(failed)),
        )?;
        let mut selected = DatabaseBuilder::new().casts(casts).build()?.connect();
        let result =
            selected.query("SELECT try_strptime('2000-02-29','template'::ENUM('template'))");
        if failed {
            assert!(matches!(result, Err(Error::Resource(_))));
        } else {
            assert_eq!(
                result?.rows,
                vec![vec![timestamp("2000-02-29", &DataType::Timestamp)?]]
            );
        }
    }
    let mut c = Database::memory()?.connect();
    assert_eq!(
        c.query(
            "SELECT
               strptime(format := '%Y-%m-%d', text := '2001-02-03'),
               try_strptime(format := ['%Y/%m/%d','%Y-%m-%d'], text := '2001-02-03'),
               strptime('2001-02-03', format := '%Y-%m-%d')"
        )?
        .rows,
        vec![vec![
            timestamp("2001-02-03", &DataType::Timestamp)?,
            timestamp("2001-02-03", &DataType::Timestamp)?,
            timestamp("2001-02-03", &DataType::Timestamp)?,
        ]]
    );
    assert_eq!(
        c.query("SELECT strptime('2000/02/29',['%Y-%m-%d','%Y/%m/%d'])")?
            .rows,
        vec![vec![timestamp("2000-02-29", &DataType::Timestamp)?]]
    );
    assert_eq!(
        c.query("SELECT strptime('NULL',[NULL]::VARCHAR[])")?.rows,
        vec![vec![timestamp("1900-01-01", &DataType::Timestamp)?]]
    );
    assert_eq!(
        c.query("SELECT strptime('69',upper('%y'))")?.rows,
        vec![vec![timestamp("0069-01-01", &DataType::Timestamp)?]]
    );
    assert_eq!(
        c.query(
            "SELECT
               try_strptime('bad','%Y-%m-%d'),
               strptime('x',NULL),
               try_strptime('+1','%z'),
               try_strptime('epochx','epochx'),
               strptime('epoch','literal')",
        )?
        .rows,
        vec![vec![
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            timestamp("epoch", &DataType::Timestamp)?,
        ]]
    );
    for sql in [
        "SELECT strptime('x',[]::VARCHAR[])",
        "SELECT try_strptime(NULL,'%Q')",
        "SELECT try_strptime('2000-01-01',['%Y-%m-%d','%Q'])",
        "SELECT strptime('2000-01-01',f) FROM (VALUES ('%Y-%m-%d')) t(f)",
        "SELECT strptime('2000',(SELECT '%Y'))",
        "SELECT strptime(text := '2000', '%Y')",
        "SELECT strptime(text := '2000', text := '%Y')",
        "SELECT strptime(unknown := '2000', format := '%Y')",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    assert!(matches!(
        c.query("SELECT strptime('not-a-date','%Y-%m-%d')"),
        Err(Error::InvalidInput(message)) if message.contains("Could not parse string")
    ));
    assert!(matches!(
        c.query("SELECT strptime(CAST('bad' AS INTEGER)::VARCHAR,NULL)"),
        Err(Error::Conversion(_))
    ));
    let prepared = c.prepare("SELECT strptime($1,$2)")?;
    let result = c.execute_prepared(
        &prepared,
        &[
            Value::Varchar("69-7-20".into()),
            Value::Varchar("%y-%-m-%-d".into()),
        ],
    )?;
    assert_eq!(
        result.rows,
        vec![vec![timestamp("1969-07-20", &DataType::Timestamp)?]]
    );

    assert_eq!(
        c.query(
            "SELECT
               try_strptime('1677-09-21 12:00:00.000000000','%Y-%m-%d %H:%M:%S.%n'),
               strptime('2262-04-11 23:47:16.854775807','%Y-%m-%d %H:%M:%S.%n')"
        )?
        .rows,
        vec![vec![
            Value::Null,
            timestamp("infinity", &DataType::TimestampNs)?,
        ]]
    );
    assert!(matches!(
        c.query("SELECT strptime('1677-09-21 12:00:00.000000000','%Y-%m-%d %H:%M:%S.%n')"),
        Err(Error::Conversion(_))
    ));
    assert!(matches!(
        c.query("SELECT strptime('2001-02-30','%Y-%m-%d')"),
        Err(Error::Conversion(_))
    ));
    assert_eq!(
        c.query("SELECT try_strptime('2001-02-30','%Y-%m-%d')")?
            .rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        c.query(
            "SELECT
               try_strptime('2262-04-11 23:47:16.854775807','%Y-%m-%d %H:%M:%S.%n'),
               try_strptime('infinity','%Y-%m-%d'),
               try_strptime('-infinity','%n'),
               try_strptime('epoch','%Y-%m-%d')"
        )?
        .rows,
        vec![vec![
            Value::Null,
            timestamp("1900-01-01", &DataType::Timestamp)?,
            timestamp("1900-01-01", &DataType::TimestampNs)?,
            timestamp("1900-01-01", &DataType::Timestamp)?,
        ]]
    );
    assert_eq!(
        c.query("SELECT strptime('2020-13-01',['%Y-%m-%d','%Y-%d-%m'])")?
            .rows,
        vec![vec![timestamp("2020-01-13", &DataType::Timestamp)?]]
    );
    assert!(matches!(
        c.query("SELECT strptime('2001-02-30',['%Y-%m-%d','2001-02-30'])"),
        Err(Error::Conversion(_))
    ));
    assert_eq!(
        c.query("SELECT try_strptime('2001-02-30',['%Y-%m-%d','2001-02-30'])")?
            .rows,
        vec![vec![timestamp("1900-01-01", &DataType::Timestamp)?]]
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parsed_values_survive_defaults_mutation_and_private_and_native_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for native in [false, true] {
        let path = directory.path().join(if native {
            "parsed-native.duckdb"
        } else {
            "parsed-private.db"
        });
        let open = || -> Result<Database> {
            if native {
                let checkpoint = FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    Arc::new(DuckDbFormat::default()),
                )?
                .with_recovery(Arc::new(DuckDbWalRecovery))?;
                DatabaseBuilder::new()
                    .durability(Arc::new(FileWal::new(
                        checkpoint,
                        Arc::new(DuckDbTransactionLog),
                    )?))
                    .build()
            } else {
                Database::open(&path)
            }
        };
        let mut c = open()?.connect();
        c.execute(
            "CREATE TABLE parsed(
                id INTEGER PRIMARY KEY,
                raw VARCHAR,
                ts TIMESTAMP_NS DEFAULT strptime('2000-01-01.000000001','%Y-%m-%d.%n')
             );
             INSERT INTO parsed(id,raw) VALUES (1,'2000/02/29');
             INSERT INTO parsed VALUES (2,'2001-03-04',strptime('2001-03-04',['%Y/%m/%d','%Y-%m-%d']));
             UPDATE parsed SET ts=strptime(raw,['%Y/%m/%d','%Y-%m-%d']) WHERE id=1",
        )?;
        let expected = c.query("SELECT id,ts FROM parsed ORDER BY id")?.rows;
        assert_eq!(
            expected,
            vec![
                vec![
                    Value::Integer(1),
                    timestamp("2000-02-29", &DataType::TimestampNs)?
                ],
                vec![
                    Value::Integer(2),
                    timestamp("2001-03-04", &DataType::TimestampNs)?
                ],
            ]
        );
        c.execute("UPDATE parsed SET raw='bad' WHERE id=2")?;
        assert!(matches!(
            c.execute("UPDATE parsed SET ts=strptime(raw,['%Y/%m/%d','%Y-%m-%d'])"),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(
            c.query("SELECT id,ts FROM parsed ORDER BY id")?.rows,
            expected,
            "failed multi-row parsing update must be atomic"
        );
        c.checkpoint()?;
        drop(c);
        assert_eq!(
            open()?
                .connect()
                .query("SELECT id,ts FROM parsed ORDER BY id")?
                .rows,
            expected
        );
    }
    Ok(())
}
