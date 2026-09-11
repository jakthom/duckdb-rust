use super::*;
use duckdb_rust::{
    Date, Error,
    common::cast::{CastFunction, CastMode, CastRegistry, CastSpec},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::QueryContext,
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
fn truncation_retains_aliases_calendar_units_duration_signs_and_timestamp_precision() -> Result<()>
{
    for batched in [false, true] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .optimizer(optimizer)
                .batch_size(2)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .build()?
                .connect();
            for function in ["date_trunc", "datetrunc"] {
                for (unit, expected) in [
                    ("millennium", "2000-01-01"),
                    ("century", "2000-01-01"),
                    ("decade", "2000-01-01"),
                    ("year", "2001-01-01"),
                    ("quarter", "2001-07-01"),
                    ("month", "2001-08-01"),
                    ("week", "2001-08-20"),
                    ("isoyear", "2001-01-01"),
                    ("day", "2001-08-22"),
                    ("dow", "2001-08-22"),
                    ("isodow", "2001-08-22"),
                    ("julian", "2001-08-22"),
                    ("yearweek", "2001-08-20"),
                    ("usecs", "2001-08-22"),
                ] {
                    let result =
                        c.query(&format!("SELECT {function}('{unit}',DATE '2001-08-22')"))?;
                    assert_eq!(result.columns[0].data_type, DataType::Timestamp);
                    assert_eq!(
                        text_rows(result),
                        vec![vec![format!("{expected} 00:00:00")]],
                        "{unit}"
                    );
                }
                let result=c.query(&format!("SELECT {function}('second',TIMESTAMP '1969-12-31 23:59:59.999999'),{function}('microsecond',TIMESTAMP_NS '1969-12-31 23:59:59.999999999'),{function}('microsecond',TIMESTAMP_NS '2001-02-03 12:34:56.123456789'),{function}('hour',TIMESTAMP_S '2001-02-03 12:34:56')"))?;
                assert!(
                    result
                        .columns
                        .iter()
                        .all(|column| column.data_type == DataType::Timestamp)
                );
                assert_eq!(
                    text_rows(result),
                    vec![vec![
                        "1969-12-31 23:59:59",
                        "1970-01-01 00:00:00",
                        "2001-02-03 12:34:56.123457",
                        "2001-02-03 12:00:00"
                    ]]
                );
                for (unit, expected) in [
                    ("year", "-1 year"),
                    ("month", "-1 year -1 month"),
                    ("week", "-1 year -1 month -7 days"),
                    ("day", "-1 year -1 month -8 days"),
                    ("hour", "-1 year -1 month -8 days -01:00:00"),
                    ("millisecond", "-1 year -1 month -8 days -01:02:03.456"),
                ] {
                    let result = c.query(&format!(
                        "SELECT {function}('{unit}',INTERVAL '-13 months -8 days -01:02:03.456789')"
                    ))?;
                    assert_eq!(result.columns[0].data_type, DataType::Interval);
                    assert_eq!(text_rows(result), vec![vec![expected]], "{unit}");
                }
                assert_eq!(text_rows(c.query(&format!("SELECT {function}('decade',DATE '0002-01-01 (BC)'),{function}('isoyear',DATE '2018-12-31'),{function}('day',DATE 'infinity'),{function}('day',DATE '-infinity')"))?),vec![vec!["0001-01-01 (BC) 00:00:00","2018-12-31 00:00:00","infinity","-infinity"]]);
                assert!(
                    matches!(c.query(&format!("SELECT CASE WHEN false THEN {function}('bad',DATE 'epoch') ELSE NULL END")),Err(Error::Conversion(message)) if message=="extract specifier \"bad\" not recognized")
                );
                assert_eq!(c.query(&format!("SELECT CASE WHEN false THEN {function}('bad',INTERVAL '1 day') ELSE NULL END"))?.rows,vec![vec![Value::Null]]);
                assert!(
                    matches!(c.query(&format!("SELECT {function}('era',DATE 'infinity')")),Err(Error::Unsupported(message)) if message=="Specifier type not implemented for DATETRUNC statistics")
                );
                assert_eq!(
                    text_rows(c.query(&format!(
                        "SELECT {function}(p,DATE 'infinity') FROM (VALUES ('era')) t(p)"
                    ))?),
                    vec![vec!["infinity"]]
                );
                assert!(matches!(
                    c.query(&format!(
                        "SELECT {function}(p,DATE 'infinity') FROM (VALUES ('bad')) t(p)"
                    )),
                    Err(Error::Conversion(_))
                ));
                assert!(
                    matches!(c.query(&format!("SELECT {function}((SELECT 'era'),DATE 'infinity')")),Err(Error::Unsupported(message)) if message=="Specifier type not implemented for DATETRUNC")
                );
                assert_eq!(
                    c.query(&format!(
                        "SELECT {function}(p,CAST('bad' AS DATE)) FROM (SELECT NULL::VARCHAR p)"
                    ))?
                    .rows,
                    vec![vec![Value::Null]]
                );
                for argument in [
                    "NULL",
                    "TIME '12:00:00'",
                    "TIME_NS '12:00:00'",
                    "true",
                    "'2001-02-03'",
                ] {
                    assert!(
                        matches!(
                            c.query(&format!("SELECT {function}('day',{argument})")),
                            Err(Error::Bind(_))
                        ),
                        "{argument}"
                    );
                }
                assert_eq!(c.query(&format!("SELECT epoch_us({function}('day',make_timestamp(-9223372036854775806))),epoch_us({function}('second',make_timestamp(-9223372036854775806))),epoch_us({function}('microsecond',make_timestamp(-9223372036854775806)))"))?.rows,vec![vec![Value::Integer(9223371964909551616),Value::Integer(9223372036854551616),Value::Integer(-9223372036854775806)]]);
            }
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn buckets_keep_origins_offsets_core_time_wrapping_and_width_demand_order() -> Result<()> {
    for batched in [false, true] {
        let mut c = DatabaseBuilder::new()
            .batch_size(2)
            .optimizer(Arc::new(IdentityOptimizer))
            .expressions(if batched {
                Arc::new(BatchedEvaluator)
            } else {
                Arc::new(ScalarEvaluator)
            })
            .build()?
            .connect();
        for (sql, kind, expected) in [
            (
                "time_bucket(INTERVAL '1 week',DATE '2001-08-22')",
                DataType::Date,
                "2001-08-20",
            ),
            (
                "time_bucket(INTERVAL '3 months',DATE '2001-08-22')",
                DataType::Date,
                "2001-07-01",
            ),
            (
                "time_bucket(INTERVAL '1 day',TIMESTAMP '1969-12-31 23:59:59.999999')",
                DataType::Timestamp,
                "1969-12-31 00:00:00",
            ),
            (
                "time_bucket(INTERVAL '1 day',DATE '2001-08-22',INTERVAL '12 hours')",
                DataType::Date,
                "2001-08-21",
            ),
            (
                "time_bucket(INTERVAL '1 day',DATE '2001-08-22',TIMESTAMP '2000-01-03 12:00:00')",
                DataType::Timestamp,
                "2001-08-21 12:00:00",
            ),
            (
                "time_bucket(INTERVAL '1 microsecond',TIMESTAMP_NS '2001-02-03 12:34:56.123456789')",
                DataType::Timestamp,
                "2001-02-03 12:34:56.123457",
            ),
            (
                "time_bucket(INTERVAL '5 minutes',TIME '12:34:56.789123')",
                DataType::Time,
                "12:30:00",
            ),
            (
                "time_bucket(INTERVAL '1 month',TIME '12:34:56')",
                DataType::Time,
                "00:00:00",
            ),
            (
                "time_bucket(INTERVAL '1 hour',TIME '00:00:00',TIME '00:30:00')",
                DataType::Time,
                "23:30:00",
            ),
            (
                "time_bucket(INTERVAL '1 hour',TIME '24:00:00')",
                DataType::Time,
                "00:00:00",
            ),
            (
                "time_bucket(INTERVAL '1 month',DATE 'infinity')",
                DataType::Date,
                "infinity",
            ),
            (
                "time_bucket(INTERVAL '0 days',DATE 'epoch',DATE 'infinity')",
                DataType::Date,
                "NULL",
            ),
            (
                "time_bucket(INTERVAL '1 day',NULL::DATE)",
                DataType::Date,
                "NULL",
            ),
        ] {
            let result = c.query(&format!("SELECT {sql}"))?;
            assert_eq!(result.columns[0].data_type, kind, "{sql}");
            assert_eq!(text_rows(result), vec![vec![expected]], "{sql}");
        }
        for width in ["0 days", "-1 month", "1 month 1 day"] {
            assert!(matches!(
                c.query(&format!(
                    "SELECT time_bucket(INTERVAL '{width}',DATE 'infinity')"
                )),
                Err(Error::Unsupported(_))
            ));
            assert_eq!(
                c.query(&format!(
                    "SELECT time_bucket(INTERVAL '{width}',d) FROM (VALUES (NULL::DATE)) t(d)"
                ))?
                .rows,
                vec![vec![Value::Null]]
            );
        }
        assert!(
            matches!(c.query("SELECT time_bucket(INTERVAL '106751992 days',d) FROM (VALUES (NULL::DATE)) t(d)"),Err(Error::Conversion(message)) if message=="Could not convert Day to Microseconds")
        );
        assert_eq!(c.query("SELECT time_bucket(w,d) FROM (VALUES (INTERVAL '106751992 days',NULL::DATE)) t(w,d)")?.rows,vec![vec![Value::Null]]);
        assert_eq!(
            c.query("SELECT time_bucket(INTERVAL '106751992 days',d) FROM (SELECT NULL::DATE d)")?
                .rows,
            vec![vec![Value::Null]]
        );
        assert_eq!(
            c.query("SELECT time_bucket(w,CAST('bad' AS DATE)) FROM (SELECT NULL::INTERVAL w)")?
                .rows,
            vec![vec![Value::Null]]
        );
        let prepared = c.prepare(
            "SELECT time_bucket($1,CAST($2 AS TIMESTAMP),$3),date_trunc($4,CAST($2 AS TIMESTAMP))",
        )?;
        let parameters = [
            Value::Temporal(TemporalValue::Interval {
                months: 0,
                days: 0,
                micros: 3_600_000_000,
            }),
            Value::Varchar("2001-02-03 12:34:56".into()),
            Value::Temporal(TemporalValue::Interval {
                months: 0,
                days: 0,
                micros: 1_800_000_000,
            }),
            Value::Varchar("day".into()),
        ];
        assert_eq!(
            text_rows(c.execute_prepared(&prepared, &parameters)?),
            vec![vec!["2001-02-03 12:30:00", "2001-02-03 00:00:00"]]
        );
        let mut invalid = parameters.clone();
        invalid[0] = Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: 0,
            micros: 0,
        });
        assert!(matches!(
            c.execute_prepared(&prepared, &invalid),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(
            text_rows(c.execute_prepared(&prepared, &parameters)?),
            vec![vec!["2001-02-03 12:30:00", "2001-02-03 00:00:00"]]
        );
    }
    Ok(())
}

#[derive(Debug)]
struct CalendarSpecifierCast(bool);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for CalendarSpecifierCast {
    fn name(&self) -> &'static str {
        "selected-calendar-specifier"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.mode == CastMode::Implicit && spec.target == DataType::Varchar
    }
    fn cast(&self, _value: &Value, _spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if self.0 {
            Err(Error::Resource("selected truncation cast failed".into()))
        } else {
            Ok(Value::Varchar("month".into()))
        }
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn truncation_uses_selected_specifier_cast_for_binding_rows_nested_results_and_parameters()
-> Result<()> {
    let enumeration = DataType::enumeration(vec!["day".into()])?;
    for failed in [false, true] {
        for batched in [false, true] {
            let mut casts = CastRegistry::builtins();
            casts.replace(
                CastSpec {
                    source: enumeration.clone(),
                    target: DataType::Varchar,
                    mode: CastMode::Implicit,
                },
                Arc::new(CalendarSpecifierCast(failed)),
            )?;
            let mut c = DatabaseBuilder::new()
                .casts(casts)
                .batch_size(2)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .build()?
                .connect();
            for sql in [
                "SELECT date_trunc('day'::ENUM('day'),DATE '2001-02-03')",
                "SELECT date_trunc(p,d) FROM (VALUES ('day'::ENUM('day'),DATE '2001-02-03')) t(p,d)",
                "SELECT {'t':date_trunc('day'::ENUM('day'),DATE '2001-02-03')}.t",
            ] {
                let result = c.query(sql);
                if failed {
                    assert!(matches!(result, Err(Error::Resource(_))));
                } else {
                    assert_eq!(text_rows(result?), vec![vec!["2001-02-01 00:00:00"]]);
                }
            }
            let prepared = c.prepare("SELECT date_trunc($1,$2)")?;
            let result = c.execute_prepared(
                &prepared,
                &[
                    Value::enumeration(&enumeration, 0)?,
                    Value::Date(Date::from_ymd(2001, 2, 3)?),
                ],
            );
            if failed {
                assert!(matches!(result, Err(Error::Resource(_))));
            } else {
                assert_eq!(text_rows(result?), vec![vec!["2001-02-01 00:00:00"]]);
            }
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_results_cross_nested_values_indexes_joins_windows_mutations_and_native_recovery()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        for hashed in [false, true] {
            let path = directory
                .path()
                .join(format!("calendar-{batched}-{hashed}.duckdb"));
            let open = || {
                let checkpoint = FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    Arc::new(DuckDbFormat::default()),
                )?
                .with_recovery(Arc::new(DuckDbWalRecovery))?;
                DatabaseBuilder::new()
                    .batch_size(2)
                    .optimizer(Arc::new(IdentityOptimizer))
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
                    .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(if hashed {
                        vec![Arc::new(HashJoin), Arc::new(NestedLoopJoin)]
                    } else {
                        vec![Arc::new(NestedLoopJoin)]
                    })))
                    .durability(Arc::new(FileWal::new(
                        checkpoint,
                        Arc::new(DuckDbTransactionLog),
                    )?))
                    .build()
            };
            let mut c = open()?.connect();
            c.execute("CREATE TABLE calendar_results(id INTEGER UNIQUE,k TIMESTAMP UNIQUE,b DATE,c TIME,p STRUCT(d DATE,v TIMESTAMP[],span INTERVAL))")?;
            let insert=c.prepare("INSERT INTO calendar_results VALUES($1,date_trunc('day',CAST($2 AS TIMESTAMP_NS)),time_bucket(INTERVAL '1 month',CAST($2 AS DATE)),time_bucket(INTERVAL '1 hour',CAST($3 AS TIME)),{'d':time_bucket(INTERVAL '1 week',CAST($2 AS DATE)),'v':[date_trunc('hour',CAST($2 AS TIMESTAMP_NS)),NULL::TIMESTAMP],'span':date_trunc('month',INTERVAL '1 year 2 months 3 days')})")?;
            for (id, text, clock) in [
                (1, "2024-01-31 12:34:56.123456789", "12:34:56"),
                (2, "2024-02-01 23:59:59.999999999", "24:00:00"),
            ] {
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(id),
                        Value::Varchar(text.into()),
                        Value::Varchar(clock.into()),
                    ],
                )?;
            }
            c.execute("INSERT INTO calendar_results VALUES(3,NULL,NULL,NULL,{'d':NULL::DATE,'v':[NULL::TIMESTAMP],'span':NULL::INTERVAL})")?;
            let projection = "SELECT id,k,b,c,p.d,p.v[1],p.span FROM calendar_results ORDER BY id";
            let before = c.query(projection)?;
            assert_eq!(
                before
                    .columns
                    .iter()
                    .map(|column| column.data_type.clone())
                    .collect::<Vec<_>>(),
                vec![
                    DataType::Integer,
                    DataType::Timestamp,
                    DataType::Date,
                    DataType::Time,
                    DataType::Date,
                    DataType::Timestamp,
                    DataType::Interval
                ]
            );
            assert_eq!(
                text_rows(before.clone()),
                vec![
                    vec![
                        "1",
                        "2024-01-31 00:00:00",
                        "2024-01-01",
                        "12:00:00",
                        "2024-01-29",
                        "2024-01-31 12:00:00",
                        "1 year 2 months"
                    ],
                    vec![
                        "2",
                        "2024-02-02 00:00:00",
                        "2024-02-01",
                        "00:00:00",
                        "2024-01-29",
                        "2024-02-02 00:00:00",
                        "1 year 2 months"
                    ],
                    vec!["3", "NULL", "NULL", "NULL", "NULL", "NULL", "NULL"]
                ]
            );
            let lookup = c.prepare(
                "SELECT id FROM calendar_results WHERE k=date_trunc('day',CAST($1 AS TIMESTAMP))",
            )?;
            assert_eq!(
                c.execute_prepared(&lookup, &[Value::Varchar("2024-02-02 19:00:00".into())])?
                    .rows,
                vec![vec![Value::Integer(2)]]
            );
            assert_eq!(c.query("SELECT count(*) FROM calendar_results a JOIN calendar_results b ON time_bucket(INTERVAL '1 week',a.k)=time_bucket(INTERVAL '1 week',b.k)")?.rows,vec![vec![Value::Integer(4)]]);
            assert_eq!(text_rows(c.query("SELECT time_bucket(INTERVAL '1 week',k),count(*) FROM calendar_results GROUP BY time_bucket(INTERVAL '1 week',k) ORDER BY 1 NULLS LAST")?),vec![vec!["2024-01-29 00:00:00","2"],vec!["NULL","1"]]);
            assert_eq!(text_rows(c.query("SELECT min(date_trunc('month',k)) OVER(ORDER BY id),row_number() OVER(PARTITION BY time_bucket(INTERVAL '1 week',k) ORDER BY id) FROM calendar_results ORDER BY id")?),vec![vec!["2024-01-01 00:00:00","1"],vec!["2024-01-01 00:00:00","2"],vec!["2024-01-01 00:00:00","1"]]);
            assert!(
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(4),
                        Value::Varchar("2024-01-31 00:00:01".into()),
                        Value::Varchar("12:00:00".into())
                    ]
                )
                .is_err()
            );
            assert!(matches!(
                c.execute("UPDATE calendar_results SET b=time_bucket(INTERVAL '0 days',b)"),
                Err(Error::Unsupported(_))
            ));
            assert_eq!(c.query(projection)?.rows, before.rows);
            c.execute("BEGIN; UPDATE calendar_results SET k=date_trunc('hour',TIMESTAMP '2024-03-01 12:34:56'),p={'d':time_bucket(INTERVAL '1 month',DATE '2024-03-04'),'v':[date_trunc('month',TIMESTAMP '2024-03-04 12:34:56')],'span':date_trunc('year',INTERVAL '14 months')} WHERE id=1; DELETE FROM calendar_results WHERE id=2; ROLLBACK")?;
            assert_eq!(c.query(projection)?.rows, before.rows);
            drop(c);
            let mut c = open()?.connect();
            assert_eq!(c.query(projection)?.rows, before.rows);
            c.checkpoint()?;
            c.execute("UPDATE calendar_results SET k=date_trunc('hour',TIMESTAMP '2024-03-01 12:34:56'),c=time_bucket(INTERVAL '1 hour',TIME '00:00:00',TIME '00:30:00'),p={'d':time_bucket(INTERVAL '1 month',DATE '2024-03-04'),'v':[date_trunc('month',TIMESTAMP '2024-03-04 12:34:56')],'span':date_trunc('year',INTERVAL '14 months')} WHERE id=1")?;
            let committed = c.query(projection)?.rows;
            assert_eq!(
                committed[0][3],
                Value::Temporal(TemporalValue::Time(84_600_000_000))
            );
            drop(c);
            let mut c = open()?.connect();
            assert_eq!(c.query(projection)?.rows, committed);
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
