use std::sync::Arc;

#[path = "enumeration/named.rs"]
mod named;

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
fn enum_range_boundary_references_the_first_physical_batch_row() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let db = DatabaseBuilder::new()
            .expressions(expressions.clone())
            .batch_size(2)
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t(k ENUM('z','a')); INSERT INTO t VALUES ('z'),('a'),('a'),('z')")?;

        // The pinned function reads vector row zero. Each two-row physical
        // batch therefore references one range: [z,a], [z,a], [a], [a].
        let expected = [
            ["[z, a]", "[z]"],
            ["[z, a]", "[z]"],
            ["[a]", "[z, a]"],
            ["[a]", "[z, a]"],
        ];
        let sql =
            "SELECT enum_range_boundary(k,NULL),enum_range_boundary(NULL::ENUM('z','a'),k) FROM t";
        let ranges = |rows: Vec<Vec<Value>>| {
            rows.into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|value| value.to_string())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(ranges(c.query(sql)?.rows.into_iter().collect()), expected);

        let nested_sql = "SELECT enum_range_boundary(k,NULL)::VARCHAR,length(enum_range_boundary(k,NULL)::VARCHAR),enum_range_boundary(k,NULL)::VARCHAR='[z, a]',CASE WHEN k='z' THEN enum_range_boundary(k,NULL)::VARCHAR ELSE 'skip' END FROM t";
        let nested = vec![
            vec![
                Value::Varchar("[z, a]".into()),
                Value::Integer(6),
                Value::Boolean(true),
                Value::Varchar("[z, a]".into()),
            ],
            vec![
                Value::Varchar("[z, a]".into()),
                Value::Integer(6),
                Value::Boolean(true),
                Value::Varchar("skip".into()),
            ],
            vec![
                Value::Varchar("[a]".into()),
                Value::Integer(3),
                Value::Boolean(false),
                Value::Varchar("skip".into()),
            ],
            vec![
                Value::Varchar("[a]".into()),
                Value::Integer(3),
                Value::Boolean(false),
                // CASE evaluates its value over the selected z rows, so the
                // branch's physical row zero is z rather than batch row zero a.
                Value::Varchar("[z, a]".into()),
            ],
        ];
        assert_eq!(c.query(nested_sql)?.rows, nested);
        assert_eq!(
            c.execute_prepared(&c.prepare(nested_sql)?, &[])?.rows,
            nested
        );
        assert_eq!(
            c.query("SELECT enum_range_boundary(k,NULL)::VARCHAR,CASE WHEN false THEN CAST('bad' AS INTEGER)::VARCHAR ELSE k::VARCHAR END FROM t")?.rows,
            vec![
                vec![Value::Varchar("[z, a]".into()), Value::Varchar("z".into())],
                vec![Value::Varchar("[z, a]".into()), Value::Varchar("a".into())],
                vec![Value::Varchar("[a]".into()), Value::Varchar("a".into())],
                vec![Value::Varchar("[a]".into()), Value::Varchar("z".into())],
            ]
        );
        assert_eq!(
            c.query("SELECT CASE WHEN true THEN enum_range_boundary(k,NULL)::VARCHAR ELSE CAST('bad' AS INTEGER)::VARCHAR END FROM t")?.rows,
            vec![
                vec![Value::Varchar("[z, a]".into())],
                vec![Value::Varchar("[z, a]".into())],
                vec![Value::Varchar("[a]".into())],
                vec![Value::Varchar("[a]".into())],
            ]
        );
        assert_eq!(
            c.query("SELECT COALESCE(CASE WHEN k='z' THEN enum_range_boundary(k,NULL)::VARCHAR END,'skip') FROM t")?.rows,
            vec![
                vec![Value::Varchar("[z, a]".into())],
                vec![Value::Varchar("skip".into())],
                vec![Value::Varchar("skip".into())],
                vec![Value::Varchar("[z, a]".into())],
            ]
        );
        assert_eq!(
            c.query(
                "SELECT k::VARCHAR FROM t WHERE enum_range_boundary(k,NULL)::VARCHAR='[z, a]'"
            )?
            .rows,
            vec![
                vec![Value::Varchar("z".into())],
                vec![Value::Varchar("a".into())],
            ]
        );

        // Predicate mode keeps AND/OR demand-selected while preserving the
        // physical vector that enum_range_boundary reads. In particular, the
        // OR right side sees both rows here rather than a scalar row at a time.
        c.execute("CREATE TABLE predicate_boundary(i INTEGER,k ENUM('z','a')); INSERT INTO predicate_boundary VALUES (1,'z'),(2,'a')")?;
        let or_sql = "SELECT i FROM predicate_boundary WHERE i=100 OR enum_range_boundary(k,NULL)::VARCHAR='[z, a]'";
        assert_eq!(
            c.query(or_sql)?.rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        assert_eq!(
            c.execute_prepared(&c.prepare(or_sql)?, &[])?.rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        assert_eq!(
            c.query("SELECT i FROM predicate_boundary WHERE i=1 AND enum_range_boundary(k,NULL)::VARCHAR='[z, a]'")?.rows,
            vec![vec![Value::Integer(1)]]
        );
        assert_eq!(
            c.query("SELECT i FROM predicate_boundary WHERE i=1 OR enum_range_boundary(k,NULL)::VARCHAR='[z, a]'")?.rows,
            vec![vec![Value::Integer(1)]]
        );
        c.execute("CREATE TABLE predicate_lazy(i INTEGER,k VARCHAR); INSERT INTO predicate_lazy VALUES (1,'enum_bad'),(2,'enum_bad')")?;
        assert_eq!(
            c.query("SELECT i FROM predicate_lazy WHERE i=1 OR (i=2 OR enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR='[z, a]')")?.rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        assert_eq!(
            c.query("SELECT i FROM predicate_lazy WHERE (i=100 AND enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR='[z, a]') OR i=1")?.rows,
            vec![vec![Value::Integer(1)]]
        );
        let case_sql = "SELECT CASE WHEN i=100 AND enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR='[z, a]' THEN 1 ELSE 0 END FROM predicate_lazy";
        let case_expected = vec![vec![Value::Integer(0)], vec![Value::Integer(0)]];
        assert_eq!(c.query(case_sql)?.rows, case_expected);
        assert_eq!(
            c.execute_prepared(&c.prepare(case_sql)?, &[])?.rows,
            case_expected
        );

        // A generic parent retains child source order even when its later
        // child is a physical-batch callback. In particular, it must not
        // hoist the ENUM cast and hide the earlier INTEGER cast error.
        c.execute("CREATE TABLE sibling_order(v VARCHAR,k VARCHAR); INSERT INTO sibling_order VALUES ('left_bad','z'),('1','enum_bad')")?;
        let left_first = c.query("SELECT CAST(v AS INTEGER) + length(enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR) FROM sibling_order").unwrap_err();
        assert!(left_first.to_string().contains("left_bad"), "{left_first}");
        let physical_first = c.query("SELECT length(enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR) + CAST(v AS INTEGER) FROM sibling_order").unwrap_err();
        assert!(
            physical_first.to_string().contains("enum_bad"),
            "{physical_first}"
        );
        let null_on_constant = "SELECT pow(v::DOUBLE,length(enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR)) FROM sibling_order";
        let left_first = c.query(null_on_constant).unwrap_err();
        assert!(left_first.to_string().contains("left_bad"), "{left_first}");
        let prepared = c.prepare(null_on_constant)?;
        let left_first = c.execute_prepared(&prepared, &[]).unwrap_err();
        assert!(left_first.to_string().contains("left_bad"), "{left_first}");
        let null_short_circuit = "SELECT pow(NULL::DOUBLE,length(enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR)) FROM sibling_order";
        let null_expected = vec![vec![Value::Null], vec![Value::Null]];
        assert_eq!(c.query(null_short_circuit)?.rows, null_expected);
        assert_eq!(
            c.execute_prepared(&c.prepare(null_short_circuit)?, &[])?
                .rows,
            null_expected
        );
        let dynamic_null = "SELECT pow(NULLIF(1::DOUBLE,1::DOUBLE),length(enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR)) FROM sibling_order";
        assert_eq!(c.query(dynamic_null)?.rows, null_expected);
        assert_eq!(
            c.execute_prepared(&c.prepare(dynamic_null)?, &[])?.rows,
            null_expected
        );
        c.execute("CREATE TABLE null_order(v DOUBLE,k VARCHAR); INSERT INTO null_order VALUES (NULL,'enum_bad')")?;
        let flat_null = "SELECT pow(v,length(enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR)) FROM null_order";
        for sql in [flat_null] {
            let error = c.query(sql).unwrap_err();
            assert!(error.to_string().contains("enum_bad"), "{error}");
            let error = c.execute_prepared(&c.prepare(sql)?, &[]).unwrap_err();
            assert!(error.to_string().contains("enum_bad"), "{error}");
        }
        c.execute("CREATE TABLE physical_null(a ENUM('z','a'),b VARCHAR); INSERT INTO physical_null VALUES ('z','enum_bad')")?;
        let physical_null = "SELECT pow(CASE WHEN a='z' THEN NULL::DOUBLE ELSE length(enum_range_boundary(a,NULL)::VARCHAR) END,length(enum_range_boundary(b::ENUM('z','a'),NULL)::VARCHAR)) FROM physical_null";
        assert_eq!(c.query(physical_null)?.rows, vec![vec![Value::Null]]);
        assert_eq!(
            c.execute_prepared(&c.prepare(physical_null)?, &[])?.rows,
            vec![vec![Value::Null]]
        );
        let nullif_physical = "SELECT pow(NULLIF(length(enum_range_boundary(a,NULL)::VARCHAR),length(enum_range_boundary(a,NULL)::VARCHAR)),length(enum_range_boundary(b::ENUM('z','a'),NULL)::VARCHAR)) FROM physical_null";
        assert_eq!(c.query(nullif_physical)?.rows, vec![vec![Value::Null]]);
        assert_eq!(
            c.execute_prepared(&c.prepare(nullif_physical)?, &[])?.rows,
            vec![vec![Value::Null]]
        );
        c.execute("CREATE TABLE physical_null_two(a ENUM('z','a'),b VARCHAR); INSERT INTO physical_null_two VALUES ('z','enum_bad'),('a','enum_bad')")?;
        let nullif_two = "SELECT pow(NULLIF(length(enum_range_boundary(a,NULL)::VARCHAR),length(enum_range_boundary(a,NULL)::VARCHAR)),length(enum_range_boundary(b::ENUM('z','a'),NULL)::VARCHAR)) FROM physical_null_two";
        assert_eq!(
            c.query(nullif_two)?.rows,
            vec![vec![Value::Null], vec![Value::Null]]
        );
        let negative = "SELECT pow(NULLIF(length(enum_range_boundary(a,NULL)::VARCHAR),0),length(enum_range_boundary(b::ENUM('z','a'),NULL)::VARCHAR)) FROM physical_null";
        let error = c.query(negative).unwrap_err();
        assert!(error.to_string().contains("enum_bad"), "{error}");
        c.execute("CREATE TABLE physical_mixed(a ENUM('z','a'),b VARCHAR); INSERT INTO physical_mixed VALUES ('z','ok'),('a','enum_bad')")?;
        let mixed = "SELECT pow(CASE WHEN a='z' THEN NULL::DOUBLE ELSE length(enum_range_boundary(b::ENUM('z','a'),NULL)::VARCHAR) END,length(enum_range_boundary(b::ENUM('z','a'),NULL)::VARCHAR)) FROM physical_mixed";
        let error = c.query(mixed).unwrap_err();
        assert!(error.to_string().contains("enum_bad"), "{error}");
        c.execute("CREATE TABLE sibling_success(v VARCHAR,k VARCHAR); INSERT INTO sibling_success VALUES ('1','z'),('2','a')")?;
        assert_eq!(
            c.query("SELECT CAST(v AS INTEGER) + length(enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR) FROM sibling_success")?.rows,
            vec![vec![Value::Integer(7)], vec![Value::Integer(8)]]
        );

        // The callback receives child vectors before reading their first row.
        // A VARCHAR -> ENUM child is fallible, but valid rows must still form
        // one source-shaped physical batch for both evaluator adapters.
        c.execute("CREATE TABLE strings(k VARCHAR); INSERT INTO strings VALUES ('z'),('a')")?;
        let cast_sql = "SELECT enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR FROM strings";
        let cast_expected = vec![
            vec![Value::Varchar("[z, a]".into())],
            vec![Value::Varchar("[z, a]".into())],
        ];
        assert_eq!(c.query(cast_sql)?.rows, cast_expected);
        assert_eq!(
            c.execute_prepared(&c.prepare(cast_sql)?, &[])?.rows,
            cast_expected
        );

        // Child vectors are evaluated in source order: the bad first value is
        // observed before the later valid one. A selected CASE subset does not
        // evaluate the unselected bad cast and keeps its own batch boundary.
        c.execute("CREATE TABLE cast_order(k VARCHAR); INSERT INTO cast_order VALUES ('bad'),('z'),('z'),('a')")?;
        assert!(
            c.query("SELECT enum_range_boundary(k::ENUM('z','a'),NULL) FROM cast_order")
                .is_err()
        );
        let selected_cast_sql = "SELECT CASE WHEN k='z' THEN enum_range_boundary(k::ENUM('z','a'),NULL)::VARCHAR ELSE 'skip' END FROM cast_order";
        let selected_cast_expected = vec![
            vec![Value::Varchar("skip".into())],
            vec![Value::Varchar("[z, a]".into())],
            vec![Value::Varchar("[z, a]".into())],
            vec![Value::Varchar("skip".into())],
        ];
        assert_eq!(c.query(selected_cast_sql)?.rows, selected_cast_expected);
        assert_eq!(
            c.execute_prepared(&c.prepare(selected_cast_sql)?, &[])?
                .rows,
            selected_cast_expected
        );

        let prepared = c.prepare(sql)?;
        assert_eq!(
            ranges(
                c.execute_prepared(&prepared, &[])?
                    .rows
                    .into_iter()
                    .collect()
            ),
            expected
        );
        assert_eq!(
            c.query("SELECT enum_range_boundary('a'::ENUM('z','a'),NULL)::VARCHAR,enum_range_boundary(NULL,'z'::ENUM('z','a'))::VARCHAR,enum_range_boundary('a'::ENUM('z','a'),'z'::ENUM('z','a'))::VARCHAR")?.rows,
            vec![vec![
                Value::Varchar("[a]".into()),
                Value::Varchar("[z]".into()),
                Value::Varchar("[]".into()),
            ]]
        );

        let expressions = if expressions.name() == "scalar-expression" {
            Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>
        } else {
            Arc::new(BatchedEvaluator)
        };
        let db = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(4)
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t(k ENUM('z','a')); INSERT INTO t VALUES ('z'),('a'),('a'),('z')")?;
        assert_eq!(
            c.query("SELECT enum_range_boundary(k,NULL)::VARCHAR,CASE WHEN k='z' THEN enum_range_boundary(k,NULL)::VARCHAR ELSE 'skip' END FROM t")?.rows,
            vec![
                vec![Value::Varchar("[z, a]".into()), Value::Varchar("[z, a]".into())],
                vec![Value::Varchar("[z, a]".into()), Value::Varchar("skip".into())],
                vec![Value::Varchar("[z, a]".into()), Value::Varchar("skip".into())],
                vec![Value::Varchar("[z, a]".into()), Value::Varchar("[z, a]".into())],
            ]
        );
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
