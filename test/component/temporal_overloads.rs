//! Candidate bodies are a scoped contract, not full SQL-location parity.
use super::*;
use duckdb_rust::function::{FunctionRegistry, ScalarBindArguments, ScalarSignature};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn expected_body(name: &str, actual: &str, candidates: &[&str], ambiguous: bool) -> String {
    let call = format!("{name}({actual})");
    let mut body = if ambiguous {
        format!(
            "Could not choose a best candidate function for the function call \"{call}\". In order to select one, please add explicit type casts.\n\tCandidate functions:\n"
        )
    } else {
        format!(
            "No function matches the given name and argument types '{call}'. You might need to add explicit type casts.\n\tCandidate functions:\n"
        )
    };
    for candidate in candidates {
        body.push_str(&format!("\t{name}({candidate}\n"));
    }
    body
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_candidate_bodies_keep_native_order_alias_identity_and_literal_types() -> Result<()> {
    let mut c = DatabaseBuilder::new().build()?.connect();
    let trunc = [
        "col0 VARCHAR, col1 DATE) -> TIMESTAMP",
        "col0 VARCHAR, col1 INTERVAL) -> INTERVAL",
        "col0 VARCHAR, col1 TIMESTAMP) -> TIMESTAMP",
        "col0 VARCHAR, col1 TIMESTAMP WITH TIME ZONE) -> TIMESTAMP WITH TIME ZONE",
    ];
    for name in ["date_trunc", "datetrunc"] {
        for (args, actual, candidates, ambiguous) in [
            (
                "'day','2001-02-03'",
                "STRING_LITERAL, STRING_LITERAL",
                vec![trunc[1], trunc[2], trunc[3], trunc[0]],
                true,
            ),
            (
                "'day',NULL",
                "STRING_LITERAL, \"NULL\"",
                vec![trunc[1], trunc[0]],
                true,
            ),
            (
                "'day',TIME '12:00:00'",
                "STRING_LITERAL, TIME",
                trunc.to_vec(),
                false,
            ),
            (
                "'day',TIME_NS '12:00:00'",
                "STRING_LITERAL, TIME_NS",
                trunc.to_vec(),
                false,
            ),
            (
                "'day',TIMETZ '12:00:00+00'",
                "STRING_LITERAL, TIME WITH TIME ZONE",
                trunc.to_vec(),
                false,
            ),
            (
                "1,DATE 'epoch'",
                "INTEGER_LITERAL, DATE",
                trunc.to_vec(),
                false,
            ),
            (
                "'day',true",
                "STRING_LITERAL, BOOLEAN",
                trunc.to_vec(),
                false,
            ),
        ] {
            let sql = format!("SELECT {name}({args})");
            let Err(Error::Bind(body)) = c.query(&sql) else {
                panic!("expected selected candidate error: {sql}");
            };
            assert_eq!(
                body,
                expected_body(name, actual, &candidates, ambiguous),
                "{sql}"
            );
        }
    }
    let buckets = [
        "col0 INTERVAL, col1 DATE) -> DATE",
        "col0 INTERVAL, col1 DATE, col2 DATE) -> DATE",
        "col0 INTERVAL, col1 DATE, col2 INTERVAL) -> DATE",
        "col0 INTERVAL, col1 TIME) -> TIME",
        "col0 INTERVAL, col1 TIME, col2 INTERVAL) -> TIME",
        "col0 INTERVAL, col1 TIME, col2 TIME) -> TIME",
        "col0 INTERVAL, col1 TIMESTAMP) -> TIMESTAMP",
        "col0 INTERVAL, col1 TIMESTAMP, col2 INTERVAL) -> TIMESTAMP",
        "col0 INTERVAL, col1 TIMESTAMP, col2 TIMESTAMP) -> TIMESTAMP",
        "col0 INTERVAL, col1 TIMESTAMP WITH TIME ZONE) -> TIMESTAMP WITH TIME ZONE",
        "col0 INTERVAL, col1 TIMESTAMP WITH TIME ZONE, col2 INTERVAL) -> TIMESTAMP WITH TIME ZONE",
        "col0 INTERVAL, col1 TIMESTAMP WITH TIME ZONE, col2 TIMESTAMP WITH TIME ZONE) -> TIMESTAMP WITH TIME ZONE",
        "col0 INTERVAL, col1 TIMESTAMP WITH TIME ZONE, col2 VARCHAR) -> TIMESTAMP WITH TIME ZONE",
    ];
    for (args, actual, candidates, ambiguous) in [
        (
            "NULL",
            "INTERVAL, \"NULL\"",
            vec![buckets[3], buckets[0]],
            true,
        ),
        (
            "DATE 'epoch',NULL",
            "INTERVAL, DATE, \"NULL\"",
            vec![buckets[2], buckets[1]],
            true,
        ),
        (
            "'2001-02-03'",
            "INTERVAL, STRING_LITERAL",
            vec![buckets[3], buckets[6], buckets[9], buckets[0]],
            true,
        ),
        (
            "TIME_NS '12:00:00'",
            "INTERVAL, TIME_NS",
            buckets.to_vec(),
            false,
        ),
        (
            "DATE 'epoch','2000-01-03'",
            "INTERVAL, DATE, STRING_LITERAL",
            vec![buckets[2], buckets[1]],
            true,
        ),
    ] {
        let sql = format!("SELECT time_bucket(INTERVAL '1 day',{args})");
        let Err(Error::Bind(body)) = c.query(&sql) else {
            panic!("expected selected candidate error: {sql}");
        };
        assert_eq!(
            body,
            expected_body("time_bucket", actual, &candidates, ambiguous),
            "{sql}"
        );
    }
    Ok(())
}

struct Chosen {
    name: &'static str,
    index: usize,
    count: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for Chosen {
    fn len(&self) -> usize {
        self.count
    }
    fn data_type(&self, _: usize) -> Result<DataType> {
        Err(Error::Internal("unexpected source-type fallback".into()))
    }
    fn constant(&self, _: usize) -> Result<Value> {
        Err(Error::Internal("metadata evaluated an argument".into()))
    }
    fn is_provably_null(&self, _: usize) -> Result<bool> {
        Ok(true)
    }
    fn select_overload(&self, name: &str, _: &[ScalarSignature]) -> Result<usize> {
        assert_eq!(name, self.name);
        Ok(self.index)
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_bindings_validate_frontend_indices_and_retain_unavailable_icu_placeholders()
-> Result<()> {
    let query = QueryContext::background();
    let functions = FunctionRegistry::builtins();
    for name in ["date_trunc", "datetrunc", "time_bucket"] {
        let function = functions.scalar(name)?;
        for chosen in [
            Chosen {
                name,
                index: usize::MAX,
                count: 2,
            },
            Chosen {
                name,
                index: 0,
                count: 1,
            },
        ] {
            assert!(matches!(
                function.bind(&chosen, &query),
                Err(Error::Internal(_))
            ));
        }
        let chosen = Chosen {
            name,
            index: if name == "time_bucket" { 9 } else { 3 },
            count: 2,
        };
        assert!(
            matches!(function.bind(&chosen, &query), Err(Error::Unsupported(message)) if message.contains("ICU"))
        );
        let chosen = Chosen {
            name,
            index: 0,
            count: 2,
        };
        let bound = function
            .bind(&chosen, &query)?
            .expect("owned selected calendar binding");
        assert!(matches!(
            bound.return_type(&[DataType::Time, DataType::Time], query.types()),
            Err(Error::Internal(_))
        ));
        assert!(matches!(
            bound.argument_types(&[], query.types()),
            Err(Error::Internal(_))
        ));
        assert!(matches!(
            function.argument_types(&[DataType::Varchar, DataType::Date], query.types()),
            Err(Error::Unsupported(_))
        ));
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_selection_uses_registered_cast_availability_without_builtin_type_whitelists()
-> Result<()> {
    for fail in [false, true] {
        for batched in [false, true] {
            let mut casts = CastRegistry::builtins();
            casts.register(
                CastSpec {
                    source: DataType::SmallInt,
                    target: DataType::Varchar,
                    mode: CastMode::Implicit,
                },
                Arc::new(CalendarSpecifierCast(fail)),
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
                "SELECT date_trunc(1::SMALLINT,DATE '2001-02-03')",
                "SELECT {'t':datetrunc(p,d)}.t FROM (VALUES (1::SMALLINT,DATE '2001-02-03'),(2::SMALLINT,DATE '2001-02-03')) t(p,d)",
            ] {
                let result = c.query(sql);
                if fail {
                    assert!(matches!(result, Err(Error::Resource(_))));
                } else {
                    for row in text_rows(result?) {
                        assert_eq!(row, vec!["2001-02-01 00:00:00"]);
                    }
                }
            }
            let prepared = c.prepare("SELECT date_trunc(CAST($1 AS SMALLINT),$2)")?;
            let result = c.execute_prepared(
                &prepared,
                &[Value::Integer(1), Value::Date(Date::from_ymd(2001, 2, 3)?)],
            );
            if fail {
                assert!(matches!(result, Err(Error::Resource(_))));
            } else {
                assert_eq!(text_rows(result?), vec![vec!["2001-02-01 00:00:00"]]);
            }
        }
    }
    Ok(())
}
