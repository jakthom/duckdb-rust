//! Executable mixed-clock assertion matrix. The ordinary suite runs checkpoint
//! modes; the standalone example also runs the retained native nested WAL path.
use duckdb_rust::execution::{
    expression_executor::{BatchedEvaluator, ScalarEvaluator},
    index::{BTreeIndexFactory, HashIndexFactory},
};
use duckdb_rust::storage::{
    checkpoint::{Durability, FileCheckpoint},
    duckdb::{
        DuckDbFormat,
        wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
    },
    filesystem::OpenMode,
    format::{JsonSnapshotFormat, SnapshotFormat},
    logged::FileWal,
};
use duckdb_rust::{DatabaseBuilder, Result, TemporalValue, Value};
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn run(native_wal: bool) -> Result<()> {
    use duckdb_rust::common::temporal::{MAX_CLOCK_MICROS, MICROS_PER_DAY};
    let directory = tempfile::tempdir()?;
    for native in [false, true] {
        for batched in [false, true] {
            let path = directory
                .path()
                .join(format!("clock-domain-{native}-{batched}.db"));
            let open = || {
                let format: Arc<dyn SnapshotFormat> = if native {
                    Arc::new(DuckDbFormat::default())
                } else {
                    Arc::new(JsonSnapshotFormat)
                };
                let checkpoint = FileCheckpoint::open(&path, OpenMode::ReadWrite, format)?;
                let durability: Arc<dyn Durability> = if native && native_wal {
                    Arc::new(FileWal::new(
                        checkpoint.with_recovery(Arc::new(DuckDbWalRecovery))?,
                        Arc::new(DuckDbTransactionLog),
                    )?)
                } else {
                    Arc::new(checkpoint)
                };
                DatabaseBuilder::new()
                    .batch_size(1)
                    .indexes(if batched {
                        Arc::new(HashIndexFactory)
                    } else {
                        Arc::new(BTreeIndexFactory)
                    })
                    .expressions(if batched {
                        Arc::new(BatchedEvaluator)
                    } else {
                        Arc::new(ScalarEvaluator)
                    })
                    .durability(durability)
                    .build()
            };
            let mut c = open()?.connect();
            c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,n TIME_NS UNIQUE,u TIME,z TIMETZ,child STRUCT(n TIME_NS,z TIMETZ),items TIME_NS[])")?;
            let insert = c.prepare("INSERT INTO t SELECT $1,n,n::TIME,(n::TIME)::TIMETZ,{'n':n,'z':(n::TIME)::TIMETZ},[n,NULL] FROM (SELECT CAST($2 AS TIME_NS) AS n) s")?;
            for (id, extra) in [0, 1, 499, 500, 999, 1000, 500_000_000]
                .into_iter()
                .enumerate()
            {
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(id as i128),
                        Value::Temporal(TemporalValue::TimeNs(MICROS_PER_DAY * 1000 + extra)),
                    ],
                )?;
            }
            for invalid in [
                TemporalValue::Time(-1),
                TemporalValue::Time(MAX_CLOCK_MICROS + 1),
                TemporalValue::TimeNs(MAX_CLOCK_MICROS * 1000 + 1),
                TemporalValue::TimeTz {
                    micros: MAX_CLOCK_MICROS + 1,
                    offset: 0,
                },
            ] {
                assert!(invalid.validate().is_err());
                assert!(!Value::Temporal(invalid).fits_type(&invalid.data_type()));
            }
            let before = c.query("SELECT * FROM t ORDER BY n")?.rows;
            assert_eq!(before.len(), 7);
            assert_eq!(before[2][2].to_string(), "24:00:00");
            assert_eq!(before[3][2].to_string(), "24:00:00.000001");
            assert_eq!(before[4][3].to_string(), "24:00:00.000001+00");
            assert_eq!(before[6][2].to_string(), "24:00:00.5");
            assert_eq!(
                c.query("SELECT make_time(23,59,60.5)")?.rows[0][0],
                Value::Temporal(TemporalValue::Time(MAX_CLOCK_MICROS))
            );
            assert!(
                c.query("SELECT make_time(23,59,60.50000000000001)")
                    .is_err()
            );
            for offset in [-57599, 0, 57599] {
                let value = TemporalValue::TimeTz {
                    micros: MAX_CLOCK_MICROS,
                    offset,
                };
                assert_eq!(
                    TemporalValue::from_packed_time_tz(value.packed_time_tz()?)?,
                    value
                );
                assert!(
                    value.comparison_key()
                        > TemporalValue::TimeTz {
                            micros: MAX_CLOCK_MICROS - 1,
                            offset
                        }
                        .comparison_key()
                );
            }
            assert_eq!(
                c.query("SELECT make_time(23,59,60.49999999999999),make_time(0,0,-0.0000001)")?
                    .rows,
                vec![vec![
                    Value::Temporal(TemporalValue::Time(MAX_CLOCK_MICROS)),
                    Value::Temporal(TemporalValue::Time(0))
                ]]
            );
            assert_eq!(
                c.query("SELECT DATE '2000-01-01'+u FROM t WHERE id=3")?
                    .rows[0][0]
                    .to_string(),
                "2000-01-02 00:00:00.000001"
            );
            assert_eq!(
                c.query("SELECT count(*) FROM t a JOIN t b ON a.n=b.n")?
                    .rows[0][0],
                Value::Integer(7)
            );
            assert_eq!(
                c.query("SELECT count(DISTINCT n) FROM t")?.rows[0][0],
                Value::Integer(7)
            );
            assert_eq!(
                c.query("SELECT row_number() OVER (ORDER BY n) FROM t ORDER BY n")?
                    .rows[6],
                vec![Value::Integer(7)]
            );
            assert_eq!(c.query("SELECT TRY_CAST('24:00:00.000001' AS TIME_NS),TRY_CAST('24:00:00.000001' AS TIME),TRY_CAST('24:00:00.000001+00' AS TIMETZ)")?.rows,vec![vec![Value::Null;3]]);
            assert!(
                c.execute_prepared(
                    &insert,
                    &[
                        Value::Integer(100),
                        Value::Temporal(TemporalValue::TimeNs(MICROS_PER_DAY * 1000 + 999))
                    ]
                )
                .is_err()
            );
            c.execute(
                "BEGIN; UPDATE t SET z=TIMETZ '00:00:00+00'; DELETE FROM t WHERE id=0; ROLLBACK",
            )?;
            assert_eq!(c.query("SELECT * FROM t ORDER BY n")?.rows, before);
            drop(c);
            let mut c = open()?.connect();
            assert_eq!(c.query("SELECT * FROM t ORDER BY n")?.rows, before);
            c.execute("UPDATE t SET n=TIME_NS '01:00:00' WHERE id=0")?;
            c.checkpoint()?;
            let after = c.query("SELECT * FROM t ORDER BY n")?.rows;
            drop(c);
            assert_eq!(
                open()?.connect().query("SELECT * FROM t ORDER BY n")?.rows,
                after
            );
        }
    }
    Ok(())
}
