use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    common::{
        cast::{CastMode, CastRegistry, CastSpec},
        type_registry::{
            TypeAdapter, TypeRegistry,
            ascii::{self, AsciiCast, MaterializedAscii, StreamingAscii},
        },
    },
    parallel::QueryContext,
};
use serde_json::json;
use std::{cmp::Ordering, sync::Arc, time::Instant};

pub fn run(rows: usize, iterations: usize, batch_size: usize) -> Result<serde_json::Value> {
    let adapters: [Arc<dyn TypeAdapter>; 2] =
        [Arc::new(MaterializedAscii), Arc::new(StreamingAscii)];
    let data_type = ascii::data_type(64)?;
    let value = |s: String| Value::extension(data_type.clone(), s.into_bytes());
    let mut contexts = Vec::new();
    let mut bound = Vec::new();
    let mut databases = Vec::new();
    for adapter in &adapters {
        let mut types = TypeRegistry::builtins();
        types.register(ascii::FAMILY, adapter.clone())?;
        let mut casts = CastRegistry::builtins();
        casts.register_type(&data_type, &types)?;
        casts.register(
            CastSpec {
                source: DataType::Varchar,
                target: data_type.clone(),
                mode: CastMode::Explicit,
            },
            Arc::new(AsciiCast),
        )?;
        bound.push(types.bind(&data_type)?);
        let types = Arc::new(types);
        contexts.push(QueryContext::background().with_types(types.clone()));
        let db = DatabaseBuilder::new()
            .types(types)
            .casts(casts)
            .batch_size(batch_size)
            .build()?;
        db.connect().execute(&format!("CREATE TABLE t AS SELECT CAST((CASE WHEN range%200<100 THEN 'Prefix' ELSE 'PREFIX' END) || (range%100)::VARCHAR AS ascii_ci(64)) AS k FROM range({rows})"))?;
        databases.push(db);
    }
    let mut results = Vec::new();
    for workload in [
        "equal_text",
        "early_difference",
        "late_difference",
        "canonical_key",
        "sql_grouping",
    ] {
        let left = value("Ab".repeat(32));
        let right = value(match workload {
            "early_difference" => format!("B{}", "b".repeat(63)),
            "late_difference" => format!("{}C", "aB".repeat(31) + "A"),
            _ => "aB".repeat(32),
        });
        let expected = if matches!(workload, "early_difference" | "late_difference") {
            Ordering::Less
        } else {
            Ordering::Equal
        };
        let expected_key_payload = b"ab".repeat(32);
        let mut connections = databases.iter().map(|d| d.connect()).collect::<Vec<_>>();
        let statements = connections
            .iter()
            .map(|c| c.prepare("SELECT count(*) FROM (SELECT k FROM t GROUP BY k) groups"))
            .collect::<Result<Vec<_>>>()?;
        let mut samples = [Vec::new(), Vec::new()];
        for iteration in 0..iterations + 3 {
            let order = if iteration % 2 == 0 { [0, 1] } else { [1, 0] };
            for index in order {
                let start = Instant::now();
                if workload == "sql_grouping" {
                    let result = connections[index].execute_prepared(&statements[index], &[])?;
                    if result.rows != vec![vec![Value::Integer(rows.min(100) as i128)]] {
                        return Err(Error::Execution(
                            "type benchmark grouping correctness".into(),
                        ));
                    }
                } else {
                    let mut key = Vec::new();
                    for _ in 0..rows {
                        if workload == "canonical_key" {
                            key.clear();
                            bound[index].append_key(&left, &mut key, &contexts[index])?;
                            if key.len() != 73
                                || key[0] != 1
                                || key[1..9] != 64_u64.to_le_bytes()
                                || key[9..] != expected_key_payload
                            {
                                return Err(Error::Execution(
                                    "type benchmark key correctness".into(),
                                ));
                            }
                        } else if bound[index].compare(&left, &right, &contexts[index])? != expected
                        {
                            return Err(Error::Execution(
                                "type benchmark comparison correctness".into(),
                            ));
                        }
                    }
                }
                let elapsed = start.elapsed().as_nanos();
                if iteration >= 3 {
                    samples[index].push(json!({"elapsed_ns": elapsed, "input_rows": rows}));
                }
            }
        }
        for index in 0..2 {
            results.push(json!({"workload": workload, "type_adapter": adapters[index].name(), "adapters": databases[index].adapters(), "samples": samples[index]}));
        }
    }
    Ok(
        json!({"suite": "types", "rows": rows, "iterations": iterations, "warmups": 3, "batch_size": batch_size, "type_metadata": data_type, "value_size_bytes": std::mem::size_of::<Value>(), "order": "alternating adapters each iteration", "correctness": "passed", "results": results}),
    )
}
