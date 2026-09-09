//! Compare LIKE implementations through checked bindings and prepared SQL.
use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    common::type_registry::builtin_types,
    function::operator::{
        DynamicLike, GreedyLike, Operator, OperatorFunction, OperatorRegistry, OperatorSignature,
    },
    parallel::QueryContext,
};
use serde_json::json;
use std::{sync::Arc, time::Instant};

pub fn run(rows: usize, iterations: usize, batch_size: usize) -> Result<serde_json::Value> {
    let adapters: [Arc<dyn OperatorFunction>; 2] = [Arc::new(DynamicLike), Arc::new(GreedyLike)];
    let mut databases = Vec::new();
    let mut functions = Vec::new();
    for adapter in &adapters {
        let mut registry = OperatorRegistry::builtins();
        registry.replace(
            OperatorSignature {
                operator: Operator::Like,
                arguments: vec![DataType::Varchar; 2],
                result: DataType::Boolean,
                nullable: false,
            },
            adapter.clone(),
        )?;
        functions.push(registry.bind(
            Operator::Like,
            &[DataType::Varchar, DataType::Varchar],
            &builtin_types(),
        )?);
        let database = DatabaseBuilder::new()
            .operators(registry)
            .batch_size(batch_size)
            .build()?;
        database.connect().execute(&format!("CREATE TABLE t AS SELECT CASE WHEN range%11=0 THEN NULL ELSE 'prefix-🦆-'||(range%10)||'-suffix' END AS v FROM range({rows})"))?;
        databases.push(database);
    }
    let query = QueryContext::background();
    let mut results = Vec::new();
    let mut comparisons = Vec::new();
    for (workload, value, pattern, matched) in [
        (
            "exact_unicode",
            "prefix-🦆-suffix".to_owned(),
            "prefix-🦆-suffix".to_owned(),
            true,
        ),
        (
            "unicode_wildcards",
            "prefix-🦆-suffix".to_owned(),
            "prefix-_-s%".to_owned(),
            true,
        ),
        (
            "early_mismatch",
            "prefix-🦆-suffix".to_owned(),
            "other%".to_owned(),
            false,
        ),
        (
            "suffix_retries",
            format!("{}z", "a".repeat(128)),
            "%aaaaab".to_owned(),
            false,
        ),
        ("sql_like_filter", String::new(), String::new(), true),
    ] {
        let arguments = [Value::Varchar(value), Value::Varchar(pattern)];
        let mut samples = [Vec::new(), Vec::new()];
        let mut elapsed = [Vec::new(), Vec::new()];
        let mut connections = databases.iter().map(|db| db.connect()).collect::<Vec<_>>();
        let statements = connections
            .iter()
            .map(|c| c.prepare("SELECT count(*) FROM t WHERE v LIKE 'prefix%🦆%suffix'"))
            .collect::<Result<Vec<_>>>()?;
        let expected = if workload == "sql_like_filter" {
            rows - rows.div_ceil(11)
        } else if matched {
            rows
        } else {
            0
        };
        for iteration in 0..iterations + 3 {
            for index in if iteration.is_multiple_of(2) {
                [0, 1]
            } else {
                [1, 0]
            } {
                let start = Instant::now();
                let count = if workload == "sql_like_filter" {
                    let result = connections[index].execute_prepared(&statements[index], &[])?;
                    if result.rows.len() != 1 || result.rows[0].len() != 1 {
                        return Err(Error::Execution("operator benchmark result shape".into()));
                    }
                    usize::try_from(result.rows[0][0].as_i128()?)
                        .map_err(|_| Error::Execution("operator benchmark count".into()))?
                } else {
                    let mut count = 0;
                    for _ in 0..rows {
                        let result = functions[index].apply(&arguments, &query)?;
                        if result != Value::Boolean(matched) {
                            return Err(Error::Execution("operator benchmark value".into()));
                        }
                        count += usize::from(matched);
                    }
                    count
                };
                if count != expected {
                    return Err(Error::Execution("operator benchmark checksum".into()));
                }
                let ns = start.elapsed().as_nanos();
                if iteration >= 3 {
                    samples[index].push(json!({"elapsed_ns":ns,"input_rows":rows,"matched":count}));
                    elapsed[index].push(ns);
                }
            }
        }
        for index in 0..2 {
            results.push(json!({"workload":workload,"operator_adapter":adapters[index].name(),"adapters":databases[index].adapters(),"samples":samples[index]}));
        }
        let median = |values: &mut Vec<u128>| {
            values.sort_unstable();
            let n = values.len();
            if n.is_multiple_of(2) {
                (values[n / 2 - 1] + values[n / 2]) as f64 / 2.0
            } else {
                values[n / 2] as f64
            }
        };
        let ratio = median(&mut elapsed[1]) / median(&mut elapsed[0]);
        comparisons.push(
            json!({"workload":workload,"greedy_over_dynamic_median":ratio,"passed":ratio<=1.25}),
        );
    }
    let passed = comparisons.iter().all(|c| c["passed"] == true);
    Ok(
        json!({"suite":"operators","rows":rows,"iterations":iterations,"batch_size":batch_size,"warmups":3,"order":"alternating adapters each iteration","results":results,"correctness":"passed","selection_budget":{"max_ratio":1.25,"comparisons":comparisons,"passed":passed}}),
    )
}
