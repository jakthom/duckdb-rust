use super::*;
use duckdb_rust::{common::Error, optimizer::IdentityOptimizer};

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn variant_clock_strings_use_strict_selected_policy_in_scalar_batch_and_nested_values() -> Result<()>
{
    // Development Time::TryConvertInternal accepts HH:MM only when the final
    // minute has two digits under strict conversion. TIMETZ deliberately
    // parses its clock non-strictly, then strictly consumes the offset.
    let cases = [
        (
            "\t12:34:56\t",
            Some("12:34:56"),
            Some("12:34:56"),
            Some("12:34:56"),
            Some("12:34:56+00"),
        ),
        ("1:", Some("01:00:00"), None, None, Some("01:00:00+00")),
        ("1:2", Some("01:02:00"), None, None, Some("01:02:00+00")),
        (
            "1:02",
            Some("01:02:00"),
            Some("01:02:00"),
            Some("01:02:00"),
            Some("01:02:00+00"),
        ),
        ("1:02 ", None, None, None, None),
        ("1:2:", Some("01:02:00"), None, None, Some("01:02:00+00")),
        (
            "1:2:3",
            Some("01:02:03"),
            Some("01:02:03"),
            Some("01:02:03"),
            Some("01:02:03+00"),
        ),
        (
            "12:34:56 ",
            Some("12:34:56"),
            Some("12:34:56"),
            Some("12:34:56"),
            Some("12:34:56+00"),
        ),
        ("12:34:56junk", Some("12:34:56"), None, None, None),
        ("12:34:56Z", Some("12:34:56"), None, None, None),
        (
            "12:34:56+02",
            Some("12:34:56"),
            None,
            None,
            Some("12:34:56+02"),
        ),
        ("12:34:56+02junk", Some("12:34:56"), None, None, None),
        (
            "12:34:56+02 ",
            Some("12:34:56"),
            None,
            None,
            Some("12:34:56+02"),
        ),
        ("2000-01-01 12:34:56", Some("12:34:56"), None, None, None),
        (
            "24:00:00.000000999",
            Some("24:00:00"),
            Some("24:00:00"),
            Some("24:00:00.000000999"),
            Some("24:00:00+00"),
        ),
        ("24:00:00.000001", None, None, None, None),
        (
            "1:2:3.",
            Some("01:02:03"),
            Some("01:02:03"),
            Some("01:02:03"),
            Some("01:02:03+00"),
        ),
        (
            "000000001:02",
            Some("01:02:00"),
            Some("01:02:00"),
            Some("01:02:00"),
            Some("01:02:00+00"),
        ),
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
        c.execute("CREATE TABLE clock_text(id INTEGER,s VARCHAR)")?;
        let insert = c.prepare("INSERT INTO clock_text VALUES ($1,$2)")?;
        let mut expected = Vec::new();
        for (i, (input, ordinary, time, nanos, zoned)) in cases.iter().enumerate() {
            c.execute_prepared(
                &insert,
                &[Value::Integer(i as i128), Value::Varchar((*input).into())],
            )?;
            expected.push(
                [ordinary, time, nanos, zoned]
                    .into_iter()
                    .map(|value| value.map_or(Value::Null, |text| Value::Varchar(text.into())))
                    .collect::<Vec<_>>(),
            );
        }
        c.execute_prepared(&insert, &[Value::Integer(cases.len() as i128), Value::Null])?;
        expected.push(vec![Value::Null; 4]);
        assert_eq!(c.query("SELECT TRY_CAST(s AS TIME)::VARCHAR,TRY_CAST(s::VARIANT AS TIME)::VARCHAR,TRY_CAST(s::VARIANT AS TIME_NS)::VARCHAR,TRY_CAST(s::VARIANT AS TIMETZ)::VARCHAR FROM clock_text ORDER BY id")?.rows, expected);
        assert_eq!(c.query("SELECT TRY_CAST(['1:','2:03']::VARIANT AS TIME[]),TRY_CAST(['1:','2:03'] AS TIME[])::VARCHAR,CAST({'t':'1:02'}::VARIANT AS STRUCT(t TIME))::VARCHAR,TRY_CAST({'t':'1:2'}::VARIANT AS STRUCT(t TIME))")?.rows, vec![vec![Value::Null,Value::Varchar("['01:00:00', '02:03:00']".into()),Value::Varchar("{'t': '01:02:00'}".into()),Value::Null]]);
        assert_eq!(c.query("SELECT (make_time(23,59,60.5)::VARIANT)::TIME,(make_time(23,59,60.5)::VARIANT)::TIME_NS")?.rows, vec![vec![Value::Temporal(TemporalValue::Time(86_400_500_000)),Value::Temporal(TemporalValue::TimeNs(86_400_500_000_000))]]);
        // Timestamp casts intentionally ignore the core's strict argument;
        // VARIANT provenance must not install TIME's policy on every temporal.
        assert_eq!(c.query("SELECT ('2000-01-01 12:34:56+02'::VARIANT)::TIMESTAMP::VARCHAR,('2000-01-01 12:34:56+02'::VARIANT)::TIMESTAMPTZ::VARCHAR,('2000-01-01 12:34:56 Europe/Amsterdam'::VARIANT)::TIMESTAMP::VARCHAR")?.rows,vec![vec![Value::Varchar("2000-01-01 12:34:56".into()),Value::Varchar("2000-01-01 10:34:56+00".into()),Value::Varchar("2000-01-01 12:34:56".into())]]);
        for kind in ["TIME", "TIME_NS", "TIME WITH TIME ZONE"] {
            let error = c
                .query(&format!("SELECT CAST('12:34:56junk'::VARIANT AS {kind})"))
                .unwrap_err();
            assert!(
                matches!(error, Error::Conversion(message) if message == format!("Can't convert VARIANT(VARCHAR) value '12:34:56junk' to '{kind}'"))
            );
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn strict_variant_clock_casts_cross_prepared_indexes_mutations_wal_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        for hashed in [false, true] {
            let path = directory
                .path()
                .join(format!("strict-variant-clock-{batched}-{hashed}.duckdb"));
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
            c.execute("CREATE TABLE clocks(id INTEGER,t TIME_NS UNIQUE,z TIMETZ,payload STRUCT(t TIME_NS))")?;
            let insert = c.prepare("INSERT INTO clocks VALUES ($1,CAST($2::VARIANT AS TIME_NS),CAST($3::VARIANT AS TIMETZ),CAST({'t':$2}::VARIANT AS STRUCT(t TIME_NS)))")?;
            for (id, t, z) in [
                (1, "12:34:56.123456789", "12:34:56+02"),
                (2, "24:00:00.000000999", "24:00:00-15:59:59"),
            ] {
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(id),
                        Value::Varchar(t.into()),
                        Value::Varchar(z.into()),
                    ],
                )?;
            }
            c.execute_prepared(&insert, &[Value::Integer(3), Value::Null, Value::Null])?;
            let projection = "SELECT id,t,z,struct_extract(payload,'t') FROM clocks ORDER BY id";
            let before = c.query(projection)?.rows;
            for invalid in ["1:2", "12:34:56+02", "2000-01-01 12:34:56"] {
                assert!(matches!(
                    c.execute_prepared(
                        &insert,
                        &[
                            Value::Integer(4),
                            Value::Varchar(invalid.into()),
                            Value::Varchar("1:2".into())
                        ]
                    ),
                    Err(Error::Conversion(_))
                ));
                assert_eq!(c.query(projection)?.rows, before);
            }
            assert!(matches!(
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(4),
                        Value::Varchar("24:00:00.000000999".into()),
                        Value::Varchar("12:34:56+02".into())
                    ]
                ),
                Err(Error::Constraint(_))
            ));
            assert_eq!(c.query(projection)?.rows, before);
            assert_eq!(
                c.query("SELECT count(*) FROM clocks a JOIN clocks b ON a.t=b.t")?
                    .rows,
                vec![vec![Value::Integer(2)]]
            );
            assert_eq!(
                c.query("SELECT t,count(*) FROM clocks GROUP BY t ORDER BY t")?
                    .rows,
                vec![
                    vec![before[0][1].clone(), Value::Integer(1)],
                    vec![before[1][1].clone(), Value::Integer(1)],
                    vec![Value::Null, Value::Integer(1)]
                ]
            );
            assert_eq!(c.query("SELECT max(t) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM clocks ORDER BY id")?.rows,vec![vec![before[0][1].clone()],vec![before[1][1].clone()],vec![before[1][1].clone()]]);
            c.execute("BEGIN; UPDATE clocks SET t=CAST('01:02'::VARIANT AS TIME_NS) WHERE id=1; DELETE FROM clocks WHERE t=TIME_NS '24:00:00.000000999'; ROLLBACK")?;
            assert_eq!(c.query(projection)?.rows, before);
            assert!(
                c.execute("UPDATE clocks SET t=CAST('1:2'::VARIANT AS TIME_NS)")
                    .is_err()
            );
            assert_eq!(c.query(projection)?.rows, before);
            c.execute("UPDATE clocks SET t=CAST('01:02'::VARIANT AS TIME_NS),payload=CAST({'t':'01:02'}::VARIANT AS STRUCT(t TIME_NS)) WHERE id=1")?;
            let committed = c.query(projection)?.rows;
            drop(c);
            let mut c = open()?.connect();
            assert_eq!(c.query(projection)?.rows, committed);
            assert_eq!(
                c.query("SELECT id FROM clocks WHERE t=CAST('01:02'::VARIANT AS TIME_NS)")?
                    .rows,
                vec![vec![Value::Integer(1)]]
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
