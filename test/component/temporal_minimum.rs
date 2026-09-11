use super::*;
use duckdb_rust::{
    Error,
    common::cast::{CastMode, CastRegistry},
    optimizer::IdentityOptimizer,
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn timestamp_types() -> [DataType; 6] {
    [
        DataType::Timestamp,
        DataType::TimestampS,
        DataType::TimestampMs,
        DataType::TimestampNs,
        DataType::TimestampTz,
        DataType::TimestampTzNs,
    ]
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn every_timestamp_physical_minimum_is_finite_distinct_from_null_and_not_necessarily_renderable()
-> Result<()> {
    let query = QueryContext::background();
    for kind in timestamp_types() {
        let raw = TemporalValue::from_ticks(&kind, i64::MIN)?;
        let value = Value::Temporal(raw);
        raw.validate()?;
        assert!(raw.is_finite());
        assert!(value.fits_type(&kind));
        assert!(raw.check_text_renderable().is_err());
        assert!(!raw.to_string().is_empty());
        let bound = query.types().bind(&kind)?;
        bound.validate(&value, &query)?;
        assert!(
            bound
                .compare(
                    &value,
                    &Value::Temporal(TemporalValue::from_ticks(&kind, -i64::MAX)?),
                    &query
                )?
                .is_lt()
        );
        let mut bytes = Vec::new();
        raw.append_storage(&mut bytes)?;
        assert_eq!(bytes, i64::MIN.to_le_bytes());
        assert_eq!(raw.scale_timestamp(&kind)?, raw);
    }
    for (source, target) in [
        (DataType::Timestamp, DataType::TimestampTz),
        (DataType::TimestampTz, DataType::Timestamp),
        (DataType::TimestampTzNs, DataType::TimestampNs),
    ] {
        let cast =
            CastRegistry::builtins().bind(&source, &target, CastMode::Explicit, query.types())?;
        assert_eq!(
            cast.apply(
                &Value::Temporal(TemporalValue::from_ticks(&source, i64::MIN)?),
                &query
            )?,
            Value::Temporal(TemporalValue::from_ticks(&target, i64::MIN)?)
        );
    }
    assert!(
        CastRegistry::builtins()
            .bind(
                &DataType::TimestampNs,
                &DataType::TimestampTzNs,
                CastMode::Explicit,
                query.types(),
            )
            .is_err()
    );
    assert!(
        TemporalValue::Timestamp(i64::MIN)
            .scale_timestamp(&DataType::TimestampNs)
            .is_err()
    );
    assert!(
        TemporalValue::TimestampS(i64::MIN)
            .scale_timestamp(&DataType::Timestamp)
            .is_err()
    );
    assert_eq!(
        TemporalValue::TimestampNs(i64::MIN).scale_timestamp(&DataType::Timestamp)?,
        TemporalValue::Timestamp(-9_223_372_036_854_776)
    );
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT epoch_us(make_timestamp('-9223372036854775808'::BIGINT)),epoch_ns(make_timestamp_ns('-9223372036854775808'::BIGINT)),epoch_us(time_bucket(INTERVAL '4us',make_timestamp(-9223372036854775806)))")?.rows,vec![vec![Value::Integer(i128::from(i64::MIN));3]]);
    assert_eq!(
        c.query("SELECT make_timestamp_ns('-9223372036854775808'::BIGINT)")?
            .rows,
        vec![vec![Value::Temporal(TemporalValue::TimestampNs(i64::MIN))]]
    );
    assert!(matches!(
        c.query("SELECT CAST(make_timestamp_ns('-9223372036854775808'::BIGINT) AS VARCHAR)"),
        Err(Error::Internal(_))
    ));
    assert_eq!(c.query("SELECT isfinite(t),isinf(t),t<TIMESTAMP '-infinity',t::DATE,t::TIME,epoch_us((t::VARIANT)::TIMESTAMP) FROM (SELECT time_bucket(INTERVAL '4us',make_timestamp(-9223372036854775806)) t)")?.rows,vec![vec![Value::Boolean(true),Value::Boolean(false),Value::Boolean(true),Value::Date("290309-12-21 (BC)".parse()?),Value::Temporal(TemporalValue::Time(71_945_224_192)),Value::Integer(i128::from(i64::MIN))]]);
    assert!(matches!(c.query("SELECT time_bucket(INTERVAL '4us',make_timestamp(-9223372036854775806))+INTERVAL '1 day'"),Err(Error::Conversion(_))));
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn timestamp_minima_cross_parameters_nested_variants_indexes_and_native_wal_validity() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        for hashed in [false, true] {
            let path = directory
                .path()
                .join(format!("minimum-{batched}-{hashed}.duckdb"));
            let open = || {
                let checkpoint = FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    Arc::new(DuckDbFormat::default().with_storage_version(69)?),
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
            for (index, kind) in timestamp_types().into_iter().enumerate() {
                c.execute(&format!("CREATE TABLE minima_{index}(id INTEGER UNIQUE,t {kind} UNIQUE,p STRUCT(t {kind}),a {kind}[],v VARIANT)"))?;
                let insert = c.prepare(&format!(
                    "INSERT INTO minima_{index} VALUES($1,$2,{{'t':$2}},[$2,NULL::{}],$2::VARIANT)",
                    kind
                ))?;
                for (id, ticks) in [
                    (0, Some(i64::MIN)),
                    (1, None),
                    (2, Some(-i64::MAX)),
                    (3, Some(i64::MIN + 2)),
                ] {
                    let value = ticks
                        .map(|ticks| TemporalValue::from_ticks(&kind, ticks).map(Value::Temporal))
                        .transpose()?
                        .unwrap_or(Value::Null);
                    c.execute_prepared(&insert, &[Value::Integer(id), value])?;
                }
                let query =
                    format!("SELECT id,t,p.t,a[1],v::{kind} FROM minima_{index} ORDER BY id");
                let before = c.query(&query)?.rows;
                assert_eq!(
                    before[0],
                    vec![
                        Value::Integer(0),
                        Value::Temporal(TemporalValue::from_ticks(&kind, i64::MIN)?),
                        Value::Temporal(TemporalValue::from_ticks(&kind, i64::MIN)?),
                        Value::Temporal(TemporalValue::from_ticks(&kind, i64::MIN)?),
                        Value::Temporal(TemporalValue::from_ticks(&kind, i64::MIN)?)
                    ]
                );
                assert_eq!(
                    before[1],
                    vec![
                        Value::Integer(1),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null
                    ]
                );
                assert_eq!(
                    c.query(&format!(
                        "SELECT count(*) FROM minima_{index} a JOIN minima_{index} b ON a.t=b.t"
                    ))?
                    .rows,
                    vec![vec![Value::Integer(3)]]
                );
                assert_eq!(
                    c.query(&format!(
                        "SELECT count(*),count(t),count(DISTINCT t) FROM minima_{index}"
                    ))?
                    .rows,
                    vec![vec![
                        Value::Integer(4),
                        Value::Integer(3),
                        Value::Integer(3)
                    ]]
                );
                assert_eq!(
                    c.query(&format!(
                        "SELECT min(t) OVER(ORDER BY id) FROM minima_{index} ORDER BY id"
                    ))?
                    .rows,
                    vec![vec![Value::Temporal(TemporalValue::from_ticks(&kind, i64::MIN)?)]; 4]
                );
                let lookup = c.prepare(&format!("SELECT id FROM minima_{index} WHERE t=$1"))?;
                assert_eq!(
                    c.execute_prepared(
                        &lookup,
                        &[Value::Temporal(TemporalValue::from_ticks(&kind, i64::MIN)?)]
                    )?
                    .rows,
                    vec![vec![Value::Integer(0)]]
                );
                assert!(
                    c.execute_prepared(
                        &insert,
                        &[
                            Value::Integer(4),
                            Value::Temporal(TemporalValue::from_ticks(&kind, i64::MIN)?)
                        ]
                    )
                    .is_err()
                );
                c.execute(&format!("BEGIN; DELETE FROM minima_{index} WHERE id=0; UPDATE minima_{index} SET t=NULL WHERE id=3; ROLLBACK"))?;
                assert_eq!(c.query(&query)?.rows, before);
            }
            drop(c);
            let mut c = open()?.connect();
            for (index, kind) in timestamp_types().into_iter().enumerate() {
                let row = c
                    .query(&format!(
                        "SELECT t,p.t,a[1],v::{kind} FROM minima_{index} WHERE id=0"
                    ))?
                    .rows;
                assert_eq!(
                    row,
                    vec![vec![
                        Value::Temporal(TemporalValue::from_ticks(
                            &kind,
                            i64::MIN
                        )?);
                        4
                    ]]
                );
                assert_eq!(
                    c.query(&format!("SELECT count(t) FROM minima_{index}"))?
                        .rows,
                    vec![vec![Value::Integer(3)]]
                );
            }
            c.checkpoint()?;
            drop(c);
            let mut c = Database::open_read_only(&path)?.connect();
            for (index, kind) in timestamp_types().into_iter().enumerate() {
                assert_eq!(
                    c.query(&format!(
                        "SELECT t,p.t,a[1],v::{kind} FROM minima_{index} WHERE id=0"
                    ))?
                    .rows,
                    vec![vec![
                        Value::Temporal(TemporalValue::from_ticks(
                            &kind,
                            i64::MIN
                        )?);
                        4
                    ]]
                );
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn minimum_fixture(name: &str, destination: &std::path::Path) -> Result<()> {
    use std::{fs, io::Read, path::Path};
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test/data/duckdb")
        .join(format!("temporal-minimum-{name}.gz"));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(source)?).read_to_end(&mut bytes)?;
    fs::write(destination, bytes)?;
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn independent_cpp_timestamp_minima_keep_physical_validity_through_recovery_and_publication()
-> Result<()> {
    // These were independently made by pinned C++ Value/Appender APIs. The
    // unrenderable S/MS payloads cannot be reconstructed from a text fixture.
    for target in ["release", "development"] {
        let kinds: Vec<_> = timestamp_types()
            .into_iter()
            .filter(|kind| target == "development" || *kind != DataType::TimestampTzNs)
            .collect();
        for wal in [false, true] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("minima.duckdb");
            let prefix = format!("{target}-minima{}", if wal { "-wal" } else { "" });
            minimum_fixture(&format!("{prefix}.duckdb"), &path)?;
            if wal {
                minimum_fixture(
                    &format!("{prefix}.duckdb.wal"),
                    &path.with_extension("duckdb.wal"),
                )?;
            }
            let expected: Vec<_> = [
                Some(i64::MIN),
                Some(-i64::MAX),
                Some(i64::MIN + 2),
                Some(0),
                None,
                Some(i64::MIN),
            ]
            .into_iter()
            .enumerate()
            .map(|(id, ticks)| {
                let mut row = vec![Value::Integer(id as i128)];
                for kind in &kinds {
                    row.push(
                        ticks
                            .map(|ticks| {
                                TemporalValue::from_ticks(kind, ticks).map(Value::Temporal)
                            })
                            .transpose()?
                            .unwrap_or(Value::Null),
                    );
                }
                Ok(row)
            })
            .collect::<Result<_>>()?;
            let sql = "SELECT * FROM minima ORDER BY id";
            let readonly = Database::open_read_only(&path)?;
            assert_eq!(readonly.connect().query(sql)?.rows, expected, "{prefix}");
            drop(readonly);
            let open = || {
                let checkpoint = FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    Arc::new(DuckDbFormat::default()),
                )?
                .with_recovery(Arc::new(DuckDbWalRecovery))?;
                DatabaseBuilder::new()
                    .durability(Arc::new(
                        FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?
                            .with_checkpoint_policy(None),
                    ))
                    .build()
            };
            let mut c = open()?.connect();
            assert_eq!(c.query(sql)?.rows, expected, "{prefix} writable");
            c.execute("BEGIN; DELETE FROM minima WHERE id=0; UPDATE minima SET u=NULL,s=NULL,ms=NULL,n=NULL,z=NULL WHERE id=5; ROLLBACK")?;
            assert_eq!(c.query(sql)?.rows, expected, "{prefix} rollback");
            let names = ["u", "s", "ms", "n", "z", "zn"];
            for (column, kind) in names.into_iter().zip(&kinds) {
                let prepared = c.prepare(&format!("UPDATE minima SET {column}=$1 WHERE id=3"))?;
                c.execute_prepared(
                    &prepared,
                    &[Value::Temporal(TemporalValue::from_ticks(kind, i64::MIN)?)],
                )?;
            }
            let mut changed = expected;
            changed[3] = std::iter::once(Value::Integer(3))
                .chain(changed[0].iter().skip(1).cloned())
                .collect();
            assert_eq!(c.query(sql)?.rows, changed);
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?.connect().query(sql)?.rows,
                changed,
                "{prefix} WAL replay"
            );
            let mut c = open()?.connect();
            c.checkpoint()?;
            drop(c);
            assert_eq!(
                Database::open_read_only(&path)?.connect().query(sql)?.rows,
                changed,
                "{prefix} checkpoint"
            );
        }
    }
    Ok(())
}
