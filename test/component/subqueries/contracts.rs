use super::*;
use duckdb_rust::{
    common::{
        cast::CastRegistry,
        type_registry::{
            TypeRegistry,
            ascii::{self, AsciiCast, MaterializedAscii, StreamingAscii},
        },
    },
    function::{FunctionEffects, FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
    planner::{
        BoundExpr, BoundStatement, ExprKind, Field, LogicalPlan, PlanNode, expression::SubqueryKind,
    },
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn membership_retains_type_adapters_and_primitive_comparison_semantics() -> Result<()> {
    for subqueries in adapters() {
        for ascii_type in [
            Arc::new(MaterializedAscii) as Arc<dyn duckdb_rust::common::type_registry::TypeAdapter>,
            Arc::new(StreamingAscii),
        ] {
            let mut types = TypeRegistry::builtins();
            types.register(ascii::FAMILY, ascii_type)?;
            let mut casts = CastRegistry::builtins();
            let text_type = ascii::data_type(64)?;
            casts.register_type(&text_type, &types)?;
            for mode in [
                duckdb_rust::common::cast::CastMode::Explicit,
                duckdb_rust::common::cast::CastMode::Assignment,
            ] {
                for (source, target) in [
                    (DataType::Varchar, text_type.clone()),
                    (text_type.clone(), DataType::Varchar),
                ] {
                    casts.register(
                        duckdb_rust::common::cast::CastSpec {
                            source,
                            target,
                            mode,
                        },
                        Arc::new(AsciiCast),
                    )?;
                }
            }
            let db = DatabaseBuilder::new()
                .subqueries(subqueries.clone())
                .types(Arc::new(types))
                .casts(casts)
                .build()?;
            let mut connection = db.connect();
            for sql in [
                "SELECT 'NaN'::FLOAT IN(SELECT 'NaN'::DOUBLE)",
                "SELECT '-0'::FLOAT IN(SELECT 0::DOUBLE)",
                "SELECT 127::TINYINT IN(SELECT 127::BIGINT)",
                "SELECT DATE '2000-02-29' IN(SELECT DATE '2000-02-29')",
                "SELECT '🦆' IN(SELECT '🦆')",
                "SELECT 'Alpha'::ascii_ci(64) IN(SELECT 'ALPHA'::ascii_ci(64))",
                "SELECT (SELECT 'Alpha'::ascii_ci(64))='alpha'::ascii_ci(64)",
            ] {
                assert_eq!(
                    connection.query(sql)?.rows,
                    vec![vec![Value::Boolean(true)]],
                    "{sql}"
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct CountCalls(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for CountCalls {
    fn name(&self) -> &str {
        "count_calls"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: false,
        }
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if arguments.len() != 1 {
            return Err(Error::Bind("count_calls takes one argument".into()));
        }
        Ok(arguments[0].clone())
    }
    fn evaluate(&self, arguments: &[Value], _: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(arguments[0].clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn demand_short_circuit_effects_and_resource_limits_are_explicit() -> Result<()> {
    for (index, subqueries) in adapters().into_iter().enumerate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(CountCalls(calls.clone())))?;
        let db = DatabaseBuilder::new()
            .subqueries(subqueries.clone())
            .functions(functions)
            .batch_size(1)
            .max_intermediate_rows(200)
            .build()?;
        let mut connection = db.connect();
        assert_eq!(
            connection
                .query("SELECT EXISTS(SELECT count_calls(i) FROM range(100) t(i))")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "EXISTS ignores its target list"
        );
        assert_eq!(
            connection
                .query("SELECT EXISTS(SELECT 1 FROM range(100) t(i) WHERE count_calls(i)>=0)")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(
            calls.swap(0, Ordering::SeqCst),
            if index == 0 { 1 } else { 100 }
        );
        assert_eq!(
            connection
                .query("SELECT 0 IN(SELECT count_calls(i) FROM range(100) t(i))")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(
            calls.swap(0, Ordering::SeqCst),
            if index == 0 { 1 } else { 100 }
        );
        assert!(matches!(
            connection.query("SELECT (SELECT count_calls(i) FROM range(100) t(i))"),
            Err(Error::Execution(_))
        ));
        assert_eq!(
            calls.swap(0, Ordering::SeqCst),
            if index == 0 { 2 } else { 100 }
        );
        connection.query(
            "SELECT CASE WHEN false THEN (SELECT count_calls(i) FROM range(100) t(i)) ELSE 1 END",
        )?;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            connection
                .query("SELECT (SELECT count_calls(7)) FROM range(100)")?
                .rows
                .len(),
            100
        );
        assert_eq!(
            calls.swap(0, Ordering::SeqCst),
            1,
            "uncorrelated scalar initializes once"
        );
        assert_eq!(
            connection
                .query("SELECT (SELECT count_calls(t.i)) FROM range(100) t(i)")?
                .rows
                .len(),
            100
        );
        assert_eq!(
            calls.swap(0, Ordering::SeqCst),
            100,
            "correlated rows retain distinct evaluations"
        );
        connection.execute("CREATE TABLE outer_keys(k BIGINT); INSERT INTO outer_keys VALUES(0),(1),(2); CREATE TABLE inner_keys(k BIGINT); INSERT INTO inner_keys VALUES(0),(1),(2)")?;
        assert_eq!(
            connection.query("SELECT count(*) FROM outer_keys t WHERE EXISTS(SELECT 1 FROM inner_keys u WHERE u.k=count_calls(t.k))")?.rows,
            vec![vec![Value::Integer(3)]]
        );
        assert_eq!(
            calls.swap(0, Ordering::SeqCst),
            if index == 0 { 6 } else { 9 },
            "volatile captured keys retain dependent evaluation order and demand"
        );
        let limited = DatabaseBuilder::new()
            .subqueries(subqueries)
            .batch_size(1)
            .max_intermediate_rows(2)
            .build()?;
        let result = limited
            .connect()
            .query("SELECT 99 IN(SELECT i FROM range(100) t(i))");
        if index == 0 {
            assert_eq!(result?.rows, vec![vec![Value::Boolean(true)]]);
        } else {
            assert!(matches!(result, Err(Error::Resource(_))));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn relation(expr: BoundExpr) -> LogicalPlan {
    LogicalPlan {
        schema: vec![Field::new("v", expr.data_type.clone())],
        node: PlanNode::Values(vec![vec![expr]]),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nested_plan_validation_checks_scope_types_schema_and_total_depth() -> Result<()> {
    use duckdb_rust::{
        storage::checkpoint::MemoryDurability,
        transaction::{SnapshotTransactions, TransactionManager},
    };
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let query = QueryContext::background();
    for (depth, column, data_type) in [
        (0, 0, DataType::Integer),
        (1, 0, DataType::Integer),
        (2, 0, DataType::Integer),
        (1, 1, DataType::Integer),
        (1, 0, DataType::Varchar),
    ] {
        let valid = depth == 1 && column == 0 && data_type == DataType::Integer;
        let inner = relation(BoundExpr {
            kind: ExprKind::OuterColumn { depth, column },
            data_type,
        });
        let expression = BoundExpr {
            data_type: DataType::Integer,
            kind: ExprKind::Subquery(Arc::new(duckdb_rust::planner::expression::BoundSubquery {
                plan: Arc::new(inner),
                kind: SubqueryKind::Scalar,
            })),
        };
        let outer = LogicalPlan {
            schema: vec![Field::new("v", DataType::Integer)],
            node: PlanNode::Projection {
                input: Box::new(relation(BoundExpr {
                    kind: ExprKind::Literal(Value::Integer(1)),
                    data_type: DataType::Integer,
                })),
                expressions: vec![expression],
            },
        };
        assert_eq!(outer.validate(tx.catalog(), &query).is_ok(), valid);
    }
    let mut expression = BoundExpr::literal(Value::Integer(1));
    for _ in 0..70 {
        expression = BoundExpr {
            data_type: expression.data_type.clone(),
            kind: ExprKind::Subquery(Arc::new(duckdb_rust::planner::expression::BoundSubquery {
                plan: Arc::new(relation(expression)),
                kind: SubqueryKind::Scalar,
            })),
        };
    }
    assert!(matches!(
        BoundStatement::Query(relation(expression)).validate(tx.catalog(), &query),
        Err(Error::Resource(_))
    ));
    let mut connection = Database::memory()?.connect();
    connection.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES(1),(2)")?;
    for sql in [
        "SELECT (SELECT max(t.i)) FROM t",
        "SELECT (SELECT count(*) FILTER(WHERE t.i>0)) FROM t",
    ] {
        assert!(
            matches!(connection.query(sql), Err(Error::Unsupported(_))),
            "outer aggregates must not silently bind to the inner scope"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn file_backed_subquery_mutations_survive_rollback_checkpoint_and_restart() -> Result<()> {
    for logged in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("subqueries.duckdb");
        let db = if logged {
            Database::open_logged(&path)?
        } else {
            Database::open(&path)?
        };
        let mut connection = db.connect();
        connection.execute("CREATE TABLE t(i INTEGER PRIMARY KEY,v VARCHAR); INSERT INTO t VALUES(1,'a'),(2,'b'); UPDATE t SET v=(SELECT max(v) FROM t b) WHERE i IN(SELECT i FROM t)")?;
        connection.execute("BEGIN; DELETE FROM t WHERE EXISTS(SELECT 1 FROM t b WHERE b.i=t.i); ROLLBACK; CHECKPOINT; INSERT INTO t SELECT 3,(SELECT v FROM t WHERE i=1)")?;
        drop(connection);
        drop(db);
        assert_eq!(
            Database::open_read_only(&path)?
                .connect()
                .query("SELECT count(*) FROM t WHERE v=(SELECT v FROM t WHERE i=2)")?
                .rows,
            vec![vec![Value::Integer(3)]]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn concurrent_nested_queries_keep_outer_frames_and_owned_results_independent() -> Result<()> {
    for subqueries in adapters() {
        let db = DatabaseBuilder::new().subqueries(subqueries).build()?;
        let workers: Vec<_> = (0..4)
            .map(|start| {
                let db = db.clone();
                std::thread::spawn(move || -> Result<()> {
                    let mut connection = db.connect();
                    let statement = connection.prepare(
                        "SELECT i,(SELECT(SELECT t.i+$1)) v FROM range(10) t(i) ORDER BY i",
                    )?;
                    for value in start..start + 10 {
                        let result =
                            connection.execute_prepared(&statement, &[Value::Integer(value)])?;
                        assert_eq!(
                            result.rows,
                            (0..10)
                                .map(|i| vec![Value::Integer(i), Value::Integer(i + value)])
                                .collect::<Vec<_>>()
                        );
                    }
                    Ok(())
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap()?;
        }
    }
    Ok(())
}
