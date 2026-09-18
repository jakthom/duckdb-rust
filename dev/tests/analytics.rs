use duckdb_dev::{FileLog, Operation, TraceLayer};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tracing_subscriber::{Registry, layer::SubscriberExt};

fn command(action: &str, directory: &Path, sql: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_duckdb-dev"));
    command.arg(action).arg(directory);
    if let Some(sql) = sql {
        command.arg(sql);
    }
    command.output().unwrap()
}

fn fixture(directory: &Path) {
    let log = FileLog::create(&directory.join("trace.jsonl")).unwrap();
    tracing::subscriber::with_default(
        Registry::default().with(TraceLayer::new(log.clone())),
        || {
            let operation = Operation::enter(tracing::trace_span!(
                "test.query",
                outcome = tracing::field::Empty,
                error = tracing::field::Empty
            ));
            duckdb_dev::value("answer", &42);
            operation.result(&Err::<(), _>("intentional failure"));
        },
    );
    log.flush().unwrap();
}

#[test]
#[ignore = "requires external DuckDB v1.5.5; cargo dev test --package duckdb-dev --test analytics -- --ignored"]
fn raw_and_imported_sql_agree_and_stale_indexes_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("SQL trace 'quoted'");
    fs::create_dir(&directory).unwrap();
    fixture(&directory);
    let sql = "SELECT operation, calls, errors, panics FROM operation_stats";
    let raw = command("sql", &directory, Some(sql));
    assert!(
        raw.status.success(),
        "{}",
        String::from_utf8_lossy(&raw.stderr)
    );
    let raw: Value = serde_json::from_slice(&raw.stdout).unwrap();
    assert_eq!(raw[0]["calls"], 1);
    assert_eq!(raw[0]["errors"], 1);
    let imported = command("index", &directory, None);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let indexed = command("sql", &directory, Some(sql));
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    assert_eq!(
        raw,
        serde_json::from_slice::<Value>(&indexed.stdout).unwrap()
    );
    let path = directory.join("trace.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"kind\":\"value\",\"seq\":9999}\n");
    fs::write(path, bytes).unwrap();
    let stale = command("sql", &directory, Some(sql));
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("trace changed since import"));
    assert!(!command("index", &directory, None).status.success());
}

#[test]
#[ignore = "requires external DuckDB v1.5.5"]
fn malformed_records_are_not_silently_skipped() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("trace.jsonl"), "{\"kind\":\"end\"").unwrap();
    assert!(!command("index", directory.path(), None).status.success());
    assert!(!directory.path().join("trace.duckdb").exists());
}
