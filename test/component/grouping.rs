use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
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
#[path = "../runner/mod.rs"]
mod runner;

fn algorithms() -> Vec<Arc<dyn AggregationAlgorithm>> {
    vec![Arc::new(HashAggregation), Arc::new(OrderedAggregation)]
}

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
