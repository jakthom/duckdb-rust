use std::sync::{
    Arc, Mutex,
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

struct InterruptFilter(Arc<Mutex<Option<InterruptHandle>>>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for InterruptFilter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InterruptFilter")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for InterruptFilter {
    fn name(&self) -> &str {
        "interrupt_filter"
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
            Ok(DataType::Boolean)
        } else {
            Err(Error::Bind("interrupt_filter requires one input".into()))
        }
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .expect("installed interrupt handle")
            .interrupt();
        Ok(Value::Boolean(true))
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
    let mut connection = db.connect();
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
        calls.store(0, Ordering::Relaxed);
        assert_eq!(
            c.query(
                "SELECT list_extract(list(DISTINCT v ORDER BY v) \
                    FILTER(observe(a < 2)),1) FROM t",
            )?
            .rows,
            vec![vec![Value::Integer(1)]],
        );
        // A volatile FILTER is evaluated once for every source row and keeps
        // the registered LIST state on the generic modifier driver.
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aggregate_filter_shorthand_composes_with_distinct_and_argument_ordering() -> Result<()> {
    for algorithm in algorithms() {
        for batch_size in [1, 2, 2048] {
            let db = DatabaseBuilder::new()
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_aggregation(algorithm.clone()),
                ))
                .batch_size(batch_size)
                .build()?;
            let mut c = db.connect();
            c.execute(
                "CREATE TABLE filtered(g INTEGER,v INTEGER,k INTEGER,keep BOOLEAN); \
                 INSERT INTO filtered VALUES \
                 (1,1,3,true),(1,1,2,true),(1,2,1,true),(1,NULL,0,true), \
                 (1,3,4,false),(2,4,2,false),(2,4,1,NULL),(2,5,0,true)",
            )?;
            assert_eq!(
                c.query(
                    "SELECT g, \
                            count(*) FILTER(keep), \
                            count(DISTINCT v) FILTER (WHERE keep), \
                            sum(DISTINCT v) FILTER(keep), \
                            list_extract(list(DISTINCT v ORDER BY v DESC) \
                                FILTER(keep AND v IS NOT NULL),1), \
                            list_extract(list(DISTINCT v ORDER BY v DESC) \
                                FILTER (WHERE keep AND v IS NOT NULL),2), \
                            first(v ORDER BY k) FILTER(keep), \
                            last(v ORDER BY k) FILTER (WHERE keep) \
                     FROM filtered GROUP BY g ORDER BY g",
                )?
                .rows,
                vec![
                    vec![
                        Value::Integer(1),
                        Value::Integer(4),
                        Value::Integer(2),
                        Value::Integer(3),
                        Value::Integer(2),
                        Value::Integer(1),
                        Value::Null,
                        Value::Integer(1),
                    ],
                    vec![
                        Value::Integer(2),
                        Value::Integer(1),
                        Value::Integer(1),
                        Value::Integer(5),
                        Value::Integer(5),
                        Value::Null,
                        Value::Integer(5),
                        Value::Integer(5),
                    ],
                ],
            );
            assert_eq!(
                c.query(
                    "SELECT count(*) FILTER(false),sum(v) FILTER(NULL), \
                            list(v ORDER BY k) FILTER(false), \
                            first(v ORDER BY k) FILTER(NULL),last(v ORDER BY k) FILTER(false) \
                     FROM filtered",
                )?
                .rows,
                vec![vec![
                    Value::Integer(0),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ]],
            );
            assert_eq!(
                c.query(
                    "SELECT list_extract(array_agg(DISTINCT v ORDER BY v DESC) \
                         FILTER(keep AND v IS NOT NULL),1) FROM filtered",
                )?
                .rows,
                vec![vec![Value::Integer(5)]],
            );
            assert_eq!(
                c.query("SELECT sum(v % (v-v)) FILTER(false) FROM filtered")?
                    .rows,
                vec![vec![Value::Null]],
            );
            assert_eq!(
                c.query(
                    "SELECT g, \
                            first(DISTINCT v ORDER BY v) \
                                FILTER(keep AND v IS NOT NULL), \
                            last(DISTINCT v ORDER BY v) \
                                FILTER (WHERE keep AND v IS NOT NULL) \
                     FROM filtered GROUP BY g ORDER BY g",
                )?
                .rows,
                vec![
                    vec![Value::Integer(1), Value::Integer(1), Value::Integer(2)],
                    vec![Value::Integer(2), Value::Integer(5), Value::Integer(5)],
                ],
            );
            c.execute(
                "CREATE TABLE signed_boundary(v BIGINT,keep BOOLEAN); \
                 INSERT INTO signed_boundary VALUES \
                 (-9223372036854775808,true),(9223372036854775807,true), \
                 (-9223372036854775808,true)",
            )?;
            assert_eq!(
                c.query(
                    "SELECT first(DISTINCT v ORDER BY v) FILTER(keep), \
                            last(DISTINCT v ORDER BY v) FILTER(WHERE keep), \
                            count(DISTINCT v) FILTER(keep) FROM signed_boundary",
                )?
                .rows,
                vec![vec![
                    Value::Integer(i128::from(i64::MIN)),
                    Value::Integer(i128::from(i64::MAX)),
                    Value::Integer(2),
                ]],
            );
            let prepared = c.prepare(
                "SELECT count(DISTINCT v) FILTER(v > $1), \
                        sum(v) FILTER (WHERE v > $1) FROM filtered",
            )?;
            assert_eq!(
                c.execute_prepared(&prepared, &[Value::Integer(1)])?.rows,
                vec![vec![Value::Integer(4), Value::Integer(18)]],
            );
            assert!(matches!(
                c.query("SELECT sum(v) FILTER(v) FROM filtered"),
                Err(Error::Bind(message)) if message == "predicate must be BOOLEAN"
            ));
            assert!(matches!(
                c.query(
                    "SELECT first(DISTINCT v ORDER BY k) FILTER(missing) FROM filtered"
                ),
                Err(Error::Bind(message)) if message == "In a DISTINCT aggregate, ORDER BY expressions must appear in the argument list"
            ));
            assert!(matches!(
                c.query("SELECT count(*) FILTER(missing) FROM filtered"),
                Err(Error::Bind(message)) if message == "column missing not found"
            ));
            assert!(matches!(
                c.query("SELECT abs(v) FILTER(v > 0) FROM filtered"),
                Err(Error::Bind(message)) if message == "FILTER requires an aggregate"
            ));
            assert!(matches!(
                c.query(
                    "SELECT (SELECT sum(1) FILTER(i > 0) FROM range(1) inner_rows(j)) \
                     FROM range(1) outer_rows(i)"
                ),
                Err(Error::Unsupported(_))
            ));
        }
    }

    let mut encoded = DatabaseBuilder::new().batch_size(7).build()?.connect();
    encoded.execute(
        "CREATE TABLE encoded_filter AS SELECT i, \
             CASE WHEN i%5=0 THEN NULL ELSE i%10 END AS v, i%3=0 AS keep \
         FROM range(100) t(i)",
    )?;
    assert_eq!(
        encoded
            .query(
                "SELECT count(*) FILTER(keep),sum(v) FILTER(keep), \
                        count(DISTINCT v) FILTER(keep), \
                        list_extract(list(DISTINCT v ORDER BY v DESC) \
                            FILTER(keep AND v IS NOT NULL),1) FROM encoded_filter",
            )?
            .rows,
        vec![vec![
            Value::Integer(34),
            Value::Integer(138),
            Value::Integer(8),
            Value::Integer(9),
        ]],
    );

    let mut limited = DatabaseBuilder::new()
        .max_intermediate_rows(2)
        .batch_size(1)
        .build()?
        .connect();
    assert!(matches!(
        limited.query("SELECT list(DISTINCT i ORDER BY i) FILTER(i >= 0) FROM range(4) t(i)"),
        Err(Error::Resource(_))
    ));

    let slot = Arc::new(Mutex::new(None));
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(InterruptFilter(slot.clone())))?;
    let mut interrupted = DatabaseBuilder::new()
        .functions(functions)
        .batch_size(1)
        .build()?
        .connect();
    *slot.lock().unwrap() = Some(interrupted.interrupt_handle());
    assert!(matches!(
        interrupted.query(
            "SELECT list(DISTINCT i ORDER BY i) FILTER(interrupt_filter(i)) \
             FROM range(4) t(i)"
        ),
        Err(Error::Interrupted)
    ));
    assert_eq!(
        interrupted.query("SELECT 7")?.rows,
        vec![vec![Value::Integer(7)]]
    );
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
                    Value::Integer(8),
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
        // Equal extreme keys preserve their first source row for FIRST and
        // LAST, matching DuckDB's stable ordered-aggregate wrapper.
        assert_eq!(
            connection
                .query("SELECT first(v ORDER BY k),last(v ORDER BY k) FROM ordered WHERE g=3")?
                .rows,
            vec![vec![Value::Integer(8), Value::Integer(8)]],
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
        // Candidate accumulators have no input rows to retain, but their
        // result still has the one ungrouped aggregate row.
        assert_eq!(
            connection
                .query("SELECT first(v ORDER BY k),last(v ORDER BY k) FROM ordered WHERE false")?
                .rows,
            vec![vec![Value::Null, Value::Null]],
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
            .max_intermediate_rows(7)
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn string_agg_binds_constant_separators_and_composes_with_grouping_modifiers() -> Result<()> {
    for algorithm in algorithms() {
        let db = DatabaseBuilder::new()
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .batch_size(2)
            .build()?;
        let mut connection = db.connect();
        connection.execute(
            "CREATE TABLE strings(g INTEGER, x VARCHAR, keep BOOLEAN, k INTEGER); \
             INSERT INTO strings VALUES \
             (1,'b',true,2),(1,'a',true,1),(1,'a',false,3), \
             (2,'z',true,1),(2,NULL,true,2),(3,NULL,true,1)",
        )?;
        let result = connection.query(
            "SELECT g, string_agg(DISTINCT x, '|' ORDER BY x) FILTER (WHERE keep), \
                    group_concat(x ORDER BY k) \
             FROM strings GROUP BY g ORDER BY g",
        )?;
        assert_eq!(result.columns[1].data_type, DataType::Varchar);
        assert_eq!(
            result.rows,
            vec![
                vec![
                    Value::Integer(1),
                    Value::Varchar("a|b".into()),
                    Value::Varchar("a,b,a".into()),
                ],
                vec![
                    Value::Integer(2),
                    Value::Varchar("z".into()),
                    Value::Varchar("z".into()),
                ],
                vec![Value::Integer(3), Value::Null, Value::Null],
            ]
        );
        assert_eq!(
            connection
                .query("SELECT string_agg(x),string_agg(x, NULL) FROM strings WHERE g>9")?
                .rows,
            vec![vec![Value::Null, Value::Null]]
        );
        assert!(matches!(
            connection.query("SELECT string_agg(x, CAST(g AS VARCHAR)) FROM strings"),
            Err(Error::Bind(message)) if message == "string_agg argument 2 must be a constant expression"
        ));
        assert!(matches!(
            connection.query("SELECT string_agg(1, ',')"),
            Err(Error::Bind(message)) if message.contains("no overload")
        ));
    }
    Ok(())
}
