use super::*;
use duckdb_rust::{
    common::Row,
    execution::{
        expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
        operator::join::{HashJoin, JoinAlgorithm, NestedLoopJoin},
    },
    optimizer::{DecorrelateExists, OptimizerContext, OptimizerPass, SimplifyExpressions},
    planner::{LogicalPlan, PlanNode, logical::JoinKind},
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn joins() -> [Arc<dyn JoinAlgorithm>; 2] {
    [Arc::new(HashJoin), Arc::new(NestedLoopJoin)]
}

#[test]
fn decorrelated_floating_keys_keep_nan_signed_zero_and_null_equality() -> Result<()> {
    for join in joins() {
        for data_type in ["FLOAT", "DOUBLE"] {
            let mut c = DatabaseBuilder::new()
                .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![
                    join.clone(),
                ])))
                .build()?
                .connect();
            c.execute(&format!("CREATE TABLE t(k {data_type}); INSERT INTO t VALUES('NaN'::{data_type}),('-0'::{data_type}),(0::{data_type}),(1::{data_type}),(1::{data_type}),(NULL); CREATE TABLE u(k {data_type}); INSERT INTO u VALUES('NaN'::{data_type}),(0::{data_type}),(NULL)"))?;
            for (negated, expected) in [(false, 3), (true, 3)] {
                let sql = format!(
                    "SELECT count(*) FROM t WHERE {}EXISTS(SELECT 1 FROM u WHERE u.k=t.k)",
                    if negated { "NOT " } else { "" }
                );
                assert_eq!(c.query(&sql)?.rows, vec![vec![Value::Integer(expected)]]);
            }
        }
    }
    Ok(())
}

#[test]
fn decorrelation_preserves_nulls_duplicates_order_scope_and_empty_inputs() -> Result<()> {
    let values = [-65, -64, -63, -2, -1, 0, 1, 1, 2, 63, 64, 65];
    for optimizer in [
        Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
        Arc::new(PipelineOptimizer::default()),
        Arc::new(PipelineOptimizer::new(vec![Arc::new(DecorrelateExists)])),
    ] {
        for join in joins() {
            for expressions in [
                Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                Arc::new(BatchedEvaluator),
            ] {
                for subqueries in adapters() {
                    for batch_size in [1, 3, 2048] {
                        let mut c = DatabaseBuilder::new()
                            .optimizer(optimizer.clone())
                            .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![
                                join.clone(),
                            ])))
                            .expressions(expressions.clone())
                            .subqueries(subqueries.clone())
                            .batch_size(batch_size)
                            .build()?
                            .connect();
                        c.execute("CREATE TABLE t(k BIGINT, label VARCHAR); CREATE TABLE u(label VARCHAR, k BIGINT); INSERT INTO u VALUES('a',-1),('b',0),('c',1),('d',1),('null',NULL)")?;
                        for value in values {
                            c.execute(&format!("INSERT INTO t VALUES({value},'v{value}')"))?;
                        }
                        c.execute("INSERT INTO t VALUES(NULL,'null')")?;
                        for negated in [false, true] {
                            let mut expected: Vec<Row> = values
                                .into_iter()
                                .filter(|value| [-1, 0, 1].contains(&(value % 64)) != negated)
                                .map(|value| {
                                    vec![Value::Integer(value), Value::Varchar(format!("v{value}"))]
                                })
                                .collect();
                            if negated {
                                expected.push(vec![Value::Null, Value::Varchar("null".into())]);
                            }
                            for equality in ["u.k=t.k%64", "t.k%64=u.k"] {
                                let sql = format!(
                                    "SELECT t.k,t.label FROM t WHERE {}EXISTS(SELECT 1 FROM u WHERE {equality})",
                                    if negated { "NOT " } else { "" }
                                );
                                assert_eq!(
                                    c.query(&sql)?.rows,
                                    expected,
                                    "{sql}; {} {} {} {batch_size}",
                                    optimizer.name(),
                                    join.name(),
                                    expressions.name()
                                );
                                assert_eq!(
                                    c.query(&format!("{sql} LIMIT 2 OFFSET 1"))?.rows,
                                    expected[1..3].to_vec()
                                );
                            }
                        }
                        c.execute("DELETE FROM u")?;
                        for setup in ["SELECT 1", "INSERT INTO u VALUES('null',NULL)"] {
                            c.execute(setup)?;
                            assert_eq!(c.query("SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.k=t.k%64)")?.rows, vec![vec![Value::Integer(0)]]);
                            assert_eq!(c.query("SELECT count(*) FROM t WHERE NOT EXISTS(SELECT 1 FROM u WHERE u.k=t.k%64)")?.rows, vec![vec![Value::Integer(13)]]);
                        }
                        c.execute("DELETE FROM t")?;
                        assert!(
                            c.query(
                                "SELECT * FROM t WHERE NOT EXISTS(SELECT 1 FROM u WHERE u.k=t.k%64)"
                            )?
                            .rows
                            .is_empty()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

struct ObserveJoins(Arc<AtomicUsize>);
impl OptimizerPass for ObserveJoins {
    fn name(&self) -> &'static str {
        "observe-existence-joins"
    }
    fn rewrite(&self, plan: LogicalPlan, _: &OptimizerContext<'_>) -> Result<LogicalPlan> {
        if matches!(
            plan.node,
            PlanNode::Join {
                kind: JoinKind::Semi | JoinKind::Anti,
                ..
            }
        ) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        Ok(plan)
    }
}

#[test]
fn rewrite_requires_total_immediate_correlation_and_budgeted_snapshot_metadata() -> Result<()> {
    use duckdb_rust::{storage::checkpoint::MemoryDurability, transaction::SnapshotTransactions};
    let manager = Arc::new(SnapshotTransactions::new(Arc::new(MemoryDurability))?);
    let mut writer = DatabaseBuilder::new()
        .transactions(manager.clone())
        .build()?
        .connect();
    writer.execute("CREATE TABLE t(k BIGINT,s VARCHAR); INSERT INTO t VALUES(0,'0'),(1,'bad'),(-9223372036854775808,'2'); CREATE TABLE u(k BIGINT); INSERT INTO u VALUES(0),(1),(2)")?;
    for budget in [2, 100] {
        let rewritten = Arc::new(AtomicUsize::new(0));
        let mut c = DatabaseBuilder::new()
            .transactions(manager.clone())
            .max_intermediate_rows(budget)
            .batch_size(1)
            .optimizer(Arc::new(PipelineOptimizer::new(vec![
                Arc::new(SimplifyExpressions),
                Arc::new(DecorrelateExists),
                Arc::new(ObserveJoins(rewritten.clone())),
            ])))
            .build()?
            .connect();
        for (sql, eligible, succeeds) in [
            (
                "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.k=t.k%64)",
                true,
                true,
            ),
            (
                "SELECT count(*) FROM t WHERE NOT EXISTS(SELECT 1 FROM u WHERE u.k=t.k)",
                true,
                true,
            ),
            (
                "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.k=t.k LIMIT 1)",
                false,
                true,
            ),
            (
                "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.k=t.k OFFSET 1)",
                false,
                true,
            ),
            (
                "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.k=t.k AND u.k>0)",
                false,
                true,
            ),
            (
                "SELECT count(*) FROM t WHERE EXISTS(SELECT DISTINCT u.k FROM u WHERE u.k=t.k)",
                false,
                true,
            ),
            (
                "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.k=t.k%-1)",
                false,
                false,
            ),
            (
                "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.k=CAST(t.s AS BIGINT))",
                false,
                false,
            ),
            (
                "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE EXISTS(SELECT 1 FROM u x WHERE x.k=t.k))",
                false,
                true,
            ),
        ] {
            let result = c.query(sql);
            assert_eq!(result.is_ok(), succeeds, "{sql}: {result:?}");
            assert_eq!(
                rewritten.swap(0, Ordering::Relaxed),
                usize::from(eligible && budget >= 3),
                "{sql}; budget {budget}"
            );
        }
    }
    Ok(())
}

#[test]
fn decorrelated_prepared_queries_rebuild_from_each_visible_snapshot() -> Result<()> {
    let db = Database::memory()?;
    let mut writer = db.connect();
    writer.execute("CREATE TABLE t AS SELECT i FROM range(8) t(i); CREATE TABLE u(i BIGINT); INSERT INTO u VALUES(0),(1)")?;
    let prepared =
        writer.prepare("SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.i=t.i%$1)")?;
    let mut reader = db.connect();
    reader.execute("BEGIN")?;
    writer.execute("BEGIN; DELETE FROM u; INSERT INTO u VALUES(2)")?;
    assert_eq!(
        writer
            .execute_prepared(&prepared, &[Value::Integer(4)])?
            .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        reader
            .execute_prepared(&prepared, &[Value::Integer(4)])?
            .rows,
        vec![vec![Value::Integer(4)]]
    );
    writer.execute("ROLLBACK")?;
    assert_eq!(
        writer
            .execute_prepared(&prepared, &[Value::Integer(2)])?
            .rows,
        vec![vec![Value::Integer(8)]]
    );
    writer.execute("DELETE FROM u; INSERT INTO u VALUES(3)")?;
    assert_eq!(
        writer
            .execute_prepared(&prepared, &[Value::Integer(4)])?
            .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        reader
            .execute_prepared(&prepared, &[Value::Integer(4)])?
            .rows,
        vec![vec![Value::Integer(4)]]
    );
    reader.execute("COMMIT")?;
    assert_eq!(
        reader
            .execute_prepared(&prepared, &[Value::Integer(2)])?
            .rows,
        vec![vec![Value::Integer(0)]]
    );
    writer.execute("DELETE FROM u")?;
    assert_eq!(
        writer
            .execute_prepared(&prepared, &[Value::Integer(4)])?
            .rows,
        vec![vec![Value::Integer(0)]]
    );
    Ok(())
}
