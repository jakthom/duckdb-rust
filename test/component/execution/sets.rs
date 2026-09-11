use super::support::ProbePlan;
use super::*;
use duckdb_rust::{
    execution::{
        operator::set::{HashSetOperations, OrderedSetOperations, SetAlgorithm, SetPlan},
        subquery::{PreparedSubqueries, StreamingSubqueries},
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    planner::logical::SetOperation,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn algorithms() -> Vec<Arc<dyn SetAlgorithm>> {
    vec![Arc::new(HashSetOperations), Arc::new(OrderedSetOperations)]
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn set_cursors_preserve_demand_failures_limits_and_retained_results() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let interrupt = InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2, 8)?;
    let planner = NativePhysicalPlanner::default();
    let context = ExecutionContext {
        transaction: tx.as_ref(),
        expressions: &ScalarEvaluator,
        query: &query,
        subquery_plans: &PreparedSubqueries::new(&planner),
        subqueries: &StreamingSubqueries,
        outer: None,
        recursive: None,
    };
    for algorithm in algorithms() {
        let left = ProbePlan::new(&[1, 1, 2, 3, 4]);
        let right = ProbePlan::new(&[1, 1, 1, 2]);
        let open = |kind, all| {
            algorithm.open(
                SetPlan {
                    left: &left,
                    right: &right,
                    kind,
                    all,
                    schema: &left.schema,
                },
                &context,
            )
        };
        let mut first = open(SetOperation::Intersect, true)?;
        assert_eq!(left.reads.load(Ordering::Relaxed), 0);
        assert_eq!(right.reads.load(Ordering::Relaxed), 0);
        let retained = first.next(1)?.unwrap();
        assert_eq!(retained.rows().collect::<Vec<_>>(), vec![ints(&[1])]);
        assert_eq!(left.reads.load(Ordering::Relaxed), 1);
        assert_eq!(right.reads.load(Ordering::Relaxed), 4);
        let mut second = open(SetOperation::Except, true)?;
        assert_eq!(
            second.next(1)?.unwrap().rows().collect::<Vec<_>>(),
            vec![ints(&[3])]
        );
        assert_eq!(
            first.next(2)?.unwrap().rows().collect::<Vec<_>>(),
            vec![ints(&[1]), ints(&[2])]
        );
        assert!(first.next(2)?.is_none());
        assert!(first.next(0)?.is_none());
        interrupt.interrupt();
        assert!(matches!(second.next(1), Err(Error::Interrupted)));
        interrupt.reset();
        assert!(second.next(1)?.is_none());
        drop(first);
        drop(second);
        drop(left);
        drop(right);
        assert_eq!(retained.rows().collect::<Vec<_>>(), vec![ints(&[1])]);

        let left = ProbePlan::new(&[1]);
        for oversized in [false, true] {
            let mut right = ProbePlan::new(&(0..9).collect::<Vec<_>>());
            right.invalid = !oversized;
            let mut cursor = algorithm.open(
                SetPlan {
                    left: &left,
                    right: &right,
                    kind: SetOperation::Except,
                    all: false,
                    schema: &left.schema,
                },
                &context,
            )?;
            let failure = cursor.next(1);
            if oversized {
                assert!(matches!(failure, Err(Error::Resource(_))));
            } else {
                assert!(matches!(failure, Err(Error::Internal(_))));
            }
            assert!(cursor.next(1)?.is_none());
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn exhausted_intersection_preserves_later_input_errors_and_effects() -> Result<()> {
    for algorithm in algorithms() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(CountCalls(calls.clone())))?;
        let mut c = DatabaseBuilder::new()
            .batch_size(1)
            .functions(functions)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_sets(algorithm),
            ))
            .build()?
            .connect();
        let result = c.query("SELECT count_calls(i) FROM range(4)t(i) INTERSECT SELECT 0")?;
        assert_eq!(result.rows, vec![ints(&[0])]);
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        assert!(matches!(
            c.query("SELECT CAST(v AS INTEGER) FROM (VALUES('0'),('bad'))t(v) INTERSECT SELECT 0"),
            Err(Error::Conversion(_))
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn set_adapters_share_sql_multiset_types_scope_and_composition_contracts() -> Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for algorithm in algorithms() {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for executor in executors() {
                for batch_size in [1, 3, 2048] {
                    let db = DatabaseBuilder::new()
                        .physical_planner(Arc::new(
                            NativePhysicalPlanner::default().with_sets(algorithm.clone()),
                        ))
                        .optimizer(optimizer.clone())
                        .executor(executor.clone())
                        .batch_size(batch_size)
                        .build()?;
                    assert!(
                        db.adapters()
                            .contains(&("set_operations", algorithm.name()))
                    );
                    for file in [
                        "set-operations.test",
                        "nested-except.test",
                        "union-except-empty.test",
                    ] {
                        runner::run_file(&db, &root.join("test/sql").join(file))?;
                    }
                    let mut c = db.connect();
                    for op in ["UNION", "INTERSECT", "EXCEPT"] {
                        let result = c.query(&format!(
                            "SELECT 1::INTEGER AS x {op} SELECT 1::BIGINT AS y"
                        ))?;
                        assert_eq!(result.columns[0].data_type, DataType::BigInt);
                        assert_eq!(result.columns[0].name, "x");
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn set_multiplicities_match_independent_counts_and_prepared_queries_rebind() -> Result<()> {
    for algorithm in algorithms() {
        let db = DatabaseBuilder::new()
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_sets(algorithm),
            ))
            .batch_size(3)
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE a(k INTEGER); CREATE TABLE b(k INTEGER)")?;
        let intersect = c.prepare("SELECT k FROM a INTERSECT ALL SELECT k FROM b ORDER BY k")?;
        let except = c.prepare("SELECT k FROM a EXCEPT ALL SELECT k FROM b ORDER BY k")?;
        for trial in 0..12 {
            c.execute("DELETE FROM a; DELETE FROM b")?;
            let mut expected_intersect = Vec::new();
            let mut expected_except = Vec::new();
            for index in 0..9 {
                let a = (trial * 7 + index * 3) % 5;
                let b = (trial * 2 + index * 7) % 6;
                let value = if index == 8 {
                    Value::Null
                } else {
                    Value::Integer(index - 4)
                };
                let sql = if value.is_null() {
                    "NULL".into()
                } else {
                    (index - 4).to_string()
                };
                for _ in 0..a {
                    c.execute(&format!("INSERT INTO a VALUES ({sql})"))?;
                }
                for _ in 0..b {
                    c.execute(&format!("INSERT INTO b VALUES ({sql})"))?;
                }
                expected_intersect.extend((0..a.min(b)).map(|_| vec![value.clone()]));
                expected_except.extend((0..(a - b).max(0)).map(|_| vec![value.clone()]));
            }
            assert_eq!(
                c.execute_prepared(&intersect, &[])?.rows,
                expected_intersect
            );
            assert_eq!(c.execute_prepared(&except, &[])?.rows, expected_except);
        }
    }
    Ok(())
}
