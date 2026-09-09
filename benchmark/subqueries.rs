//! Compare nested query consumption through the public composition and SQL API.
use duckdb_rust::{
    DatabaseBuilder, Error, Result,
    execution::subquery::{MaterializingSubqueries, StreamingSubqueries, SubqueryExecutor},
};
use serde_json::json;
use std::{sync::Arc, time::Instant};

pub fn run(rows: usize, iterations: usize, batch_size: usize) -> Result<serde_json::Value> {
    let adapters: [Arc<dyn SubqueryExecutor>; 2] = [
        Arc::new(StreamingSubqueries),
        Arc::new(MaterializingSubqueries),
    ];
    let databases = adapters.iter().map(|adapter| {
        let db = DatabaseBuilder::new().subqueries(adapter.clone()).batch_size(batch_size).build()?;
        db.connect().execute(&format!("CREATE TABLE t AS SELECT i FROM range({rows}) t(i); CREATE TABLE u AS SELECT i%32 k,i v FROM range(64) u(i); INSERT INTO u VALUES(NULL,100)"))?;
        Ok(db)
    }).collect::<Result<Vec<_>>>()?;
    let selected = (0..rows).filter(|i| i % 64 < 32).count() as i128;
    let modulo_sum = (0..rows).map(|i| (i % 32) as i128).sum::<i128>();
    let mut results = Vec::new();
    let mut comparisons = Vec::new();
    for (workload, sql, expected) in [
        (
            "scalar_once",
            "SELECT sum((SELECT count(*) FROM u)) FROM t",
            rows as i128 * 65,
        ),
        (
            "correlated_scalar",
            "SELECT sum((SELECT max(v) FROM u WHERE u.k=t.i%32)) FROM t",
            modulo_sum + rows as i128 * 32,
        ),
        (
            "correlated_exists",
            "SELECT count(*) FROM t WHERE EXISTS(SELECT 1 FROM u WHERE u.k=t.i%64)",
            selected,
        ),
        (
            "in_nullable",
            "SELECT count(*) FROM t WHERE t.i%64 IN(SELECT k FROM u WHERE v>=32)",
            selected,
        ),
        (
            "in_empty",
            "SELECT count(*) FROM t WHERE NULL NOT IN(SELECT k FROM u WHERE false)",
            rows as i128,
        ),
        (
            "scalar_top",
            "SELECT sum((SELECT k FROM u WHERE k=t.i%32 ORDER BY v DESC LIMIT 1)) FROM t",
            modulo_sum,
        ),
    ] {
        let mut connections = databases.iter().map(|db| db.connect()).collect::<Vec<_>>();
        let statements = connections
            .iter()
            .map(|c| c.prepare(sql))
            .collect::<Result<Vec<_>>>()?;
        let mut samples = [Vec::new(), Vec::new()];
        let mut elapsed = [Vec::new(), Vec::new()];
        for iteration in 0..iterations + 3 {
            for index in if iteration.is_multiple_of(2) {
                [0, 1]
            } else {
                [1, 0]
            } {
                let start = Instant::now();
                let result = connections[index].execute_prepared(&statements[index], &[])?;
                if result.rows.len() != 1
                    || result.rows[0].len() != 1
                    || result.rows[0][0].as_i128()? != expected
                {
                    return Err(Error::Execution(format!(
                        "subquery benchmark mismatch: {workload}"
                    )));
                }
                let ns = start.elapsed().as_nanos();
                if iteration >= 3 {
                    samples[index].push(json!({"elapsed_ns":ns,"result":expected.to_string()}));
                    elapsed[index].push(ns);
                }
            }
        }
        let mut medians = [0; 2];
        for index in 0..2 {
            elapsed[index].sort_unstable();
            medians[index] = elapsed[index][elapsed[index].len() / 2];
            results.push(json!({"workload":workload,"sql":sql,"adapters":databases[index].adapters(),"samples":samples[index],"median_ns":medians[index]}));
        }
        let ratio = medians[0] as f64 / medians[1] as f64;
        comparisons.push(json!({"workload":workload,"streaming_over_materializing_median":ratio,"passed":ratio<=1.0}));
    }
    let passed = comparisons.iter().all(|c| c["passed"] == true);
    Ok(
        json!({"suite":"subqueries","rows":rows,"inner_rows":65,"iterations":iterations,"warmups":3,"batch_size":batch_size,"max_intermediate_rows":10000000,"order":"alternating adapters by sample","correctness":"passed","results":results,"selection_budget":{"max_ratio":1.0,"passed":passed,"comparisons":comparisons}}),
    )
}
