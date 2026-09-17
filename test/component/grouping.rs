use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    execution::{
        Executor, MaterializingExecutor, PullExecutor,
        expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
        operator::aggregate::{AggregationAlgorithm, HashAggregation, OrderedAggregation},
        physical_plan::NativePhysicalPlanner,
    },
    function::{FunctionEffects, FunctionRegistry, ScalarFunction},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::{InterruptHandle, QueryContext},
    planner::{
        BoundExpr, Field, LogicalPlan, PlanNode,
        aggregation::{AggregateOutput, Aggregation, GroupingSet},
    },
    storage::table::Snapshot,
};

#[path = "grouping/columns.rs"]
mod columns;
#[path = "grouping/domains.rs"]
mod domains;
#[path = "grouping/product.rs"]
mod product;
#[path = "../runner/mod.rs"]
mod runner;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn algorithms() -> Vec<Arc<dyn AggregationAlgorithm>> {
    vec![Arc::new(HashAggregation), Arc::new(OrderedAggregation)]
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouping_algorithms_share_the_sql_contract_across_compositions() -> Result<()> {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/sql/grouping.test");
    for algorithm in algorithms() {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for expressions in [
                Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                Arc::new(BatchedEvaluator),
            ] {
                for executor in [
                    Arc::new(PullExecutor) as Arc<dyn Executor>,
                    Arc::new(MaterializingExecutor),
                ] {
                    for batch_size in [1, 3, 2048] {
                        let db = DatabaseBuilder::new()
                            .physical_planner(Arc::new(
                                NativePhysicalPlanner::default()
                                    .with_aggregation(algorithm.clone()),
                            ))
                            .optimizer(optimizer.clone())
                            .expressions(expressions.clone())
                            .executor(executor.clone())
                            .batch_size(batch_size)
                            .build()?;
                        assert!(db.adapters().contains(&("aggregation", algorithm.name())));
                        assert_eq!(runner::run_file(&db, &corpus)?, 35);
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Observe(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Observe {
    fn name(&self) -> &str {
        "observe"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: false,
        }
    }
    fn return_type(
        &self,
        args: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if args.len() == 1 {
            Ok(args[0].clone())
        } else {
            Err(Error::Bind("observe requires one input".into()))
        }
    }
    fn evaluate(&self, args: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(args[0].clone())
    }
}

#[derive(Debug)]
struct PureProbe {
    calls: Arc<AtomicUsize>,
    fail: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for PureProbe {
    fn name(&self) -> &str {
        "pure_probe"
    }
    fn return_type(
        &self,
        args: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if args == [DataType::BigInt] {
            Ok(DataType::BigInt)
        } else {
            Err(Error::Bind("pure_probe requires BIGINT".into()))
        }
    }
    fn evaluate(&self, args: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        let value = args[0].as_i128()?;
        if self.fail && value == 2 {
            return Err(Error::Conversion("first pure probe failure".into()));
        }
        if self.fail && value == 3 {
            return Err(Error::Conversion("second pure probe failure".into()));
        }
        Ok(Value::Integer(value))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn single_ungrouped_aggregate_keeps_batch_results_errors_and_evaluation_counts() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(PureProbe {
            calls: calls.clone(),
            fail: false,
        }))?;
        let db = DatabaseBuilder::new()
            .functions(functions)
            .expressions(expressions.clone())
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(Arc::new(HashAggregation)),
            ))
            .batch_size(4)
            .build()?;
        assert_eq!(
            db.connect()
                .query("SELECT sum(pure_probe(i)) FROM range(4) t(i)")?
                .rows,
            vec![vec![Value::Integer(6)]],
        );
        assert_eq!(calls.load(Ordering::Relaxed), 4);

        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(PureProbe { calls, fail: true }))?;
        let db = DatabaseBuilder::new()
            .functions(functions)
            .expressions(expressions)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(Arc::new(HashAggregation)),
            ))
            .batch_size(4)
            .build()?;
        assert!(matches!(
            db.connect().query("SELECT sum(pure_probe(i)) FROM range(4) t(i)"),
            Err(Error::Conversion(message)) if message == "first pure probe failure"
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn order_insensitive_sum_elides_only_total_keys() -> Result<()> {
    // A column key is pure and total, so exact SUM may stream it without
    // retaining ordered rows. A registered probe is pure but not proven total:
    // it must still execute in source order and expose its first failure.
    for fail in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(PureProbe {
            calls: calls.clone(),
            fail,
        }))?;
        let db = DatabaseBuilder::new()
            .functions(functions)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(Arc::new(HashAggregation)),
            ))
            .batch_size(4)
            .build()?;
        let result = db
            .connect()
            .query("SELECT sum(i ORDER BY pure_probe(i)) FROM range(4) t(i)");
        if fail {
            assert!(matches!(
                result,
                Err(Error::Conversion(message)) if message == "first pure probe failure"
            ));
            assert_eq!(calls.load(Ordering::Relaxed), 3);
        } else {
            assert_eq!(result?.rows, vec![vec![Value::Integer(6)]]);
            assert_eq!(calls.load(Ordering::Relaxed), 4);
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(Observe(calls.clone())))?;
    let db = DatabaseBuilder::new()
        .functions(functions)
        .physical_planner(Arc::new(
            NativePhysicalPlanner::default().with_aggregation(Arc::new(HashAggregation)),
        ))
        .batch_size(4)
        .build()?;
    assert_eq!(
        db.connect()
            .query("SELECT sum(i ORDER BY observe(i)) FROM range(4) t(i)")?
            .rows,
        vec![vec![Value::Integer(6)]],
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);

    // An order key can be elided from execution for exact SUM, but it still
    // participates in aggregate scope validation. An outer-only key is an
    // unsupported outer aggregate, while a key mixed with an inner argument
    // is a valid correlated aggregate.
    let db = Database::memory()?;
    let connection = db.connect();
    assert!(matches!(
        connection.query(
            "SELECT (SELECT sum(1 ORDER BY outer_rows.i) FROM range(1) inner_rows(j)) \
             FROM range(1) outer_rows(i)",
        ),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(
        connection
            .query(
                "SELECT (SELECT sum(inner_rows.j ORDER BY outer_rows.i) \
                 FROM range(1) inner_rows(j)) FROM range(3) outer_rows(i) ORDER BY i",
            )?
            .rows,
        vec![
            vec![Value::Integer(0)],
            vec![Value::Integer(0)],
            vec![Value::Integer(0)],
        ]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouping_evaluates_inputs_once_and_isolates_distinct_and_filter_states() -> Result<()> {
    for algorithm in algorithms() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(Observe(calls.clone())))?;
        let db = DatabaseBuilder::new()
            .functions(functions)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .batch_size(2)
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t(a INTEGER, v INTEGER); INSERT INTO t VALUES (1,1),(1,1),(2,2)")?;
        let result = c.query("SELECT count(DISTINCT observe(v)),sum(v) FILTER (WHERE observe(v)>1),grouping(a) FROM t GROUP BY GROUPING SETS ((a),(),()) ORDER BY 3,1")?;
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Integer(1), Value::Null, Value::Integer(0)],
                vec![Value::Integer(1), Value::Integer(2), Value::Integer(0)],
                vec![Value::Integer(2), Value::Integer(2), Value::Integer(1)],
                vec![Value::Integer(2), Value::Integer(2), Value::Integer(1)],
            ]
        );
        assert_eq!(calls.load(Ordering::Relaxed), 6);
        calls.store(0, Ordering::Relaxed);
        c.query("SELECT sum(observe(v)) FILTER(WHERE false),grouping(a) FROM t GROUP BY CUBE(a)")?;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let result = c.query("SELECT observe(sum(v)) AS k,observe(count(*)) AS k FROM t GROUP BY a ORDER BY ALL DESC")?;
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Integer(2), Value::Integer(2)],
                vec![Value::Integer(2), Value::Integer(1)],
            ]
        );
        assert_eq!(calls.load(Ordering::Relaxed), 4);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aggregate_argument_ordering_is_stable_grouped_and_modifier_aware() -> Result<()> {
    for algorithm in algorithms() {
        let db = DatabaseBuilder::new()
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .batch_size(2)
            .build()?;
        let mut connection = db.connect();
        connection.execute(
            "CREATE TABLE ordered(g INTEGER, v INTEGER, k INTEGER); \
             INSERT INTO ordered VALUES \
                (1,10,2),(1,20,1),(1,30,NULL),(2,5,NULL),(2,7,1),(3,8,1),(3,9,1)",
        )?;
        assert_eq!(
            connection
                .query(
                    "SELECT g,first(v ORDER BY k NULLS LAST),last(v ORDER BY k NULLS LAST), \
                            sum(v ORDER BY k DESC),count(v ORDER BY k) \
                     FROM ordered GROUP BY g ORDER BY g",
                )?
                .rows,
            vec![
                vec![
                    Value::Integer(1),
                    Value::Integer(20),
                    Value::Integer(30),
                    Value::Integer(60),
                    Value::Integer(3),
                ],
                vec![
                    Value::Integer(2),
                    Value::Integer(7),
                    Value::Integer(5),
                    Value::Integer(12),
                    Value::Integer(2),
                ],
                vec![
                    Value::Integer(3),
                    Value::Integer(8),
                    Value::Integer(9),
                    Value::Integer(17),
                    Value::Integer(2),
                ],
            ]
        );
        assert_eq!(
            connection
                .query(
                    "SELECT first(DISTINCT v ORDER BY v DESC), \
                            first(v ORDER BY k) FILTER (WHERE v <> 10) FROM ordered WHERE g=1",
                )?
                .rows,
            vec![vec![Value::Integer(30), Value::Integer(20)]],
        );
        // The blocking sort is stable: equal argument-order keys preserve the
        // stream order used by FIRST and LAST.
        assert_eq!(
            connection
                .query("SELECT first(v ORDER BY k),last(v ORDER BY k) FROM ordered WHERE g=3")?
                .rows,
            vec![vec![Value::Integer(8), Value::Integer(9)]],
        );
        // FIRST/LAST keep one stable candidate: multi-key comparisons, NULL
        // arguments, and ties retain exactly the sorted-wrapper result.
        assert_eq!(
            connection
                .query(
                    "SELECT first(v ORDER BY k,j),last(v ORDER BY k,j) \
                     FROM (VALUES (10,1,1),(20,1,2),(30,1,2), \
                                  (NULL::INTEGER,1,0),(40,NULL,0)) t(v,k,j)",
                )?
                .rows,
            vec![vec![Value::Null, Value::Integer(40)]],
        );
        // An ungrouped aggregate takes the same buffered path and honours
        // direction plus explicit NULL placement.
        assert_eq!(
            connection
                .query(
                    "SELECT first(v ORDER BY k DESC NULLS FIRST),last(v ORDER BY k DESC NULLS FIRST) \
                     FROM ordered WHERE g=1",
                )?
                .rows,
            vec![vec![Value::Integer(30), Value::Integer(20)]],
        );
        assert_eq!(
            connection
                .query(
                    "SELECT first(v ORDER BY k),last(v ORDER BY k),sum(v ORDER BY k),count(v ORDER BY k) \
                     FROM ordered WHERE false",
                )?
                .rows,
            vec![vec![Value::Null, Value::Null, Value::Null, Value::Integer(0)]],
        );
        assert_eq!(
            connection
                .query(
                    "SELECT first(v ORDER BY k),last(v ORDER BY k) \
                     FROM (VALUES (NULL::INTEGER,1),(NULL::INTEGER,2)) AS nulls(v,k)",
                )?
                .rows,
            vec![vec![Value::Null, Value::Null]],
        );
        assert!(
            connection
                .query("SELECT sum(v,k ORDER BY k) FROM ordered")
                .is_err()
        );
        assert!(
            connection
                .query("SELECT sum(v ORDER BY missing) FROM ordered")
                .is_err()
        );
        assert!(matches!(
            connection.query(
                "SELECT first(DISTINCT v ORDER BY k) \
                 FROM (VALUES (1,9),(1,2),(2,1)) AS distinct_order(v,k)",
            ),
            Err(Error::Bind(message)) if message == "In a DISTINCT aggregate, ORDER BY expressions must appear in the argument list"
        ));
        assert!(matches!(
            connection.query(
                "SELECT sum(v) WITHIN GROUP (ORDER BY abs(v)) FROM ordered",
            ),
            Err(Error::Parse(message)) if message == "Unknown ordered aggregate \"sum\""
        ));
        // Numeric argument-order literals are constants, never SELECT-list
        // ordinals. A stable tie therefore preserves the input order.
        assert_eq!(
            connection
                .query(
                    "SELECT first(v ORDER BY 2) FROM (VALUES (10,2),(20,1)) AS literal_order(v,k)"
                )?
                .rows,
            vec![vec![Value::Integer(10)]],
        );
    }
    for algorithm in algorithms() {
        let db = DatabaseBuilder::new()
            .max_intermediate_rows(8)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .build()?;
        // Candidate aggregates no longer reserve stable-sort permutations for
        // every input row, while an order-sensitive buffered aggregate still
        // receives the original resource accounting.
        assert_eq!(
            db.connect()
                .query(
                    "SELECT first(v ORDER BY k),last(v ORDER BY k) \
                     FROM (VALUES (1,10,1),(2,20,1)) AS limited(g,v,k) GROUP BY g",
                )?
                .rows,
            vec![
                vec![Value::Integer(10), Value::Integer(10)],
                vec![Value::Integer(20), Value::Integer(20)]
            ],
        );
        assert!(matches!(
            db.connect().query(
                "SELECT avg(v::DOUBLE ORDER BY k) \
                 FROM (VALUES (1,10,1),(2,20,1)) AS limited(g,v,k) GROUP BY g",
            ),
            Err(Error::Resource(_))
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouping_boundaries_reject_invalid_ordinals_masks_expansion_and_resources() -> Result<()> {
    let query = QueryContext::background();
    let snapshot = Snapshot::default();
    for (sets, outputs) in [
        (vec![], vec![]),
        (vec![GroupingSet::new([1])], vec![]),
        (
            vec![GroupingSet::new([0])],
            vec![AggregateOutput::Grouping(vec![1])],
        ),
        (
            vec![GroupingSet::new([0])],
            vec![AggregateOutput::Grouping(vec![])],
        ),
        (
            vec![GroupingSet::new([0])],
            vec![AggregateOutput::Grouping(vec![0; 64])],
        ),
    ] {
        let aggregation = Aggregation {
            groups: vec![BoundExpr::column(0, DataType::Integer)],
            sets,
            outputs,
        };
        assert!(matches!(
            aggregation.validate_metadata(&query),
            Err(Error::Internal(_))
        ));
        let plan = LogicalPlan {
            schema: vec![Field::new("a", DataType::Integer)],
            node: PlanNode::Aggregate {
                input: Box::new(LogicalPlan {
                    schema: vec![Field::new("a", DataType::Integer)],
                    node: PlanNode::Values(vec![]),
                }),
                aggregation,
            },
        };
        assert!(plan.validate(&snapshot, &query).is_err());
    }
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 10)?;
    interrupt.interrupt();
    assert!(matches!(
        Aggregation {
            groups: vec![],
            sets: vec![GroupingSet::new([])],
            outputs: vec![]
        }
        .validate_metadata(&cancelled),
        Err(Error::Interrupted)
    ));
    for algorithm in algorithms() {
        let db = DatabaseBuilder::new()
            .max_intermediate_rows(2)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .build()?;
        let mut c = db.connect();
        assert!(matches!(
            c.query("SELECT count(*) GROUP BY GROUPING SETS ((),(),())"),
            Err(Error::Resource(_))
        ));
        let columns = std::iter::repeat_n("a", 16).collect::<Vec<_>>().join(",");
        assert!(matches!(
            c.query(&format!(
                "SELECT count(*) FROM (VALUES (1))t(a) GROUP BY CUBE({columns})"
            )),
            Err(Error::Parse(_))
        ));
        let columns = std::iter::repeat_n("a", 64).collect::<Vec<_>>().join(",");
        assert!(matches!(
            c.query(&format!(
                "SELECT GROUPING({columns}) FROM (VALUES (1))t(a) GROUP BY a"
            )),
            Err(Error::Bind(_))
        ));
        for clause in [
            "GROUPING SETS(ROLLUP(DISTINCT a))",
            "GROUPING SETS(CUBE(a) IGNORE NULLS)",
            "GROUPING SETS(ROLLUP(a) FILTER(WHERE true))",
        ] {
            assert!(
                c.query(&format!("SELECT a FROM (VALUES(1))t(a) GROUP BY {clause}"))
                    .is_err(),
                "accepted invalid grouping construct: {clause}"
            );
        }
        assert!(matches!(
            c.query("SELECT a FROM (VALUES(1))t(a) GROUP BY ROLLUP(ROLLUP(a))"),
            Err(Error::Catalog(_))
        ));
        assert_eq!(c.query("SELECT 1")?.rows, vec![vec![Value::Integer(1)]]);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouping_prepared_queries_keep_snapshot_visibility_and_atomic_mutations() -> Result<()> {
    for algorithm in algorithms() {
        let db = DatabaseBuilder::new()
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .batch_size(2)
            .build()?;
        let mut writer = db.connect();
        let mut reader = db.connect();
        writer.execute("CREATE TABLE t(a INTEGER,v BIGINT); INSERT INTO t VALUES(1,2),(1,3); CREATE TABLE sums(a INTEGER,s HUGEINT,g BIGINT)")?;
        let prepared = writer.prepare(
            "SELECT a,sum(v),grouping(a) FROM t GROUP BY CUBE(a) ORDER BY grouping(a),a",
        )?;
        reader.execute("BEGIN")?;
        let old = reader
            .query("SELECT a,sum(v),grouping(a) FROM t GROUP BY CUBE(a) ORDER BY grouping(a),a")?;
        writer.execute("INSERT INTO t VALUES(2,4)")?;
        assert_eq!(writer.execute_prepared(&prepared, &[])?.rows.len(), 3);
        assert_eq!(
            reader
                .query(
                    "SELECT a,sum(v),grouping(a) FROM t GROUP BY CUBE(a) ORDER BY grouping(a),a"
                )?
                .rows,
            old.rows
        );
        reader.execute("ROLLBACK")?;
        writer.execute(
            "BEGIN; INSERT INTO sums SELECT a,sum(v),grouping(a) FROM t GROUP BY CUBE(a); ROLLBACK",
        )?;
        assert!(writer.query("SELECT * FROM sums")?.rows.is_empty());
        writer.execute("INSERT INTO sums SELECT a,sum(v),grouping(a) FROM t GROUP BY CUBE(a)")?;
        assert_eq!(
            writer.query("SELECT * FROM sums ORDER BY g,a")?.rows,
            writer.execute_prepared(&prepared, &[])?.rows
        );
        writer.execute("BEGIN")?;
        assert!(writer.execute("INSERT INTO sums SELECT a,sum(v),grouping(a) FROM t GROUP BY CUBE(a) HAVING CAST('bad' AS INTEGER)>0").is_err());
        writer.execute("ROLLBACK")?;
        assert_eq!(
            writer.query("SELECT count(*) FROM sums")?.rows,
            vec![vec![Value::Integer(3)]]
        );
        assert_eq!(old.rows.len(), 2);
    }
    Ok(())
}
