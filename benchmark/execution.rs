//! Reproducible in-memory execution comparison. Correctness checks are included
//! in timings; this is not a throughput or compatibility promotion gate.
use duckdb_rust::{
    DatabaseBuilder, Error, Result,
    execution::{Executor, MaterializingExecutor, PullExecutor, StreamControl},
};
use serde_json::json;
use std::{sync::Arc, time::Instant};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn run(rows: usize, iterations: usize, batch_size: usize) -> Result<serde_json::Value> {
    let mut results = Vec::new();
    let adapters: Vec<Arc<dyn Executor>> =
        vec![Arc::new(PullExecutor), Arc::new(MaterializingExecutor)];
    for executor in adapters {
        let database = DatabaseBuilder::new()
            .executor(executor)
            .batch_size(batch_size)
            .build()?;
        let mut c = database.connect();
        c.execute(&format!(
            "CREATE TABLE t(i BIGINT PRIMARY KEY); INSERT INTO t SELECT range FROM range({rows})"
        ))?;
        let n = rows as i128;
        let prefix = rows.min(batch_size) as i128;
        let selected = ((rows - 1) / 17 + 1) as i128;
        for (name, sql, expected_rows, expected_sum, stop) in [
            (
                "scan",
                "SELECT i FROM t".to_owned(),
                rows,
                n * (n - 1) / 2,
                false,
            ),
            (
                "filter",
                "SELECT i FROM t WHERE i%17=0".to_owned(),
                selected as usize,
                17 * selected * (selected - 1) / 2,
                false,
            ),
            (
                "aggregate",
                "SELECT sum(i) FROM t".to_owned(),
                1,
                n * (n - 1) / 2,
                false,
            ),
            (
                "point",
                format!("SELECT i FROM t WHERE i={}", rows / 2),
                1,
                (rows / 2) as i128,
                false,
            ),
            (
                "first_batch",
                "SELECT i FROM t".to_owned(),
                prefix as usize,
                prefix * (prefix - 1) / 2,
                true,
            ),
        ] {
            let prepared = c.prepare(&sql)?;
            let mut samples = Vec::new();
            for iteration in 0..=iterations {
                let mut actual_rows = 0;
                let mut actual_sum = 0;
                let mut first_batch = None;
                let start = Instant::now();
                let summary = c.execute_prepared_batches(&prepared, &[], |_, batch| {
                    first_batch.get_or_insert_with(|| start.elapsed().as_nanos());
                    for row in batch.rows() {
                        actual_rows += 1;
                        actual_sum += row[0].as_i128()?;
                    }
                    Ok(if stop {
                        StreamControl::Stop
                    } else {
                        StreamControl::Continue
                    })
                })?;
                let elapsed = start.elapsed().as_nanos();
                if (actual_rows, actual_sum) != (expected_rows, expected_sum)
                    || summary.execution.rows_delivered != actual_rows
                    || summary.execution.stopped_early != stop
                {
                    return Err(Error::Execution(format!(
                        "benchmark correctness failure: {name}"
                    )));
                }
                if iteration > 0 {
                    samples.push(json!({"elapsed_ns": elapsed, "first_batch_ns": first_batch, "rows": actual_rows, "sum": actual_sum}));
                }
            }
            results.push(json!({"workload": name, "sql": sql, "adapters": database.adapters(), "samples": samples}));
        }
    }
    Ok(
        json!({"suite": "execution", "rows": rows, "iterations": iterations, "warmups": 1, "batch_size": batch_size, "max_intermediate_rows": 10000000, "results": results, "correctness": "passed"}),
    )
}
