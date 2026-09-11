use super::support::ProbePlan;
use super::*;
use duckdb_rust::{
    execution::{
        operator::join::{HashJoin, JoinAlgorithm, JoinPlan},
        subquery::{PreparedSubqueries, StreamingSubqueries},
    },
    planner::{BoundExpr, ExprKind, expression::BinaryOp, logical::JoinKind},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn using_and_natural_joins_share_names_types_and_correlation_across_adapters() -> Result<()> {
    use duckdb_rust::{
        execution::{expression_executor::BatchedEvaluator, operator::join::NestedLoopJoin},
        optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for algorithm in [
        Arc::new(HashJoin) as Arc<dyn JoinAlgorithm>,
        Arc::new(NestedLoopJoin),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for batch_size in [1, 3, 2048] {
                for executor in executors() {
                    let db = DatabaseBuilder::new()
                        .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![
                            algorithm.clone(),
                            Arc::new(NestedLoopJoin),
                        ])))
                        .optimizer(optimizer.clone())
                        .executor(executor)
                        .expressions(Arc::new(BatchedEvaluator))
                        .batch_size(batch_size)
                        .build()?;
                    for corpus in ["using.test", "using-chain.test", "using-development.test"] {
                        runner::run_file(&db, &root.join("test/sql").join(corpus))?;
                    }
                    let mut connection = db.connect();
                    for (kind, merged) in [
                        ("INNER", DataType::Integer),
                        ("LEFT", DataType::Integer),
                        ("RIGHT", DataType::BigInt),
                        ("FULL", DataType::BigInt),
                    ] {
                        let result = connection.query(&format!("SELECT k,a.k,b.k FROM (VALUES(1::INTEGER))a(k) {kind} JOIN (VALUES(1::BIGINT))b(k) USING(k)"))?;
                        assert_eq!(
                            result
                                .columns
                                .iter()
                                .map(|field| field.data_type.clone())
                                .collect::<Vec<_>>(),
                            vec![merged, DataType::Integer, DataType::BigInt]
                        );
                        assert_eq!(result.rows, vec![ints(&[1, 1, 1])]);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn equality(query: &QueryContext) -> Result<BoundExpr> {
    Ok(BoundExpr {
        data_type: DataType::Boolean,
        kind: ExprKind::Binary(
            BinaryOp::Equal,
            Box::new(BoundExpr::column(0, DataType::BigInt)),
            Box::new(BoundExpr::column(1, DataType::BigInt)),
            query.types().bind(&DataType::BigInt)?.into(),
        ),
    })
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn join<'a>(
    left: &'a ProbePlan,
    right: &'a ProbePlan,
    condition: &'a BoundExpr,
    kind: JoinKind,
) -> JoinPlan<'a> {
    JoinPlan {
        left,
        right,
        kind,
        condition,
        schema: &left.schema,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn hash_outer_join_resumes_duplicates_checks_failures_and_keeps_owned_output() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let interrupt = InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 3, 8)?;
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
    let condition = equality(&query)?;
    let left = ProbePlan::new(&[1, 3]);
    let right = ProbePlan::new(&[1, 1, 1, 2]);
    let schema = left.schema.iter().chain(&right.schema).cloned().collect();
    let plan = || JoinPlan {
        left: &left,
        right: &right,
        kind: JoinKind::Full,
        condition: &condition,
        schema: &schema,
    };
    let mut first = HashJoin.open(plan(), &context)?;
    let mut second = HashJoin.open(plan(), &context)?;
    assert_eq!(left.reads.load(Ordering::Relaxed), 0);
    assert_eq!(right.reads.load(Ordering::Relaxed), 0);
    let retained = first.next(1)?.unwrap();
    assert_eq!(retained.rows().collect::<Vec<_>>(), vec![ints(&[1, 1])]);
    assert_eq!(left.reads.load(Ordering::Relaxed), 1);
    assert_eq!(right.reads.load(Ordering::Relaxed), 4);
    let mut result = retained.rows().collect::<Vec<_>>();
    while let Some(batch) = first.next(2)? {
        assert!(batch.len() <= 2);
        result.extend(batch.rows());
    }
    assert_eq!(
        result,
        vec![
            ints(&[1, 1]),
            ints(&[1, 1]),
            ints(&[1, 1]),
            vec![Value::Integer(3), Value::Null],
            vec![Value::Null, Value::Integer(2)]
        ]
    );
    assert!(first.next(0)?.is_none());
    assert_eq!(
        second.next(1)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[1, 1])]
    );
    interrupt.interrupt();
    assert!(matches!(second.next(1), Err(Error::Interrupted)));
    interrupt.reset();
    assert!(second.next(1)?.is_none());
    drop(first);
    drop(second);
    drop(left);
    drop(right);
    assert_eq!(retained.rows().collect::<Vec<_>>(), vec![ints(&[1, 1])]);
    for oversized in [false, true] {
        let left = ProbePlan::new(&[0]);
        let mut right = ProbePlan::new(&(0..9).collect::<Vec<_>>());
        right.invalid = !oversized;
        let mut cursor = HashJoin.open(
            JoinPlan {
                left: &left,
                right: &right,
                kind: JoinKind::Full,
                condition: &condition,
                schema: &schema,
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
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn hash_semi_join_builds_once_streams_demand_and_retains_owned_chunks() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let query = QueryContext::new(InterruptHandle::default(), None, 3, 100)?;
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
    let condition = equality(&query)?;
    let left = ProbePlan::new(&(0..10).collect::<Vec<_>>());
    let mut right = ProbePlan::new(&[0, 2, 2, 4]);
    right.rows.push(vec![Value::Null]);
    let mut first = HashJoin.open(join(&left, &right, &condition, JoinKind::Semi), &context)?;
    assert_eq!(left.reads.load(Ordering::Relaxed), 0);
    assert_eq!(right.opens.load(Ordering::Relaxed), 0);
    let retained = first.next(1)?.unwrap();
    assert_eq!(retained.rows().collect::<Vec<_>>(), vec![ints(&[0])]);
    assert_eq!(left.reads.load(Ordering::Relaxed), 1);
    assert_eq!(right.reads.load(Ordering::Relaxed), 5);
    assert_eq!(
        first.next(2)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[2])]
    );
    assert_eq!(left.reads.load(Ordering::Relaxed), 3);
    assert_eq!(right.opens.load(Ordering::Relaxed), 1);
    let mut second = HashJoin.open(join(&left, &right, &condition, JoinKind::Anti), &context)?;
    assert_eq!(
        second.next(1)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[1])]
    );
    assert_eq!(right.opens.load(Ordering::Relaxed), 2);
    assert_eq!(
        first.next(3)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[4])]
    );
    assert!(first.next(100)?.is_none());
    assert!(first.next(0)?.is_none());
    drop(first);
    drop(second);
    drop(left);
    drop(right);
    assert_eq!(retained.rows().collect::<Vec<_>>(), vec![ints(&[0])]);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn hash_semi_join_checks_build_schema_limits_cancellation_and_empty_outer() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let interrupt = InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2, 2)?;
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
    let condition = equality(&query)?;
    let empty = ProbePlan::new(&[]);
    let left = ProbePlan::new(&[0, 1, 2]);
    let mut invalid = ProbePlan::new(&[0]);
    invalid.invalid = true;
    let mut cursor = HashJoin.open(join(&empty, &invalid, &condition, JoinKind::Semi), &context)?;
    assert!(cursor.next(1)?.is_none());
    assert_eq!(invalid.opens.load(Ordering::Relaxed), 0);
    for kind in [JoinKind::Semi, JoinKind::Anti] {
        let mut cursor = HashJoin.open(join(&left, &invalid, &condition, kind), &context)?;
        assert!(matches!(cursor.next(1), Err(Error::Internal(_))));
        assert!(cursor.next(1)?.is_none());
        let oversized = ProbePlan::new(&[0, 1, 2]);
        let mut cursor = HashJoin.open(join(&left, &oversized, &condition, kind), &context)?;
        assert!(matches!(cursor.next(1), Err(Error::Resource(_))));
        assert!(cursor.next(1)?.is_none());
    }
    let right = ProbePlan::new(&[0, 1]);
    let mut cursor = HashJoin.open(join(&left, &right, &condition, JoinKind::Semi), &context)?;
    assert!(cursor.next(1)?.is_some());
    interrupt.interrupt();
    assert!(matches!(cursor.next(1), Err(Error::Interrupted)));
    interrupt.reset();
    assert!(cursor.next(1)?.is_none());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn membership_joins_match_nested_loops_across_domains_and_batch_sizes() -> Result<()> {
    use duckdb_rust::execution::operator::join::NestedLoopJoin;
    let minimum = i128::MIN;
    let maximum = i128::MAX;
    let cases = [
        // Negative offsets, gaps, duplicates, and NULLs on both sides.
        (
            vec![-70, -69, -7, -6, 0, 1, 1, 9, 63, 64, 65],
            vec![-69, -7, 0, 1, 1, 64],
        ),
        // Sparse integer keys must not allocate an array proportional to range.
        (vec![minimum, -1, 0, 1, maximum], vec![minimum, 1, maximum]),
        // Compact domains at either HUGEINT bound expose offset wraparound.
        (
            vec![minimum, minimum + 1, maximum - 1, maximum],
            vec![maximum - 1, maximum],
        ),
        (
            vec![minimum, minimum + 1, maximum - 1, maximum],
            vec![minimum, minimum + 1],
        ),
        (vec![0, 1], vec![]),
        (vec![], vec![0, 1]),
    ];
    for (left, right) in cases {
        for include_null in [false, true] {
            let mut expected = None;
            for algorithm in [
                Arc::new(NestedLoopJoin) as Arc<dyn JoinAlgorithm>,
                Arc::new(HashJoin),
            ] {
                for batch_size in [1, 3, 2048] {
                    let db = DatabaseBuilder::new()
                        .batch_size(batch_size)
                        .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![
                            algorithm.clone(),
                        ])))
                        .build()?;
                    let mut c = db.connect();
                    c.execute("CREATE TABLE l(i HUGEINT); CREATE TABLE r(i HUGEINT)")?;
                    for (name, values) in [("l", &left), ("r", &right)] {
                        for value in values {
                            c.execute(&format!("INSERT INTO {name} VALUES ('{value}'::HUGEINT)"))?;
                        }
                        if include_null {
                            c.execute(&format!("INSERT INTO {name} VALUES (NULL)"))?;
                        }
                    }
                    let mut results = Vec::new();
                    for predicate in ["EXISTS", "NOT EXISTS"] {
                        results.push(
                            c.query(&format!(
                                "SELECT i FROM l WHERE {predicate}(SELECT 1 FROM r WHERE l.i=r.i)"
                            ))?
                            .rows,
                        );
                    }
                    if let Some(expected) = &expected {
                        assert_eq!(
                            &results,
                            expected,
                            "{} batch={batch_size}",
                            algorithm.name()
                        );
                    } else {
                        expected = Some(results);
                    }
                }
            }
        }
    }
    Ok(())
}
