use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    common::type_registry::{
        KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry, ValueValidation,
    },
    execution::{
        Executor, MaterializingExecutor, PullExecutor,
        expression_executor::{
            BatchedEvaluator, EvaluationContext, ExpressionEvaluator, ScalarEvaluator,
        },
        operator::aggregate::{AggregationAlgorithm, HashAggregation, OrderedAggregation},
        physical_plan::NativePhysicalPlanner,
    },
    function::{
        AggregateBinding, AggregateFunction, AggregateModifierStrategy, AggregateState,
        FunctionEffects, FunctionRegistry, ScalarFunction,
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::{InterruptHandle, QueryContext},
    planner::{
        BoundExpr, ExprKind, Field, LogicalPlan, PlanNode,
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
        assert_eq!(
            connection
                .query("SELECT listagg(x, '|') FROM strings WHERE g=1")?
                .rows,
            vec![vec![Value::Varchar("b|a|a".into())]]
        );
        assert!(matches!(
            connection.query("SELECT string_agg(x, CAST(g AS VARCHAR)) FROM strings"),
            Err(Error::Bind(message)) if message == "Separator argument to string_agg must be a constant expression"
        ));
        assert!(matches!(
            connection.query("SELECT string_agg(1, ',')"),
            Err(Error::Bind(message)) if message.contains("No function matches")
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aggregate_order_literals_match_the_default_pinned_rule() -> Result<()> {
    for algorithm in algorithms() {
        let mut connection = DatabaseBuilder::new()
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .build()?
            .connect();
        for sql in [
            "SELECT sum(i ORDER BY 1) FROM range(3) t(i)",
            "SELECT list(i ORDER BY -1) FROM range(3) t(i)",
            "SELECT string_agg(i::VARCHAR ORDER BY +1) FROM range(3) t(i)",
            "SELECT string_agg(i::VARCHAR ORDER BY 1+0) FROM range(3) t(i)",
            "SELECT list(i ORDER BY 1.0+0.0) FROM range(3) t(i)",
            "SELECT string_agg(i::VARCHAR ORDER BY '_'||'') FROM range(3) t(i)",
            "SELECT sum(i ORDER BY -i) FROM range(3) t(i)",
            "SELECT list(i ORDER BY +(i+1)) FROM range(3) t(i)",
            "SELECT string_agg(i::VARCHAR ORDER BY (1)) FROM range(3) t(i)",
            "SELECT list(i ORDER BY -(1.5)) FROM range(3) t(i)",
        ] {
            connection.query(sql)?;
        }
        for sql in [
            "SELECT sum(i ORDER BY 1.5) FROM range(3) t(i)",
            "SELECT list(i ORDER BY '_') FROM range(3) t(i)",
            "SELECT string_agg(i::VARCHAR ORDER BY NULL) FROM range(3) t(i)",
            "SELECT sum(i ORDER BY true) FROM range(3) t(i)",
            "SELECT list(i ORDER BY 340282366920938463463374607431768211456) FROM range(3) t(i)",
            "SELECT string_agg(i::VARCHAR ORDER BY ('_')) FROM range(3) t(i)",
            "SELECT list(i ORDER BY (1.5)) FROM range(3) t(i)",
        ] {
            assert!(matches!(
                connection.query(sql),
                Err(Error::Bind(message)) if message == "ORDER BY non-integer literal has no effect"
            ));
        }
    }
    Ok(())
}

struct BufferedProbe {
    name: &'static str,
    strategy: AggregateModifierStrategy,
    row_calls: Arc<AtomicUsize>,
    batch_calls: Arc<AtomicUsize>,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
}

impl std::fmt::Debug for BufferedProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BufferedProbe")
            .field("name", &self.name)
            .field("strategy", &self.strategy)
            .finish_non_exhaustive()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for BufferedProbe {
    fn name(&self) -> &str {
        self.name
    }
    fn argument_types(&self, arguments: &[DataType]) -> Result<Vec<DataType>> {
        if arguments == [DataType::Varchar] {
            Ok(arguments.to_vec())
        } else {
            Err(Error::Bind("buffered probe requires VARCHAR".into()))
        }
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        self.argument_types(arguments)?;
        Ok(DataType::Varchar)
    }
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        self.return_type(arguments, types)?;
        Ok(Box::new(BufferedProbeState {
            values: Vec::new(),
            row_calls: self.row_calls.clone(),
            batch_calls: self.batch_calls.clone(),
            interrupt: self.interrupt.clone(),
        }))
    }
    fn modifier_strategy(&self, _: &[DataType]) -> AggregateModifierStrategy {
        self.strategy
    }
}

struct BufferedProbeState {
    values: Vec<String>,
    row_calls: Arc<AtomicUsize>,
    batch_calls: Arc<AtomicUsize>,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateState for BufferedProbeState {
    fn update(&mut self, arguments: &[Value], query: &QueryContext) -> Result<()> {
        query.check()?;
        self.row_calls.fetch_add(1, Ordering::SeqCst);
        let [value] = arguments else {
            return Err(Error::Internal("buffered probe argument count".into()));
        };
        self.values.push(match value {
            Value::Varchar(value) => value.clone(),
            Value::Null => "NULL".into(),
            _ => return Err(Error::Internal("buffered probe argument type".into())),
        });
        Ok(())
    }
    fn update_batch(
        &mut self,
        arguments: &duckdb_rust::common::vector::DataChunk,
        query: &QueryContext,
    ) -> Result<()> {
        self.batch_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(interrupt) = self.interrupt.lock().unwrap().take() {
            interrupt.interrupt();
            query.check()?;
        }
        for row in arguments.rows() {
            self.update(&row, query)?;
        }
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(Value::Varchar(self.values.join("|")))
    }
}

#[derive(Debug)]
struct SignedBufferedProbe(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for SignedBufferedProbe {
    fn name(&self) -> &str {
        "signed_buffered_probe"
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if matches!(arguments, [data_type] if data_type.is_signed_integer()) {
            Ok(DataType::Varchar)
        } else {
            Err(Error::Bind(
                "signed buffered probe requires an integer".into(),
            ))
        }
    }
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        self.return_type(arguments, types)?;
        Ok(Box::new(SignedBufferedProbeState {
            values: Vec::new(),
            batch_calls: self.0.clone(),
        }))
    }
    fn modifier_strategy(&self, _: &[DataType]) -> AggregateModifierStrategy {
        AggregateModifierStrategy::BufferedTotal
    }
}

struct SignedBufferedProbeState {
    values: Vec<i128>,
    batch_calls: Arc<AtomicUsize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateState for SignedBufferedProbeState {
    fn update(&mut self, arguments: &[Value], query: &QueryContext) -> Result<()> {
        query.check()?;
        let [Value::Integer(value)] = arguments else {
            return Err(Error::Internal("signed buffered probe argument".into()));
        };
        self.values.push(*value);
        Ok(())
    }
    fn update_batch(
        &mut self,
        arguments: &duckdb_rust::common::vector::DataChunk,
        query: &QueryContext,
    ) -> Result<()> {
        self.batch_calls.fetch_add(1, Ordering::SeqCst);
        for row in arguments.rows() {
            self.update(&row, query)?;
        }
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(Value::Varchar(
            self.values
                .iter()
                .map(i128::to_string)
                .collect::<Vec<_>>()
                .join("|"),
        ))
    }
}

#[derive(Debug)]
struct ComparisonIntegers;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for ComparisonIntegers {
    fn name(&self) -> &'static str {
        "comparison-integers"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(data_type)
    }
    fn validate_value(
        &self,
        data_type: &DataType,
        value: &Value,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.validate_value(data_type, value, query)
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(left, right)
    }
    fn compare(
        &self,
        data_type: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<std::cmp::Ordering> {
        PrimitiveTypes.compare(data_type, left, right, query)
    }
    fn write_key(
        &self,
        data_type: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(data_type, value, output, query)
    }
}

#[derive(Debug)]
struct CaseFoldVarchar;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for CaseFoldVarchar {
    fn name(&self) -> &'static str {
        "case-fold-varchar"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        if data_type == &DataType::Varchar {
            Ok(())
        } else {
            Err(Error::Bind("case-fold adapter requires VARCHAR".into()))
        }
    }
    fn validate_value(
        &self,
        data_type: &DataType,
        value: &Value,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.validate_value(data_type, value, query)
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(left, right)
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<std::cmp::Ordering> {
        query.check()?;
        let (Value::Varchar(left), Value::Varchar(right)) = (left, right) else {
            return Err(Error::Internal("case-fold comparison input".into()));
        };
        Ok(left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()))
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        let Value::Varchar(value) = value else {
            return Err(Error::Internal("case-fold key input".into()));
        };
        output.extend_from_slice(value.to_ascii_lowercase().as_bytes())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn total_buffered_modifiers_preserve_types_order_identity_and_boundaries() -> Result<()> {
    let row_calls = Arc::new(AtomicUsize::new(0));
    let batch_calls = Arc::new(AtomicUsize::new(0));
    let interrupt = Arc::new(Mutex::new(None));
    let signed_batch_calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::builtins();
    functions.register_aggregate(Arc::new(BufferedProbe {
        name: "buffered_probe",
        strategy: AggregateModifierStrategy::BufferedTotal,
        row_calls: row_calls.clone(),
        batch_calls: batch_calls.clone(),
        interrupt: interrupt.clone(),
    }))?;
    functions.register_aggregate(Arc::new(BufferedProbe {
        name: "effectful_probe",
        strategy: AggregateModifierStrategy::Generic,
        row_calls: row_calls.clone(),
        batch_calls: batch_calls.clone(),
        interrupt: Arc::new(Mutex::new(None)),
    }))?;
    functions.register_aggregate(Arc::new(SignedBufferedProbe(signed_batch_calls.clone())))?;
    let database = DatabaseBuilder::new()
        .functions(functions.clone())
        .batch_size(2)
        .physical_planner(Arc::new(
            NativePhysicalPlanner::default().with_aggregation(Arc::new(HashAggregation)),
        ))
        .build()?;
    let mut connection = database.connect();
    assert_eq!(
        connection
            .query(
                "SELECT buffered_probe(DISTINCT x ORDER BY x DESC NULLS FIRST) \
                 FROM (VALUES ('b'),(NULL),(''),('a'),('b'),(NULL),('')) t(x)",
            )?
            .rows,
        vec![vec![Value::Varchar("NULL|b|a|".into())]]
    );
    assert_eq!(
        connection
            .query(
                "SELECT buffered_probe(x ORDER BY k) \
                 FROM (VALUES ('a',1),('b',1),('c',0)) t(x,k)",
            )?
            .rows,
        vec![vec![Value::Varchar("c|a|b".into())]]
    );
    assert_eq!(batch_calls.load(Ordering::SeqCst), 2);
    assert_eq!(row_calls.load(Ordering::SeqCst), 7);
    assert_eq!(
        connection
            .query(
                "SELECT list_extract(list(x ORDER BY k),1), \
                        list_extract(list(x ORDER BY k),2), \
                        list_extract(list(x ORDER BY k),3), \
                        list_extract(list(x ORDER BY k),4) \
                 FROM (VALUES ('a',1),('b',1),('c',0),(NULL,-1)) t(x,k)",
            )?
            .rows,
        vec![vec![
            Value::Null,
            Value::Varchar("c".into()),
            Value::Varchar("a".into()),
            Value::Varchar("b".into()),
        ]]
    );
    assert_eq!(
        connection
            .query(
                "SELECT signed_buffered_probe(DISTINCT i ORDER BY i DESC) \
                 FROM (VALUES (2),(1),(2)) t(i)",
            )?
            .rows,
        vec![vec![Value::Varchar("2|1".into())]]
    );
    assert_eq!(signed_batch_calls.load(Ordering::SeqCst), 0);

    let custom_batch_calls = Arc::new(AtomicUsize::new(0));
    let mut custom_functions = FunctionRegistry::builtins();
    custom_functions
        .register_aggregate(Arc::new(SignedBufferedProbe(custom_batch_calls.clone())))?;
    let mut custom_types = TypeRegistry::builtins();
    custom_types.replace(DataType::Integer.family(), Arc::new(ComparisonIntegers))?;
    let mut custom = DatabaseBuilder::new()
        .functions(custom_functions)
        .types(Arc::new(custom_types))
        .build()?
        .connect();
    assert_eq!(
        custom
            .query(
                "SELECT signed_buffered_probe(DISTINCT i ORDER BY i DESC) \
                 FROM (VALUES (2),(1),(2)) t(i)",
            )?
            .rows,
        vec![vec![Value::Varchar("2|1".into())]]
    );
    assert_eq!(custom_batch_calls.load(Ordering::SeqCst), 1);

    let folded_batch_calls = Arc::new(AtomicUsize::new(0));
    let mut folded_functions = FunctionRegistry::builtins();
    folded_functions.register_aggregate(Arc::new(BufferedProbe {
        name: "folded_probe",
        strategy: AggregateModifierStrategy::BufferedTotal,
        row_calls: Arc::new(AtomicUsize::new(0)),
        batch_calls: folded_batch_calls.clone(),
        interrupt: Arc::new(Mutex::new(None)),
    }))?;
    let mut folded_types = TypeRegistry::builtins();
    folded_types.replace(DataType::Varchar.family(), Arc::new(CaseFoldVarchar))?;
    let mut folded = DatabaseBuilder::new()
        .functions(folded_functions)
        .types(Arc::new(folded_types))
        .build()?
        .connect();
    assert_eq!(
        folded
            .query(
                "SELECT folded_probe(DISTINCT x ORDER BY x) \
                 FROM (VALUES ('A'),('a'),('B')) t(x)",
            )?
            .rows,
        vec![vec![Value::Varchar("A|B".into())]]
    );
    assert_eq!(folded_batch_calls.load(Ordering::SeqCst), 1);

    let before_batches = batch_calls.load(Ordering::SeqCst);
    assert_eq!(
        connection
            .query(
                "SELECT effectful_probe(x ORDER BY k) \
                 FROM (VALUES ('a',1),('b',1),('c',0)) t(x,k)",
            )?
            .rows,
        vec![vec![Value::Varchar("c|a|b".into())]]
    );
    assert_eq!(batch_calls.load(Ordering::SeqCst), before_batches);

    let mut limited = DatabaseBuilder::new()
        .functions(functions.clone())
        .batch_size(2)
        .max_intermediate_rows(5)
        .build()?
        .connect();
    assert!(matches!(
        limited.query("SELECT buffered_probe(x ORDER BY x) FROM (VALUES ('a'),('b')) t(x)"),
        Err(Error::Resource(_))
    ));

    let mut cancelled = DatabaseBuilder::new()
        .functions(functions)
        .batch_size(2)
        .build()?
        .connect();
    *interrupt.lock().unwrap() = Some(cancelled.interrupt_handle());
    assert!(matches!(
        cancelled.query("SELECT buffered_probe(x ORDER BY x) FROM (VALUES ('a'),('b')) t(x)"),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn string_agg_default_configuration_replays_pinned_source_records() -> Result<()> {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test/sql/string-agg-default-source.test");
    for algorithm in algorithms() {
        let database = DatabaseBuilder::new()
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .build()?;
        assert_eq!(runner::run_file(&database, &source)?, 29);
    }
    Ok(())
}

#[derive(Debug)]
struct ConstantSeparator(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ConstantSeparator {
    fn name(&self) -> &str {
        "constant_separator"
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments.is_empty() {
            Ok(DataType::Varchar)
        } else {
            Err(Error::Bind("constant_separator takes no arguments".into()))
        }
    }
    fn evaluate(&self, _: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Varchar("|".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn string_agg_captures_constants_and_skips_input_for_null_separator() -> Result<()> {
    for algorithm in algorithms() {
        let input_calls = Arc::new(AtomicUsize::new(0));
        let separator_calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(Observe(input_calls.clone())))?;
        functions.register_scalar(Arc::new(ConstantSeparator(separator_calls.clone())))?;
        let mut connection = DatabaseBuilder::new()
            .functions(functions)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_aggregation(algorithm),
            ))
            .build()?
            .connect();
        assert_eq!(
            connection
                .query(
                    "SELECT string_agg(x, '|' ORDER BY k), \
                            string_agg(x, '' ORDER BY k), \
                            string_agg(observe(x), NULL) \
                     FROM (VALUES (0,NULL::VARCHAR),(1,''),(2,'a'),(3,NULL),(4,'b')) t(k,x)",
                )?
                .rows,
            vec![vec![
                Value::Varchar("|a|b".into()),
                Value::Varchar("ab".into()),
                Value::Null,
            ]]
        );
        assert_eq!(input_calls.load(Ordering::SeqCst), 0);

        let prepared = connection.prepare(
            "SELECT string_agg(x, constant_separator() ORDER BY k) \
             FROM (VALUES (1,'a'),(2,'b')) t(k,x)",
        )?;
        for _ in 0..2 {
            assert_eq!(
                connection.execute_prepared(&prepared, &[])?.rows,
                vec![vec![Value::Varchar("a|b".into())]]
            );
        }
        assert_eq!(separator_calls.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum InvalidAggregateBindingKind {
    ConstantIndex,
    ReplacementIndex,
    ReplacementType,
    RetainedIndex,
}

#[derive(Debug)]
struct InvalidAggregateBinding {
    source: Arc<dyn AggregateFunction>,
    kind: InvalidAggregateBindingKind,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for InvalidAggregateBinding {
    fn name(&self) -> &str {
        match self.kind {
            InvalidAggregateBindingKind::ConstantIndex => "invalid_aggregate_constant_index",
            InvalidAggregateBindingKind::ReplacementIndex => "invalid_aggregate_replacement_index",
            InvalidAggregateBindingKind::ReplacementType => "invalid_aggregate_replacement_type",
            InvalidAggregateBindingKind::RetainedIndex => "invalid_aggregate_retained_index",
        }
    }
    fn constant_arguments(&self, _: usize) -> &[usize] {
        if matches!(self.kind, InvalidAggregateBindingKind::ConstantIndex) {
            &[1]
        } else {
            &[]
        }
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        types: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        self.source.return_type(arguments, types)
    }
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        self.source.create_state(arguments, types)
    }
    fn bind(&self, _: &[Option<Value>]) -> Result<Option<AggregateBinding>> {
        let retain_arguments = if matches!(self.kind, InvalidAggregateBindingKind::RetainedIndex) {
            vec![1]
        } else {
            vec![0]
        };
        let replacements = match self.kind {
            InvalidAggregateBindingKind::ReplacementIndex => vec![(1, Value::Integer(0))],
            InvalidAggregateBindingKind::ReplacementType => {
                vec![(0, Value::Varchar("wrong".into()))]
            }
            _ => Vec::new(),
        };
        Ok(Some(AggregateBinding {
            function: self.source.clone(),
            retain_arguments,
            replacements,
        }))
    }
}

#[derive(Debug)]
struct InvalidStringConstant;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for InvalidStringConstant {
    fn name(&self) -> &'static str {
        "invalid-string-constant"
    }
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &duckdb_rust::common::Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        if matches!(&expression.kind, ExprKind::Literal(Value::Varchar(value)) if value == "|") {
            Ok(Value::Integer(7))
        } else {
            ScalarEvaluator.evaluate(expression, row, context)
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aggregate_binding_metadata_rejects_invalid_indices_and_typed_literals() -> Result<()> {
    let mut functions = FunctionRegistry::builtins();
    let sum = functions.aggregate("sum").expect("builtin SUM");
    for kind in [
        InvalidAggregateBindingKind::ConstantIndex,
        InvalidAggregateBindingKind::ReplacementIndex,
        InvalidAggregateBindingKind::ReplacementType,
        InvalidAggregateBindingKind::RetainedIndex,
    ] {
        functions.register_aggregate(Arc::new(InvalidAggregateBinding {
            source: sum.clone(),
            kind,
        }))?;
    }
    let mut connection = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert!(
        matches!(connection.query("SELECT invalid_aggregate_constant_index(i) FROM range(1) t(i)"), Err(Error::Internal(message)) if message == "aggregate constant argument outside signature")
    );
    assert!(
        matches!(connection.query("SELECT invalid_aggregate_replacement_index(i) FROM range(1) t(i)"), Err(Error::Internal(message)) if message == "aggregate replacement outside signature")
    );
    assert!(matches!(
        connection.query("SELECT invalid_aggregate_replacement_type(i) FROM range(1) t(i)"),
        Err(Error::Conversion(_))
    ));
    assert!(
        matches!(connection.query("SELECT invalid_aggregate_retained_index(i) FROM range(1) t(i)"), Err(Error::Internal(message)) if message == "aggregate retained argument outside signature")
    );

    let mut invalid_constant = DatabaseBuilder::new()
        .expressions(Arc::new(InvalidStringConstant))
        .build()?
        .connect();
    assert!(
        matches!(invalid_constant.query("SELECT string_agg(x, '|') FROM (VALUES ('a')) t(x)"), Err(Error::Internal(message)) if message == "constant evaluator returned an invalid value")
    );
    Ok(())
}
