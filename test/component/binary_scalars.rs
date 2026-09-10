use std::sync::Arc;

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    common::{
        cast::{CastMode, CastRegistry},
        scalar::{parse_blob, parse_uuid},
        type_registry::builtin_types,
        vector::Vector,
    },
    execution::{
        expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        operator::join::{HashJoin, JoinAlgorithm, NestedLoopJoin},
        physical_plan::NativePhysicalPlanner,
    },
    parallel::QueryContext,
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

#[derive(Debug)]
struct ConcatIntegerCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::common::cast::CastFunction for ConcatIntegerCast {
    fn name(&self) -> &'static str {
        "concat-selected-integer-text"
    }
    fn supports(&self, spec: &duckdb_rust::common::cast::CastSpec) -> bool {
        spec.source == DataType::Integer
            && spec.target == DataType::Varchar
            && spec.mode == CastMode::Explicit
    }
    fn cast(
        &self,
        value: &Value,
        _: &duckdb_rust::common::cast::CastSpec,
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        match value {
            Value::Integer(9) => Err(Error::Internal("selected concat failure".into())),
            Value::Integer(value) => Ok(Value::Varchar(format!("selected:{value}"))),
            _ => Err(Error::Internal("selected concat cast input".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn concat_keeps_selected_text_casts_nulls_failures_and_atomic_mutations() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(duckdb_rust::optimizer::IdentityOptimizer)
                as Arc<dyn duckdb_rust::optimizer::Optimizer>,
            Arc::new(duckdb_rust::optimizer::PipelineOptimizer::default()),
        ] {
            let mut normal = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer.clone())
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(normal.query("SELECT concat(NULL),concat(1,NULL,DATE '2001-01-01'),concat('a'::BLOB,'b'::BLOB),concat({'a':1},2)")?.rows,
                vec![vec![Value::Varchar("".into()),Value::Varchar("12001-01-01".into()),Value::Varchar("ab".into()),Value::Varchar("{'a': 1}2".into())]]);
            assert!(matches!(
                normal.query("SELECT concat()"),
                Err(Error::Bind(_))
            ));
            assert_eq!(
                normal.query("SELECT concat([1])::VARCHAR")?.rows,
                vec![vec![Value::Varchar("[1]".into())]]
            );
            for sql in [
                "SELECT concat(make_timestamp(-9223372036854775806))",
                "SELECT TRY_CAST(concat(make_timestamp(-9223372036854775806)) AS VARCHAR)",
                "SELECT concat({'a':make_timestamp(-9223372036854775806)})",
            ] {
                assert!(
                    matches!(normal.query(sql), Err(Error::Internal(_))),
                    "{sql}"
                );
            }
            assert!(matches!(
                normal.execute_params(
                    "SELECT concat($1)",
                    &[Value::Temporal(
                        duckdb_rust::common::TemporalValue::Timestamp(-i64::MAX + 1)
                    )]
                ),
                Err(Error::Internal(_))
            ));
            let mut casts = CastRegistry::builtins();
            casts.replace(
                duckdb_rust::common::cast::CastSpec {
                    source: DataType::Integer,
                    target: DataType::Varchar,
                    mode: CastMode::Explicit,
                },
                Arc::new(ConcatIntegerCast),
            )?;
            let mut c = DatabaseBuilder::new()
                .casts(casts)
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(
                c.query("SELECT concat(1::INTEGER,NULL::INTEGER,'!')")?.rows,
                vec![vec![Value::Varchar("selected:1!".into())]]
            );
            assert_eq!(
                c.execute_params("SELECT concat($1,'!')", &[Value::Integer(2)])?[0].rows,
                vec![vec![Value::Varchar("selected:2!".into())]]
            );
            c.execute("CREATE TABLE strings(k INTEGER PRIMARY KEY,s VARCHAR); INSERT INTO strings VALUES (1,'old'),(2,NULL),(9,'last')")?;
            assert!(matches!(
                c.execute("UPDATE strings SET s=concat(k,'!')"),
                Err(Error::Internal(_))
            ));
            assert_eq!(
                c.query("SELECT s FROM strings ORDER BY k")?.rows,
                vec![
                    vec![Value::Varchar("old".into())],
                    vec![Value::Null],
                    vec![Value::Varchar("last".into())]
                ]
            );
            c.execute("BEGIN; UPDATE strings SET s=concat(k,'!') WHERE k<9; ROLLBACK")?;
            assert_eq!(
                c.query("SELECT s FROM strings WHERE k=1")?.rows,
                vec![vec![Value::Varchar("old".into())]]
            );
            assert!(matches!(
                c.query("SELECT TRY_CAST(concat(9::INTEGER) AS VARCHAR)"),
                Err(Error::Internal(_))
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn binary_scalar_conversion_vectors_preserve_bytes_and_uuid_bits() -> Result<()> {
    let query = QueryContext::background();
    let types = builtin_types();
    let casts = CastRegistry::builtins();
    let bytes: Vec<u8> = (0..=255).collect();
    let blob = Value::Blob(bytes.clone());
    assert_eq!(parse_blob(&blob.to_string(), || Ok(()))?, bytes);
    for text in ["\\", "\\x0", "\\xGG", "\\X00", "\\n", "é"] {
        assert!(parse_blob(text, || Ok(())).is_err(), "{text}");
    }
    for value in [0, 1, 1 << 127, (1 << 127) - 1, u128::MAX] {
        let uuid = Value::Uuid(value);
        assert_eq!(parse_uuid(&uuid.to_string(), || Ok(()))?, value);
        assert_eq!(
            parse_uuid(
                &format!("{{{}}}", uuid.to_string().replace('-', "")),
                || Ok(())
            )?,
            value
        );
        assert_eq!(
            uuid.cast(&DataType::Blob)?,
            Value::Blob(value.to_be_bytes().to_vec())
        );
        assert_eq!(uuid.cast(&DataType::UHugeInt)?, Value::Unsigned(value));
        assert_eq!(
            casts
                .bind(
                    &DataType::UHugeInt,
                    &DataType::Uuid,
                    CastMode::Explicit,
                    &types
                )?
                .apply(&Value::Unsigned(value), &query)?,
            uuid
        );
        assert_eq!(uuid.cast(&DataType::Blob)?.cast(&DataType::Uuid)?, uuid);
    }
    for text in [
        "",
        "1",
        "{00112233445566778899aabbccddeeff",
        " 00112233445566778899aabbccddeeff",
        "00112233445566778899aabbccddeeffg",
    ] {
        assert!(parse_uuid(text, || Ok(())).is_err(), "{text}");
    }
    for data_type in [DataType::Blob, DataType::Uuid] {
        assert!(
            casts
                .bind(&DataType::Varchar, &data_type, CastMode::Implicit, &types)
                .is_err()
        );
        let values = if data_type == DataType::Blob {
            vec![
                blob.clone(),
                Value::Null,
                Value::Blob(vec![]),
                Value::Blob(vec![0, 1, 255]),
                blob.clone(),
            ]
        } else {
            vec![
                Value::Uuid(u128::MAX),
                Value::Null,
                Value::Uuid(0),
                Value::Uuid(1 << 127),
                Value::Uuid(u128::MAX),
            ]
        };
        let flat = Vector::flat(data_type.clone(), values.clone())?;
        let selected = Arc::new(flat.clone()).select(vec![4, 0, 2, 1, 3, 0])?;
        for column in [
            flat.clone(),
            flat.slice(1, 3)?,
            selected.clone(),
            selected.slice(1, 4)?,
            Vector::constant(data_type.clone(), values[0].clone(), 3)?,
        ] {
            let cast = casts.bind(&data_type, &DataType::Varchar, CastMode::Explicit, &types)?;
            let result = cast.apply_batch(&column, &query)?;
            let expected = column
                .values()
                .map(|value| cast.apply(value, &query))
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(result.values().cloned().collect::<Vec<_>>(), expected);
            let bound = types.bind(&data_type)?;
            for a in column.values() {
                for b in column.values() {
                    let mut ak = vec![];
                    let mut bk = vec![];
                    bound.append_key(a, &mut ak, &query)?;
                    bound.append_key(b, &mut bk, &query)?;
                    assert_eq!(ak == bk, a == b);
                    if !a.is_null() && !b.is_null() {
                        assert_eq!(bound.compare(a, b, &query)?, a.compare(b)?);
                    }
                }
            }
        }
    }
    assert!(matches!(
        parse_blob("a", || Err(Error::Interrupted)),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn binary_scalar_sql_functions_and_negative_cases_match_reference() -> Result<()> {
    let mut c = Database::memory()?.connect();
    let result = c.query(r"SELECT '\x00\xFF'::BLOB, '{00112233445566778899AABBCCDDEEFF}'::UUID, typeof('a'::BINARY),typeof('a'::VARBINARY),typeof('a'::BYTEA),typeof('00000000000000000000000000000000'::GUID)")?;
    assert_eq!(result.columns[0].data_type, DataType::Blob);
    assert_eq!(result.columns[1].data_type, DataType::Uuid);
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Blob(vec![0, 255]),
            Value::Uuid(0x00112233445566778899aabbccddeeff),
            Value::Varchar("BLOB".into()),
            Value::Varchar("BLOB".into()),
            Value::Varchar("BLOB".into()),
            Value::Varchar("UUID".into())
        ]]
    );
    assert_eq!(c.query(r"SELECT hex('é'),hex('\x00\xFF'::BLOB),unhex('F')::VARCHAR,encode('é')::VARCHAR,decode('hello'::BLOB),octet_length('a\x00'::BLOB),hex(-1::HUGEINT),hex(-1::BIGINT),hex('a'::BLOB||'\x00'::BLOB)")?.rows,
        vec![vec!["C3A9","00FF",r"\x0F",r"\xC3\xA9","hello","2","FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF","FFFFFFFFFFFFFFFF","6100"].into_iter().enumerate().map(|(i,v)| if i==5 {Value::Integer(2)} else {Value::Varchar(v.into())}).collect::<Vec<_>>()]);
    for sql in [
        r"SELECT '\xGG'::BLOB",
        "SELECT 'é'::BLOB",
        "SELECT 'oops'::UUID",
        "SELECT 'abc'::BLOB::UUID",
        r"SELECT decode('\xFF'::BLOB)",
        "SELECT unhex('not hex')",
        "SELECT 1::INTEGER::UUID",
        "SELECT length('a'::BLOB)",
        "SELECT 'a'::BLOB(3)",
        "SELECT 'a'::BINARY(3)",
        "SELECT 'a'::VARBINARY(3)",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    assert_eq!(c.query(r"SELECT TRY_CAST('\xGG' AS BLOB),TRY_CAST('bad' AS UUID),CASE WHEN false THEN '\xGG'::BLOB ELSE 'ok'::BLOB END, NULL::BLOB||'x'::BLOB, encode(NULL)")?.rows,
        vec![vec![Value::Null,Value::Null,Value::Blob(b"ok".to_vec()),Value::Null,Value::Null]]);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn binary_scalars_survive_relational_operations_parameters_mutations_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut composition = 0;
    for format in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ] {
        for index in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            for expressions in [
                Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                Arc::new(BatchedEvaluator),
            ] {
                for join in [
                    Arc::new(HashJoin) as Arc<dyn JoinAlgorithm>,
                    Arc::new(NestedLoopJoin),
                ] {
                    composition += 1;
                    let path = directory.path().join(format!("scalar-{composition}.db"));
                    let open = || {
                        DatabaseBuilder::new()
                            .indexes(index.clone())
                            .expressions(expressions.clone())
                            .batch_size(if composition % 2 == 0 { 3 } else { 2048 })
                            .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![
                                join.clone(),
                            ])))
                            .durability(Arc::new(FileCheckpoint::open(
                                &path,
                                OpenMode::ReadWrite,
                                format.clone(),
                            )?))
                            .build()
                    };
                    {
                        let mut c = open()?.connect();
                        c.execute(r"CREATE TABLE t(k UUID PRIMARY KEY DEFAULT '00000000-0000-0000-0000-000000000000', b BLOB UNIQUE DEFAULT '\x00\xFF', n DECIMAL(5,2) DEFAULT 1.25); INSERT INTO t DEFAULT VALUES")?;
                        let insert = c.prepare("INSERT INTO t VALUES ($1, $2, $3)")?;
                        c.execute_prepared(
                            &insert,
                            &[
                                Value::Uuid(u128::MAX),
                                Value::Blob(vec![0, 1, 255]),
                                Value::Varchar("2.50".into()),
                            ],
                        )?;
                        c.execute_prepared(
                            &insert,
                            &[
                                Value::Uuid(1 << 127),
                                Value::Blob(vec![]),
                                Value::Varchar("3.75".into()),
                            ],
                        )?;
                        let lookup = c.prepare("SELECT k,b,n FROM t WHERE k=$1")?;
                        for key in [0, u128::MAX, 1 << 127] {
                            assert_eq!(
                                c.execute_prepared(&lookup, &[Value::Uuid(key)])?.rows[0][0],
                                Value::Uuid(key)
                            );
                        }
                        assert!(matches!(
                            c.execute_prepared(
                                &insert,
                                &[Value::Uuid(1), Value::Blob(vec![0, 255]), Value::Integer(1)]
                            ),
                            Err(Error::Constraint(_))
                        ));
                        c.execute("CREATE TABLE s AS SELECT b FROM t UNION ALL SELECT b FROM t UNION ALL SELECT NULL::BLOB")?;
                        assert_eq!(
                            c.query("SELECT count(*) FROM t JOIN s ON t.b=s.b")?.rows,
                            vec![vec![Value::Integer(6)]]
                        );
                        assert_eq!(
                            c.query("SELECT count(DISTINCT b), min(k), max(k) FROM t")?
                                .rows,
                            vec![vec![
                                Value::Integer(3),
                                Value::Uuid(0),
                                Value::Uuid(u128::MAX)
                            ]]
                        );
                        assert_eq!(
                            c.query(
                                "SELECT count(*) FROM (SELECT b FROM s UNION SELECT b FROM t) q"
                            )?
                            .rows,
                            vec![vec![Value::Integer(4)]]
                        );
                        assert_eq!(
                            c.query(
                                "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM s WHERE t.b=s.b)"
                            )?
                            .rows,
                            vec![vec![Value::Integer(3)]]
                        );
                        assert_eq!(
                            c.query("SELECT count(*) FROM s GROUP BY b ORDER BY b")?
                                .rows,
                            vec![
                                vec![Value::Integer(2)],
                                vec![Value::Integer(2)],
                                vec![Value::Integer(2)],
                                vec![Value::Integer(1)]
                            ]
                        );
                        assert_eq!(c.query("SELECT first_value(k) OVER(ORDER BY k),lag(b) OVER(ORDER BY k),count(*) OVER(PARTITION BY b) FROM t ORDER BY k")?.rows,
                            vec![vec![Value::Uuid(0),Value::Null,Value::Integer(1)],vec![Value::Uuid(0),Value::Blob(vec![0,255]),Value::Integer(1)],vec![Value::Uuid(0),Value::Blob(vec![]),Value::Integer(1)]]);
                        c.execute("BEGIN; DELETE FROM t; ROLLBACK; BEGIN; UPDATE t SET b='changed'::BLOB; ROLLBACK")
                            .or_else(|error| { c.execute("ROLLBACK")?; if matches!(error,Error::Constraint(_)) {Ok(vec![])} else {Err(error)} })?;
                        c.execute("UPDATE t SET b='updated'::BLOB WHERE k='00000000-0000-0000-0000-000000000000'::UUID; DELETE FROM t WHERE k='80000000-0000-0000-0000-000000000000'::UUID")?;
                    }
                    let mut c = open()?.connect();
                    assert_eq!(
                        c.query("SELECT k,b FROM t ORDER BY k")?.rows,
                        vec![
                            vec![Value::Uuid(0), Value::Blob(b"updated".to_vec())],
                            vec![Value::Uuid(u128::MAX), Value::Blob(vec![0, 1, 255])]
                        ]
                    );
                    assert_eq!(
                        c.query("SELECT typeof(k),typeof(b),typeof(n) FROM t LIMIT 1")?
                            .rows,
                        vec![vec![
                            Value::Varchar("UUID".into()),
                            Value::Varchar("BLOB".into()),
                            Value::Varchar("DECIMAL(5,2)".into())
                        ]]
                    );
                    c.execute("INSERT INTO t DEFAULT VALUES")
                        .expect_err("reopened UUID primary key remains enforced");
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn binary_scalar_wal_replay_and_overflow_preserve_committed_values() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("scalar-wal.duckdb");
    let bytes: Vec<u8> = (0..300000).map(|index| (index % 256) as u8).collect();
    {
        let db = Database::open_logged(&path)?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t(k UUID PRIMARY KEY, b BLOB, n DECIMAL(38,3))")?;
        c.execute_params(
            "INSERT INTO t VALUES ($1,$2,1.125)",
            &[Value::Uuid(0), Value::Blob(bytes.clone())],
        )?;
        c.execute_params(
            "INSERT INTO t VALUES ($1,$2,2.250)",
            &[Value::Uuid(u128::MAX), Value::Blob(vec![0, 255, 0])],
        )?;
        let mut reader = db.connect();
        reader.execute("BEGIN")?;
        assert_eq!(
            reader.query("SELECT count(*) FROM t")?.rows,
            vec![vec![Value::Integer(2)]]
        );
        c.execute("BEGIN; DELETE FROM t; ROLLBACK")?;
        c.execute("BEGIN; UPDATE t SET b='discarded'::BLOB; ROLLBACK")?;
        c.execute("INSERT INTO t VALUES ('80000000000000000000000000000000'::UUID,NULL,NULL)")?;
        assert_eq!(
            reader.query("SELECT count(*) FROM t")?.rows,
            vec![vec![Value::Integer(2)]]
        );
        reader.execute("COMMIT")?;
        assert_eq!(
            reader.query("SELECT count(*) FROM t")?.rows,
            vec![vec![Value::Integer(3)]]
        );
    }
    assert!(std::fs::metadata(path.with_extension("duckdb.wal"))?.len() > 0);
    for checkpoint in [false, true] {
        let mut c = Database::open_logged(&path)?.connect();
        assert_eq!(
            c.query("SELECT k,b FROM t ORDER BY k")?.rows,
            vec![
                vec![Value::Uuid(0), Value::Blob(bytes.clone())],
                vec![Value::Uuid(1 << 127), Value::Null],
                vec![Value::Uuid(u128::MAX), Value::Blob(vec![0, 255, 0])]
            ]
        );
        if checkpoint {
            c.execute("CHECKPOINT")?;
        }
    }
    let mut c = Database::open_read_only(&path)?.connect();
    assert_eq!(
        c.query("SELECT b FROM t WHERE k='00000000000000000000000000000000'::UUID")?
            .rows,
        vec![vec![Value::Blob(bytes)]]
    );
    Ok(())
}
