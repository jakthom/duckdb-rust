use super::*;
use duckdb_rust::{
    Date, Error,
    common::cast::{CastFunction, CastMode, CastRegistry, CastSpec},
    optimizer::IdentityOptimizer,
    parallel::QueryContext,
};

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_differences_keep_period_crossings_complete_months_negative_instants_and_overloads()
-> Result<()> {
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
        let units = [
            ("microsecond", 2, 2),
            ("millisecond", 1, 0),
            ("second", 1, 0),
            ("minute", 1, 0),
            ("hour", 1, 0),
            ("day", 1, 0),
            ("week", 0, 0),
            ("month", 1, 0),
            ("quarter", 1, 0),
            ("year", 1, 0),
            ("decade", 1, 0),
            ("century", 0, 0),
            ("millennium", 0, 0),
        ];
        for (unit, crossings, complete) in units {
            for (name, expected) in [
                ("date_diff", crossings),
                ("datediff", crossings),
                ("date_sub", complete),
                ("datesub", complete),
            ] {
                let result=c.query(&format!("SELECT {name}('{unit}',TIMESTAMP '1969-12-31 23:59:59.999999',TIMESTAMP '1970-01-01 00:00:00.000001'),{name}('{unit}',TIMESTAMP '1970-01-01 00:00:00.000001',TIMESTAMP '1969-12-31 23:59:59.999999')"))?;
                assert_eq!(
                    result.rows,
                    vec![vec![Value::Integer(expected), Value::Integer(-expected)]],
                    "{name}/{unit}"
                );
                assert!(
                    result
                        .columns
                        .iter()
                        .all(|column| column.data_type == DataType::BigInt)
                );
            }
        }
        assert_eq!(c.query("SELECT date_sub('month',TIMESTAMP '2024-01-31 12:00:00',TIMESTAMP '2024-02-29 12:00:00'),date_sub('month',TIMESTAMP '2024-01-31 12:00:00',TIMESTAMP '2024-02-29 11:59:59.999999'),date_sub('month',DATE '2023-01-31',DATE '2023-02-28'),date_sub('month',DATE '2023-01-31',DATE '2023-02-27'),date_sub('month',DATE '2023-02-28',DATE '2023-01-31')")?.rows,vec![vec![Value::Integer(1),Value::Integer(0),Value::Integer(1),Value::Integer(0),Value::Integer(-1)]]);
        assert_eq!(c.query("SELECT date_diff('week',DATE '2018-12-30',DATE '2018-12-31'),date_diff('isoyear',DATE '2018-12-30',DATE '2018-12-31'),date_sub('isoyear',DATE '2018-12-30',DATE '2018-12-31'),date_diff('year',DATE '0002-01-01 (BC)',DATE '0001-01-01 (BC)'),date_diff('decade',DATE '0002-01-01 (BC)',DATE '0001-01-01 (BC)'),date_diff('century',DATE '0002-01-01 (BC)',DATE '0001-01-01 (BC)')")?.rows,vec![vec![Value::Integer(0),Value::Integer(1),Value::Integer(0),Value::Integer(1),Value::Integer(0),Value::Integer(0)]]);
        assert_eq!(c.query("SELECT date_diff('day',DATE '5877642-06-25 (BC)',DATE '5881580-07-10'),date_diff('day',make_timestamp(-9223372036854775806),TIMESTAMP 'epoch'),date_diff('year',make_timestamp(-9223372036854775806),TIMESTAMP 'epoch'),date_sub('hour',make_timestamp(-9223372036854775806),TIMESTAMP 'epoch'),date_diff('hour',make_timestamp(-9223372036854775806),TIMESTAMP 'epoch')")?.rows,vec![vec![Value::Integer(4294967292),Value::Integer(106751992),Value::Integer(292278),Value::Integer(2562047788),Value::Integer(2562047789)]]);
        assert_eq!(c.query("SELECT date_diff('microsecond',TIME '24:00:00',make_time(23,59,60.49999999999999)),date_sub('second',TIME '24:00:00',make_time(23,59,60.49999999999999)),date_diff('day',DATE 'epoch','1970-01-02'),date_diff('microsecond',TIMESTAMP_NS '1969-12-31 23:59:59.999999999',TIMESTAMP 'epoch')")?.rows,vec![vec![Value::Integer(500000),Value::Integer(0),Value::Integer(1),Value::Integer(0)]]);
        for function in ["date_diff", "date_sub"] {
            c.execute("CREATE TABLE calendar_null_input(d DATE); INSERT INTO calendar_null_input VALUES(NULL)")?;
            assert!(
                matches!(c.query(&format!("SELECT {function}('bad',d,DATE 'epoch') FROM calendar_null_input")),Err(Error::Conversion(message)) if message=="extract specifier \"bad\" not recognized")
            );
            c.execute("DROP TABLE calendar_null_input")?;
            let null_result = c.query(&format!("SELECT {function}('bad',NULL::DATE,CAST('bad' AS DATE)),{function}('bad',TRY_CAST('bad' AS DATE),DATE 'epoch'),{function}('bad',DATE 'epoch',NULL::DATE)"))?;
            assert_eq!(null_result.rows, vec![vec![Value::Null; 3]]);
            assert!(
                null_result
                    .columns
                    .iter()
                    .all(|column| column.data_type == DataType::BigInt)
            );
            assert_eq!(c.query(&format!("SELECT {function}('day',DATE 'infinity',DATE 'epoch'),{function}('bad',NULL::DATE,DATE 'epoch'),{function}(NULL,DATE 'epoch',DATE 'epoch')"))?.rows,vec![vec![Value::Null;3]]);
            assert!(
                matches!(c.query(&format!("SELECT {function}('bad',DATE 'infinity',DATE 'epoch')")),Err(Error::Conversion(message)) if message=="extract specifier \"bad\" not recognized")
            );
            assert!(
                matches!(c.query(&format!("SELECT {function}('timezone',DATE 'infinity',DATE 'epoch')")),Err(Error::Unsupported(message)) if message.contains("Specifier type not implemented"))
            );
            assert_eq!(c.query(&format!("SELECT {function}(p,d,DATE 'epoch') FROM (VALUES ('bad',DATE 'infinity'),('timezone',NULL)) t(p,d)"))?.rows,vec![vec![Value::Null];2]);
            for args in [
                "'day',NULL,NULL",
                "'day','2000-01-01','2000-01-02'",
                "'hour',TIME_NS '12:00:00',TIME_NS '13:00:00'",
                "'hour',TIMETZ '12:00:00+02',TIMETZ '13:00:00+00'",
                "'day',TIME '12:00:00',DATE 'epoch'",
                "1,DATE 'epoch',DATE 'epoch'",
                "'day',NULL::DATE,true",
                "'day',NULL::DATE,'bad'::VARCHAR",
                "'day',NULL::DATE,'1970-01-01'::ENUM('1970-01-01')",
            ] {
                assert!(
                    matches!(
                        c.query(&format!("SELECT {function}({args})")),
                        Err(Error::Bind(_))
                    ),
                    "{function}({args})"
                );
            }
            assert!(
                matches!(c.query(&format!("SELECT {function}('day',TIME '12:00:00',TIME '13:00:00')")),Err(Error::Unsupported(message)) if message=="\"time\" units \"day\" not recognized")
            );
        }
        for (sql, expected) in [
            (
                "SELECT date_diff('microseconds',DATE '5881580-07-10',DATE '5877642-06-25 (BC)')",
                "Could not convert DATE (5881580-07-10) to microseconds",
            ),
            (
                "SELECT date_diff('milliseconds',DATE '5881580-07-10',DATE '5877642-06-25 (BC)')",
                "Could not convert DATE (5877642-06-25 (BC)) to microseconds",
            ),
            (
                "SELECT date_sub('day',DATE '5881580-07-10',DATE '5881580-07-10')",
                "Date and time not in timestamp range",
            ),
            (
                "SELECT date_sub('year',make_timestamp(-9223372036854775806),TIMESTAMP 'epoch')",
                "Date out of range in timestamp conversion",
            ),
        ] {
            assert!(
                matches!(c.query(sql),Err(Error::Conversion(message)) if message==expected),
                "{sql}"
            );
        }
        assert!(
            matches!(c.query("SELECT date_sub('day',DATE '0001-01-01',DATE '294247-01-10')"),Err(Error::OutOfRange(message)) if message=="Overflow in subtraction of INT64 (9223372022400000000 - -62135596800000000)!")
        );
        assert_eq!(
            c.query(
                "SELECT CASE WHEN false THEN date_diff('bad',DATE 'epoch',DATE 'epoch') ELSE 7 END"
            )?
            .rows,
            vec![vec![Value::Integer(7)]]
        );
        for function in ["date_diff", "date_sub"] {
            assert_eq!(c.query(&format!("SELECT CASE WHEN false THEN {function}(CAST(CAST('bad' AS INTEGER) AS VARCHAR),DATE 'epoch',DATE 'epoch') ELSE 7 END"))?.rows,vec![vec![Value::Integer(7)]]);
            assert!(matches!(
                c.query(&format!(
                    "SELECT {function}(CAST('bad' AS INTEGER),NULL,NULL)"
                )),
                Err(Error::Bind(_))
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct DifferenceUnitCast(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for DifferenceUnitCast {
    fn name(&self) -> &'static str {
        "selected-calendar-unit"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.mode == CastMode::Implicit && spec.target == DataType::Varchar
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        if self.0 {
            Err(Error::Resource("selected calendar unit failed".into()))
        } else {
            Ok(Value::Varchar("month".into()))
        }
    }
}

#[derive(Debug)]
struct DifferenceNullProbeCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for DifferenceNullProbeCast {
    fn name(&self) -> &'static str {
        "calendar-null-probe"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target == DataType::Date
            && spec.mode == CastMode::Explicit
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match value {
            Value::Varchar(text) if text == "resource" => {
                Err(Error::Resource("calendar NULL probe resource".into()))
            }
            Value::Varchar(text) if text == "internal" => {
                Err(Error::Internal("calendar NULL probe internal".into()))
            }
            Value::Varchar(text) if text == "invalid" => Ok(Value::Integer(0)),
            Value::Varchar(text) if text == "conversion" => {
                Err(Error::Conversion("calendar input rejected".into()))
            }
            Value::Varchar(text) if text == "range" => {
                Err(Error::OutOfRange("calendar input range".into()))
            }
            _ => Ok(Value::Date(Date::from_days(0)?)),
        }
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_null_template_preserves_selected_fatal_failures_and_valid_overload_first() -> Result<()>
{
    for batched in [false, true] {
        let mut casts = CastRegistry::builtins();
        casts.replace(
            CastSpec {
                source: DataType::Varchar,
                target: DataType::Date,
                mode: CastMode::Explicit,
            },
            Arc::new(DifferenceNullProbeCast),
        )?;
        let mut c = DatabaseBuilder::new()
            .casts(casts)
            .optimizer(Arc::new(IdentityOptimizer))
            .expressions(if batched {
                Arc::new(BatchedEvaluator)
            } else {
                Arc::new(ScalarEvaluator)
            })
            .build()?
            .connect();
        for function in ["date_diff", "date_sub"] {
            for input in ["resource", "internal", "invalid"] {
                let error = c
                    .query(&format!(
                        "SELECT TRY_CAST({function}('day','{input}'::DATE,NULL::DATE) AS BIGINT)"
                    ))
                    .unwrap_err();
                assert!(
                    match input {
                        "resource" => matches!(error, Error::Resource(_)),
                        _ => matches!(error, Error::Internal(_)),
                    },
                    "{input}"
                );
                assert!(matches!(
                    c.query(&format!("SELECT {function}(1,'{input}'::DATE,NULL::DATE)")),
                    Err(Error::Bind(_))
                ));
            }
            let prepared = c.prepare(&format!(
                "SELECT {function}('day',CAST($1 AS DATE),NULL::DATE)"
            ))?;
            for input in ["conversion", "range"] {
                assert_eq!(
                    c.execute_prepared(&prepared, &[Value::Varchar(input.into())])?
                        .rows,
                    vec![vec![Value::Null]]
                );
                // The unsuccessful speculative probe does not suppress the
                // same conversion when execution actually needs this child.
                assert!(matches!(
                    c.query(&format!(
                        "SELECT {function}('day','{input}'::DATE,DATE 'epoch')"
                    )),
                    Err(Error::Conversion(_) | Error::OutOfRange(_))
                ));
            }
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_difference_constant_provenance_keeps_selected_enum_casts_and_prepared_types()
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
                Arc::new(DifferenceUnitCast(failed)),
            )?;
            let mut c = DatabaseBuilder::new()
                .casts(casts)
                .optimizer(Arc::new(IdentityOptimizer))
                .batch_size(2)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .build()?
                .connect();
            c.execute("CREATE TABLE periods(p ENUM('day'),d DATE); INSERT INTO periods VALUES ('day',DATE '2000-01-01'),('day',DATE '2000-01-02')")?;
            for sql in [
                "SELECT date_diff('day'::ENUM('day'),DATE '2000-01-01',DATE '2000-02-15')",
                "SELECT date_diff(p,d,DATE '2000-02-15') FROM periods ORDER BY d",
            ] {
                let result = c.query(sql);
                if failed {
                    assert!(matches!(result, Err(Error::Resource(_))));
                } else {
                    assert!(
                        result?
                            .rows
                            .iter()
                            .all(|row| row == vec![Value::Integer(1)])
                    );
                }
            }
            let prepared = c.prepare("SELECT date_diff($1,$2,$3),date_sub($1,$2,$3)")?;
            let result = c.execute_prepared(
                &prepared,
                &[
                    Value::enumeration(&enumeration, 0)?,
                    Value::Date(Date::EPOCH),
                    Value::Date("1970-02-15".parse()?),
                ],
            );
            if failed {
                assert!(matches!(result, Err(Error::Resource(_))));
            } else {
                assert_eq!(result?.rows, vec![vec![Value::Integer(1); 2]]);
            }
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_differences_cross_nested_parameters_indexes_relations_wal_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        for hashed in [false, true] {
            let path = directory
                .path()
                .join(format!("calendar-periods-{batched}-{hashed}.duckdb"));
            let open = || {
                let checkpoint = FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    Arc::new(DuckDbFormat::default()),
                )?
                .with_recovery(Arc::new(DuckDbWalRecovery))?;
                DatabaseBuilder::new()
                    .optimizer(Arc::new(IdentityOptimizer))
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
            c.execute("CREATE TABLE periods(k BIGINT UNIQUE,d DATE,a TIMESTAMP,b TIMESTAMP,t TIME,p STRUCT(d DATE,t TIME),whole BIGINT DEFAULT datesub('month',DATE '2000-01-31',DATE '2000-02-29'),raw TIMESTAMP DEFAULT TIMESTAMP 'epoch')")?;
            let insert=c.prepare("INSERT INTO periods(k,d,a,b,t,p,raw) VALUES(date_diff('day',DATE 'epoch',DATE($1)),DATE($1),CAST($2 AS TIMESTAMP),CAST($3 AS TIMESTAMP),$4,{'d':DATE($1),'t':$4},make_timestamp(-9223372036854775806))")?;
            for (day, start, end, clock) in [
                (
                    Value::Varchar("2000-01-01".into()),
                    Value::Varchar("2024-01-31 12:00:00".into()),
                    Value::Varchar("2024-02-29 12:00:00".into()),
                    Value::Temporal(TemporalValue::Time(86400500000)),
                ),
                (
                    Value::Varchar("2000-01-02".into()),
                    Value::Varchar("1969-12-31 23:59:59.999999".into()),
                    Value::Varchar("1970-01-01 00:00:00.000001".into()),
                    Value::Temporal(TemporalValue::Time(1)),
                ),
                (Value::Null, Value::Null, Value::Null, Value::Null),
            ] {
                c.execute_prepared(&insert, &[day, start, end, clock])?;
            }
            let query = "SELECT k,date_diff('month',a,b),date_sub('month',a,b),date_diff('microsecond',TIME '24:00:00',p.t),p.d,date_diff('year',raw,TIMESTAMP 'epoch') FROM periods ORDER BY k NULLS LAST";
            let before = c.query(query)?.rows;
            assert_eq!(
                before,
                vec![
                    vec![
                        Value::Integer(10957),
                        Value::Integer(1),
                        Value::Integer(1),
                        Value::Integer(500000),
                        Value::Date("2000-01-01".parse()?),
                        Value::Integer(292278)
                    ],
                    vec![
                        Value::Integer(10958),
                        Value::Integer(1),
                        Value::Integer(0),
                        Value::Integer(-86399999999),
                        Value::Date("2000-01-02".parse()?),
                        Value::Integer(292278)
                    ],
                    vec![
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Integer(292278)
                    ]
                ]
            );
            assert_eq!(c.query("SELECT count(*) FROM periods a JOIN periods b ON date_diff('day',a.p.d,b.p.d)=0")?.rows,vec![vec![Value::Integer(2)]]);
            assert_eq!(c.query("SELECT date_diff('month',a,b),count(*) FROM periods GROUP BY date_diff('month',a,b) ORDER BY 1 NULLS LAST")?.rows,vec![vec![Value::Integer(1),Value::Integer(2)],vec![Value::Null,Value::Integer(1)]]);
            assert_eq!(c.query("SELECT max(date_sub('month',a,b)) OVER(ORDER BY k NULLS LAST ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM periods ORDER BY k NULLS LAST")?.rows,vec![vec![Value::Integer(1)];3]);
            c.execute(
                "BEGIN; UPDATE periods SET whole=9; DELETE FROM periods WHERE k=10957; ROLLBACK",
            )?;
            assert_eq!(c.query(query)?.rows, before);
            assert_eq!(
                c.query("SELECT whole FROM periods ORDER BY k NULLS LAST")?
                    .rows,
                vec![vec![Value::Integer(1)]; 3]
            );
            assert!(matches!(c.execute("UPDATE periods SET whole=date_diff('microsecond',make_timestamp(-9223372036854775806),make_timestamp(9223372036854775806))"),Err(Error::OutOfRange(_))));
            assert_eq!(
                c.query("SELECT whole FROM periods ORDER BY k NULLS LAST")?
                    .rows,
                vec![vec![Value::Integer(1)]; 3]
            );
            c.execute("UPDATE periods SET whole=date_sub('month',a,b)")?;
            drop(c);
            let mut c = open()?.connect();
            assert_eq!(c.query(query)?.rows, before);
            assert_eq!(
                c.query("SELECT whole FROM periods ORDER BY k NULLS LAST")?
                    .rows,
                vec![
                    vec![Value::Integer(1)],
                    vec![Value::Integer(0)],
                    vec![Value::Null]
                ]
            );
            let lookup=c.prepare("SELECT date_diff('day',p.d,DATE('2000-01-10')) FROM periods WHERE k=datediff('day',DATE 'epoch',DATE($1))")?;
            assert_eq!(
                c.execute_prepared(&lookup, &[Value::Varchar("2000-01-02".into())])?
                    .rows,
                vec![vec![Value::Integer(8)]]
            );
            c.checkpoint()?;
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?
                    .connect()
                    .query(query)?
                    .rows,
                before
            );
        }
    }
    Ok(())
}
