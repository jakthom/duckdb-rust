use std::sync::Arc;

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    common::{
        cast::{CastMode, CastRegistry},
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn enums_flow_through_selected_casts_functions_vectors_and_nested_children() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT TRY_CAST(1 AS ENUM('1')),TRY_CAST(true AS ENUM('true')),TRY_CAST('1'::BLOB AS ENUM('1'))")?.rows,vec![vec![Value::Null,Value::Null,Value::Null]]);
    assert_eq!(c.query("SELECT '12'::ENUM('12','-1')::INTEGER,'12.50'::ENUM('12.50')::DECIMAL(8,2),'2000-02-29'::ENUM('2000-02-29')::DATE::VARCHAR,'01:02:03'::ENUM('01:02:03')::TIME::VARCHAR,lower('UP'::ENUM('UP')),length('é'::ENUM('é')),hex('A'::ENUM('A'))")?.rows,
        vec![vec![Value::Integer(12),Value::Decimal{value:1250,width:8,scale:2},Value::Varchar("2000-02-29".into()),Value::Varchar("01:02:03".into()),Value::Varchar("up".into()),Value::Integer(1),Value::Varchar("41".into())]]);
    assert_eq!(c.query("SELECT enum_first(NULL::ENUM('z','a','')),enum_last(NULL::ENUM('z','a','')),enum_code('a'::ENUM('z','a','')),enum_code(NULL::ENUM('z','a','')),enum_range(NULL::ENUM('z','a'))::VARCHAR,enum_range_boundary(NULL,'a'::ENUM('z','a'))::VARCHAR,enum_range_boundary('a'::ENUM('z','a'),NULL)::VARCHAR")?.rows,
        vec![vec![Value::Varchar("z".into()),Value::Varchar("".into()),Value::Unsigned(1),Value::Null,Value::Varchar("[z, a]".into()),Value::Varchar("[z, a]".into()),Value::Varchar("[a]".into())]]);
    assert_eq!(c.query("SELECT list_extract(['a'::ENUM('z','a'),NULL]::VARCHAR[],1),struct_extract({'e':'a'::ENUM('z','a'),'n':1.25::DECIMAL(4,2)},'e')::VARCHAR,TRY_CAST('bad' AS ENUM('z','a')),TRY_CAST('bad'::ENUM('bad') AS INTEGER)")?.rows,
        vec![vec![Value::Varchar("a".into()),Value::Varchar("a".into()),Value::Null,Value::Null]]);
    for sql in [
        "SELECT enum_first('bad'::ENUM('z','a'))",
        "SELECT enum_code('a')",
        "SELECT enum_range_boundary(NULL,NULL)",
        "SELECT enum_range_boundary('a'::ENUM('a','b'),'a'::ENUM('b','a'))",
        "SELECT 'bad'::ENUM('bad')::INTEGER",
        "SELECT 1::INTEGER::ENUM('1')",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    let ty = DataType::enumeration(vec!["12".into(), "-1".into()])?;
    let query = QueryContext::background();
    let types = builtin_types();
    let cast =
        CastRegistry::builtins().bind(&ty, &DataType::Integer, CastMode::Explicit, &types)?;
    let values = vec![
        Value::enumeration(&ty, 0)?,
        Value::Null,
        Value::enumeration(&ty, 1)?,
    ];
    let flat = Vector::flat(ty.clone(), values.clone())?;
    let selected = Arc::new(flat.clone()).select(vec![2, 0, 1, 0])?;
    for vector in [
        flat.clone(),
        flat.slice(1, 2)?,
        selected,
        Vector::constant(ty.clone(), values[0].clone(), 4)?,
    ] {
        assert_eq!(
            cast.apply_batch(&vector, &query)?
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vector
                .values()
                .map(|value| cast.apply(value, &query))
                .collect::<Result<Vec<_>>>()?
        );
        let bound = types.bind(&ty)?;
        for a in vector.values() {
            for b in vector.values() {
                let mut ak = Vec::new();
                let mut bk = Vec::new();
                bound.append_key(a, &mut ak, &query)?;
                bound.append_key(b, &mut bk, &query)?;
                assert_eq!(ak == bk, a == b);
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn enum_dictionaries_survive_relational_mutations_rollback_and_native_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut composition = 0;
    let ty = DataType::enumeration(vec!["z".into(), "a".into(), "".into()])?;
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
                    let path = directory.path().join(format!("enum-{composition}.db"));
                    let open = || {
                        DatabaseBuilder::new()
                            .indexes(index.clone())
                            .expressions(expressions.clone())
                            .batch_size(3)
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
                        c.execute("CREATE TABLE t(k ENUM('z','a','') PRIMARY KEY DEFAULT 'z', u UUID, b BLOB, n DECIMAL(8,2)); INSERT INTO t DEFAULT VALUES")?;
                        let insert = c.prepare("INSERT INTO t VALUES ($1,$2,$3,$4)")?;
                        for ordinal in [1, 2] {
                            c.execute_prepared(
                                &insert,
                                &[
                                    Value::enumeration(&ty, ordinal)?,
                                    Value::Uuid(u128::from(ordinal)),
                                    Value::Blob(vec![0, 255, ordinal as u8]),
                                    Value::Varchar("12.50".into()),
                                ],
                            )?;
                        }
                        assert!(matches!(
                            c.execute("INSERT INTO t(k) VALUES ('a')"),
                            Err(Error::Constraint(_))
                        ));
                        assert_eq!(
                            c.query("SELECT k FROM t ORDER BY k")?.rows,
                            (0..3)
                                .map(|n| Ok(vec![Value::enumeration(&ty, n)?]))
                                .collect::<Result<Vec<_>>>()?
                        );
                        assert_eq!(
                            c.query(
                                "SELECT min(k)::VARCHAR,max(k)::VARCHAR,count(DISTINCT k) FROM t"
                            )?
                            .rows,
                            vec![vec![
                                Value::Varchar("z".into()),
                                Value::Varchar("".into()),
                                Value::Integer(3)
                            ]]
                        );
                        assert_eq!(
                            c.query("SELECT count(*) FROM t a JOIN t b ON a.k=b.k")?
                                .rows,
                            vec![vec![Value::Integer(3)]]
                        );
                        assert_eq!(c.query("SELECT count(*) FROM t a JOIN (SELECT k::ENUM('','a','z') AS k FROM t) b ON a.k=b.k")?.rows,vec![vec![Value::Integer(3)]]);
                        assert_eq!(
                            c.query("SELECT k::VARCHAR,count(*) FROM t GROUP BY k ORDER BY k")?
                                .rows,
                            vec![
                                vec![Value::Varchar("z".into()), Value::Integer(1)],
                                vec![Value::Varchar("a".into()), Value::Integer(1)],
                                vec![Value::Varchar("".into()), Value::Integer(1)]
                            ]
                        );
                        assert_eq!(c.query("SELECT first_value(k) OVER(ORDER BY k)::VARCHAR,lag(k) OVER(ORDER BY k)::VARCHAR FROM t ORDER BY k")?.rows,vec![vec![Value::Varchar("z".into()),Value::Null],vec![Value::Varchar("z".into()),Value::Varchar("z".into())],vec![Value::Varchar("z".into()),Value::Varchar("a".into())]]);
                        assert_eq!(c.query("SELECT count(*) FROM (SELECT k FROM t UNION SELECT 'a'::ENUM('','a','z')) q")?.rows,vec![vec![Value::Integer(3)]]);
                        c.execute(
                            "BEGIN; DELETE FROM t; ROLLBACK; BEGIN; UPDATE t SET k='bad'; ROLLBACK",
                        )
                        .expect_err("invalid label must fail");
                        c.execute("ROLLBACK")?;
                        assert_eq!(
                            c.query("SELECT count(*) FROM t")?.rows,
                            vec![vec![Value::Integer(3)]]
                        );
                        c.execute("BEGIN; UPDATE t SET n=99.00; DELETE FROM t WHERE k='a'::ENUM('z','a',''); ROLLBACK; DELETE FROM t WHERE k=''::ENUM('z','a',''); UPDATE t SET b='ok'::BLOB WHERE k='a'::ENUM('z','a','')")?;
                    }
                    let mut c = open()?.connect();
                    let result = c.query("SELECT k,b,n FROM t ORDER BY k")?;
                    assert_eq!(result.columns[0].data_type, ty);
                    assert_eq!(
                        result.rows,
                        vec![
                            vec![Value::enumeration(&ty, 0)?, Value::Null, Value::Null],
                            vec![
                                Value::enumeration(&ty, 1)?,
                                Value::Blob(b"ok".to_vec()),
                                Value::Decimal {
                                    value: 1250,
                                    width: 8,
                                    scale: 2
                                }
                            ]
                        ]
                    );
                    assert!(matches!(
                        c.execute("INSERT INTO t(k) VALUES ('a')"),
                        Err(Error::Constraint(_))
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn enum_physical_widths_defaults_and_nulls_survive_wal_replay_and_checkpoint() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for count in [3, 255, 256, 65536] {
        let path = directory.path().join(format!("enum-wal-{count}.duckdb"));
        let ty = DataType::enumeration((0..count).map(|n| format!("label{n}")).collect())?;
        {
            let mut c = Database::open_logged(&path)?.connect();
            c.execute(&format!("CREATE TABLE t(k {ty} PRIMARY KEY DEFAULT 'label{}', v {ty}); INSERT INTO t DEFAULT VALUES",count-1))?;
            c.execute_params(
                "INSERT INTO t VALUES ($1,$2)",
                &[
                    Value::enumeration(&ty, 0)?,
                    Value::enumeration(&ty, count - 1)?,
                ],
            )?;
            c.execute("BEGIN; DELETE FROM t; ROLLBACK")?;
        }
        for checkpoint in [false, true] {
            let mut c = Database::open_logged(&path)?.connect();
            assert_eq!(
                c.query("SELECT enum_code(k),enum_code(v) FROM t ORDER BY k")?
                    .rows,
                vec![
                    vec![Value::Unsigned(0), Value::Unsigned(u128::from(count - 1))],
                    vec![Value::Unsigned(u128::from(count - 1)), Value::Null]
                ]
            );
            if checkpoint {
                c.execute("CHECKPOINT")?;
            }
        }
        let mut c = Database::open_read_only(&path)?.connect();
        let result = c.query("SELECT k FROM t ORDER BY k")?;
        assert_eq!(result.columns[0].data_type, ty);
        assert_eq!(
            result.rows,
            vec![
                vec![Value::enumeration(&ty, 0)?],
                vec![Value::enumeration(&ty, count - 1)?]
            ]
        );
    }
    Ok(())
}
