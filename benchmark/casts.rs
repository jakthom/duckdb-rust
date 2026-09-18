//! Compare conversion algorithms through bound casts and normal SQL callers.
use std::{sync::Arc, time::Instant};

use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    common::cast::{
        CastFunction, CastMode, CastRegistry, CastSpec, DigitIntegerCast, PrimitiveCast,
    },
    parallel::QueryContext,
};
use serde_json::json;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn run(rows: usize, iterations: usize, batch_size: usize) -> Result<serde_json::Value> {
    let adapters: [Arc<dyn CastFunction>; 2] =
        [Arc::new(PrimitiveCast), Arc::new(DigitIntegerCast)];
    let mut databases = Vec::new();
    let mut casts = Vec::new();
    let target = DataType::BigInt;
    for adapter in &adapters {
        let mut registry = CastRegistry::builtins();
        for mode in [CastMode::Explicit, CastMode::Assignment] {
            registry.replace(
                CastSpec {
                    source: DataType::Varchar,
                    target: target.clone(),
                    mode,
                },
                adapter.clone(),
            )?;
        }
        casts.push(registry.bind(
            &DataType::Varchar,
            &target,
            CastMode::Explicit,
            &duckdb_rust::common::type_registry::builtin_types(),
        )?);
        let database = DatabaseBuilder::new()
            .casts(registry)
            .batch_size(batch_size)
            .build()?;
        database.connect().execute(&format!(
            "CREATE TABLE t AS SELECT (range-{})::VARCHAR AS s FROM range({rows})",
            rows / 2
        ))?;
        databases.push(database);
    }
    let values: Vec<_> = (0..rows)
        .map(|i| Value::Varchar((i as i128 - (rows / 2) as i128).to_string()))
        .collect();
    let expected: i128 = (0..rows).map(|i| i as i128 - (rows / 2) as i128).sum();
    let q = QueryContext::background();
    let mut results = Vec::new();
    for workload in ["bound_cast", "sql_cast_aggregate"] {
        let mut samples = [Vec::new(), Vec::new()];
        let mut connections = databases.iter().map(|d| d.connect()).collect::<Vec<_>>();
        let statements = connections
            .iter()
            .map(|c| c.prepare("SELECT sum(s::BIGINT) FROM t"))
            .collect::<Result<Vec<_>>>()?;
        for iteration in 0..iterations + 3 {
            let order = if iteration % 2 == 0 { [0, 1] } else { [1, 0] };
            for index in order {
                let start = Instant::now();
                let sum = if workload == "bound_cast" {
                    let mut sum = 0;
                    for value in &values {
                        sum += casts[index].apply(value, &q)?.as_i128()?;
                    }
                    sum
                } else {
                    let result = connections[index].execute_prepared(&statements[index], &[])?;
                    if result.rows.len() != 1 || result.rows[0].len() != 1 {
                        return Err(Error::Execution("cast benchmark result shape".into()));
                    }
                    result.rows[0][0].as_i128()?
                };
                if sum != expected {
                    return Err(Error::Execution("cast benchmark checksum".into()));
                }
                let elapsed = start.elapsed().as_nanos();
                if iteration >= 3 {
                    samples[index]
                        .push(json!({"elapsed_ns": elapsed, "input_rows": rows, "sum": sum}));
                }
            }
        }
        for index in 0..2 {
            results.push(json!({"workload": workload, "cast_adapter": adapters[index].name(), "source_type": "VARCHAR", "target_type": "BIGINT", "mode": "Explicit", "adapters": databases[index].adapters(), "samples": samples[index]}));
        }
    }
    Ok(
        json!({"suite": "casts", "rows": rows, "iterations": iterations, "batch_size": batch_size, "warmups": 3, "order": "alternating adapters each iteration", "results": results, "correctness": "passed"}),
    )
}
