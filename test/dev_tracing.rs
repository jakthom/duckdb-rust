use serde_json::Value;
use std::{fs, path::Path, process::Command};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn statements(directory: &Path) -> Vec<(std::path::PathBuf, Value)> {
    let mut result = Vec::new();
    for entry in fs::read_dir(directory.join("statements")).unwrap() {
        let path = entry.unwrap().path();
        let metadata =
            serde_json::from_slice(&fs::read(path.join("statement.json")).unwrap()).unwrap();
        result.push((path, metadata));
    }
    result
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn shell_batch_has_distinct_execution_files_repeat_hashes_results_and_errors() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_duckdb-rust"))
        .args([
            "-c",
            "SELECT ';' AS punctuation; SELECT 42 AS answer; SELECT 42 AS answer; SELECT missing",
        ])
        .env("DUCKDB_DEV_LOG_DIR", directory.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let records = statements(directory.path());
    let requests = records
        .iter()
        .filter(|(_, value)| value["phase"] == "request")
        .collect::<Vec<_>>();
    let executions = records
        .iter()
        .filter(|(_, value)| value["phase"] == "execute")
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 1);
    assert_eq!(executions.len(), 4);
    assert_eq!(requests[0].1["status"], "error");
    let repeated = executions
        .iter()
        .filter(|(_, value)| value["sql"] == "SELECT 42 AS answer")
        .collect::<Vec<_>>();
    assert_eq!(repeated.len(), 2);
    assert_eq!(repeated[0].1["sql_hash"], repeated[1].1["sql_hash"]);
    assert_ne!(repeated[0].1["execution_id"], repeated[1].1["execution_id"]);
    for (path, metadata) in &executions {
        assert_eq!(
            metadata["parent_execution_id"],
            requests[0].1["execution_id"]
        );
        let summary = duckdb_dev::report::summarize(path, None).unwrap();
        let (cached, pending) = duckdb_dev::report::cached(path, None).unwrap();
        assert!(summary.incomplete.is_empty());
        assert_eq!(pending, 0);
        assert_eq!(summary.completed, cached.completed);
        assert_eq!(summary.errors, cached.errors);
        assert_eq!(summary.records, cached.records);
        for (name, operation) in &summary.operations {
            assert_eq!(operation.calls, cached.operations[name].calls);
            assert_eq!(operation.total_ns, cached.operations[name].total_ns);
        }
        let raw = fs::read_to_string(path.join("trace.jsonl")).unwrap();
        let headers = raw
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|record| record["kind"] == "statement")
            .collect::<Vec<_>>();
        assert_eq!(headers.len(), 1);
        assert_eq!(
            headers[0]["statement"]["execution_id"],
            metadata["execution_id"]
        );
    }
    let result: Value =
        serde_json::from_slice(&fs::read(repeated[0].0.join("result.json")).unwrap()).unwrap();
    assert_eq!(result["row_count"], 1);
    assert_eq!(result["preview"][0][0]["Integer"], 42);
    assert_eq!(
        executions
            .iter()
            .filter(|(_, value)| value["status"] == "error")
            .count(),
        1
    );
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn prepared_streaming_concurrent_and_invalid_sql_keep_execution_identity() {
    if std::env::var_os("DUCKDB_TRACE_TEST_WORKER").is_some() {
        let database = duckdb_rust::Database::memory().unwrap();
        let mut connection = database.connect();
        let prepared = connection.prepare("SELECT $1 AS answer").unwrap();
        for n in [7, 8] {
            let result = connection
                .execute_prepared(&prepared, &[duckdb_rust::Value::Integer(n)])
                .unwrap();
            assert_eq!(result.rows.len(), 1);
        }
        let result = connection
            .execute_prepared_batches(&prepared, &[duckdb_rust::Value::Integer(9)], |_, chunk| {
                assert_eq!(chunk.len(), 1);
                Ok(duckdb_rust::execution::StreamControl::Stop)
            })
            .unwrap();
        assert!(result.execution.stopped_early);
        assert!(connection.execute("SELECT (").is_err());
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let database = &database;
                scope.spawn(move || {
                    database.connect().query("SELECT 99").unwrap();
                });
            }
        });
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "prepared_streaming_concurrent_and_invalid_sql_keep_execution_identity",
            "--nocapture",
        ])
        .env("DUCKDB_TRACE_TEST_WORKER", "1")
        .env("DUCKDB_DEV_LOG_DIR", directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = statements(directory.path());
    let prepared = records
        .iter()
        .filter(|(_, value)| value["phase"] == "execute" && value["sql"] == "SELECT $1 AS answer")
        .collect::<Vec<_>>();
    assert_eq!(prepared.len(), 3);
    let mut ids = std::collections::HashSet::new();
    let mut parameters = Vec::new();
    for (path, metadata) in prepared {
        assert!(ids.insert(metadata["execution_id"].as_str().unwrap()));
        assert_eq!(
            metadata["sql_hash"],
            duckdb_dev::statement::sql_hash("SELECT $1 AS answer")
        );
        let params: Value =
            serde_json::from_slice(&fs::read(path.join("parameters.json")).unwrap()).unwrap();
        parameters.push(params[0]["Integer"].as_i64().unwrap());
    }
    parameters.sort();
    assert_eq!(parameters, [7, 8, 9]);
    assert_eq!(
        records
            .iter()
            .filter(|(_, value)| value["sql"] == "SELECT 99" && value["phase"] == "execute")
            .count(),
        2
    );
    assert!(
        records
            .iter()
            .any(|(_, value)| value["sql"] == "SELECT (" && value["status"] == "error")
    );
    for (path, _) in records {
        assert!(
            duckdb_dev::report::summarize(&path, None)
                .unwrap()
                .incomplete
                .is_empty()
        );
    }
}
