use std::{sync::Arc, time::Duration};

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    execution::{
        Executor, MaterializingExecutor, PullExecutor, StreamControl,
        expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
        operator::recursive::{MaterializingRecursion, RecursiveAlgorithm, StreamingRecursion},
        physical_plan::NativePhysicalPlanner,
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    planner::{BoundExpr, BoundStatement, Field, LogicalPlan, PlanNode, RecursiveId},
};

#[path = "../runner/mod.rs"]
mod runner;

#[test]
fn recursive_algorithms_share_scope_iteration_and_union_contracts() -> Result<()> {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/sql/recursive.test");
    for recursion in [
        Arc::new(StreamingRecursion) as Arc<dyn RecursiveAlgorithm>,
        Arc::new(MaterializingRecursion),
    ] {
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
                                NativePhysicalPlanner::default().with_recursion(recursion.clone()),
                            ))
                            .optimizer(optimizer.clone())
                            .expressions(expressions.clone())
                            .executor(executor.clone())
                            .batch_size(batch_size)
                            .build()?;
                        assert!(db.adapters().contains(&("recursion", recursion.name())));
                        assert!(runner::run_file(&db, &corpus)? >= 15);
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn recursive_limit_cancellation_and_owned_results() -> Result<()> {
    for batch_size in [1, 3, 2048] {
        let db = DatabaseBuilder::new().batch_size(batch_size).build()?;
        let mut connection = db.connect();
        connection.set_timeout(Some(Duration::from_secs(5)));
        let query = "WITH RECURSIVE t(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM t) SELECT i FROM t WHERE i%20=0 LIMIT 10";
        assert_eq!(
            connection.query(query)?.rows,
            (0..10)
                .map(|i| vec![Value::Integer(i * 20)])
                .collect::<Vec<_>>()
        );
        let mut retained = Vec::new();
        connection.query_batches(
            "WITH RECURSIVE t(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM t) SELECT * FROM t",
            |_, batch| {
                retained.push(batch);
                Ok(if retained.len() == 4 {
                    StreamControl::Stop
                } else {
                    StreamControl::Continue
                })
            },
        )?;
        drop(connection);
        drop(db);
        assert_eq!(
            retained
                .iter()
                .flat_map(|batch| batch.rows())
                .collect::<Vec<_>>(),
            (1..=4).map(|i| vec![Value::Integer(i)]).collect::<Vec<_>>()
        );
    }
    let db = Database::memory()?;
    let mut connection = db.connect();
    connection.set_timeout(Some(Duration::from_millis(20)));
    assert!(matches!(
        connection.query(
            "WITH RECURSIVE t(i) AS (SELECT 1 UNION ALL SELECT i FROM t) SELECT count(*) FROM t"
        ),
        Err(Error::Interrupted)
    ));
    connection.set_timeout(None);
    assert_eq!(
        connection.query("SELECT 42")?.rows,
        vec![vec![Value::Integer(42)]]
    );
    Ok(())
}

#[test]
fn recursive_resource_limits_bound_retained_state_and_failures_abort_writes() -> Result<()> {
    let db = DatabaseBuilder::new().max_intermediate_rows(8).build()?;
    let mut connection = db.connect();
    for query in [
        "WITH RECURSIVE t(i) AS (SELECT 1 UNION SELECT i+1 FROM t WHERE i<20) SELECT count(*) FROM t",
        "WITH RECURSIVE t(i) AS (SELECT range FROM range(9) UNION ALL SELECT i+1 FROM t WHERE false) SELECT count(*) FROM t",
    ] {
        assert!(
            matches!(connection.query(query), Err(Error::Resource(_))),
            "{query}"
        );
    }
    connection.execute("CREATE TABLE result(i TINYINT); INSERT INTO result VALUES(7)")?;
    connection.execute("BEGIN")?;
    assert!(matches!(connection.execute("INSERT INTO result WITH RECURSIVE t(i) AS (SELECT 126::TINYINT UNION ALL SELECT i+1 FROM t) SELECT * FROM t"), Err(Error::Conversion(_)) | Err(Error::Execution(_))));
    assert!(connection.query("SELECT * FROM result").is_err());
    connection.execute("ROLLBACK")?;
    assert_eq!(
        connection.query("SELECT * FROM result")?.rows,
        vec![vec![Value::Integer(7)]]
    );
    Ok(())
}

#[test]
fn prepared_recursive_queries_rebind_and_preserve_snapshot_visibility() -> Result<()> {
    let db = Database::memory()?;
    let mut writer = db.connect();
    writer.execute("CREATE TABLE bound(i INTEGER); INSERT INTO bound VALUES(3)")?;
    let mut reader = db.connect();
    reader.execute("BEGIN")?;
    let query = "WITH RECURSIVE t(i) AS (SELECT $1 UNION ALL SELECT i+1 FROM t WHERE i < (SELECT max(i) FROM bound)) SELECT * FROM t ORDER BY i";
    let prepared = writer.prepare(query)?;
    assert_eq!(
        writer
            .execute_prepared(&prepared, &[Value::Integer(2)])?
            .rows,
        vec![vec![Value::Integer(2)], vec![Value::Integer(3)]]
    );
    assert_eq!(
        reader.query("SELECT count(*) FROM bound")?.rows,
        vec![vec![Value::Integer(1)]]
    );
    writer.execute("INSERT INTO bound VALUES(4)")?;
    assert_eq!(
        writer
            .execute_prepared(&prepared, &[Value::Integer(3)])?
            .rows,
        vec![vec![Value::Integer(3)], vec![Value::Integer(4)]]
    );
    let reader_prepared = reader.prepare(query)?;
    assert_eq!(
        reader
            .execute_prepared(&reader_prepared, &[Value::Integer(3)])?
            .rows,
        vec![vec![Value::Integer(3)]]
    );
    reader.execute("COMMIT")?;
    Ok(())
}

#[test]
fn recursive_plan_bindings_are_validated_before_execution() -> Result<()> {
    let db = Database::memory()?;
    let id = RecursiveId::default();
    let schema = vec![Field::new("i", DataType::BigInt)];
    let input = LogicalPlan {
        schema: schema.clone(),
        node: PlanNode::RecursiveInput(id.clone()),
    };
    assert!(matches!(
        db.connect()
            .execute_plan(BoundStatement::Query(input.clone())),
        Err(Error::Bind(_))
    ));
    let seed = LogicalPlan {
        schema: schema.clone(),
        node: PlanNode::Values(vec![vec![BoundExpr::literal(Value::Integer(1))]]),
    };
    let invalid = LogicalPlan {
        schema,
        node: PlanNode::Recursive {
            id: RecursiveId::default(),
            seed: Box::new(seed),
            step: Box::new(input),
            all: false,
        },
    };
    assert!(matches!(
        db.connect().execute_plan(BoundStatement::Query(invalid)),
        Err(Error::Bind(_))
    ));
    Ok(())
}

#[test]
fn recursive_reachability_matches_independent_transitive_closure() -> Result<()> {
    // Floyd-Warshall supplies an oracle independently of the engine's
    // generation-by-generation recursive evaluation and hash equality.
    const VERTICES: usize = 12;
    for recursion in [
        Arc::new(StreamingRecursion) as Arc<dyn RecursiveAlgorithm>,
        Arc::new(MaterializingRecursion),
    ] {
        for seed in 0..12u64 {
            let mut state = seed + 1;
            let mut closure = [[false; VERTICES]; VERTICES];
            let mut edges = Vec::new();
            for (from, targets) in closure.iter_mut().enumerate() {
                targets[from] = true;
                for (to, reachable) in targets.iter_mut().enumerate() {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    if state >> 60 < 3 {
                        *reachable = true;
                        edges.push(format!("({from},{to})"));
                    }
                }
            }
            for via in 0..VERTICES {
                for from in 0..VERTICES {
                    for to in 0..VERTICES {
                        closure[from][to] |= closure[from][via] && closure[via][to];
                    }
                }
            }
            let db = DatabaseBuilder::new()
                .batch_size(3)
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_recursion(recursion.clone()),
                ))
                .build()?;
            let mut connection = db.connect();
            connection.execute("CREATE TABLE edges(source INTEGER, target INTEGER)")?;
            if !edges.is_empty() {
                connection.execute(&format!("INSERT INTO edges VALUES {}", edges.join(",")))?;
            }
            for start in [0, 5, 11] {
                let actual = connection.query(&format!("WITH RECURSIVE reachable(v) AS (SELECT {start}::INTEGER UNION SELECT e.target FROM reachable r JOIN edges e ON r.v=e.source) SELECT v FROM reachable ORDER BY v"))?;
                let expected = closure[start]
                    .iter()
                    .enumerate()
                    .filter_map(|(index, reachable)| {
                        reachable.then_some(vec![Value::Integer(index as i128)])
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual.rows, expected, "seed {seed}, start {start}");
            }
        }
    }
    Ok(())
}
