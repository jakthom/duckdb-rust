#[path = "mod.rs"]
mod runner;

use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct FeedbackToken {
    version: u8,
    source_root: std::path::PathBuf,
    path: String,
    source_sha256: String,
    suite_path: std::path::PathBuf,
    suite_sha256: String,
}

fn sha256(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Execute exactly one cache-attested upstream SQLLogic file in this process.
/// This is intentionally separate from the ordinary CLI so its small JSON
/// report cannot be mistaken for a suite-campaign report.
fn feedback(token_path: &std::path::Path, report_path: &std::path::Path) -> Result<(), String> {
    let token: FeedbackToken = serde_json::from_slice(
        &std::fs::read(token_path).map_err(|error| format!("read token: {error}"))?,
    ).map_err(|error| format!("parse token: {error}"))?;
    if token.version != 1 { return Err("unsupported feedback token".into()); }
    let suite_bytes = std::fs::read(&token.suite_path)
        .map_err(|error| format!("read suite metadata: {error}"))?;
    if format!("{:x}", Sha256::digest(&suite_bytes)) != token.suite_sha256 {
        return Err("feedback suite metadata changed".into());
    }
    let root = token.source_root.canonicalize().map_err(|error| format!("source root: {error}"))?;
    let path = root.join(&token.path).canonicalize().map_err(|error| format!("source path: {error}"))?;
    if !path.starts_with(&root) || sha256(&path)? != token.source_sha256 {
        return Err("feedback source bytes changed".into());
    }
    let suite: serde_json::Value = serde_json::from_slice(&suite_bytes)
        .map_err(|error| format!("parse suite metadata: {error}"))?;
    let declared = suite.pointer("/manifest/files").and_then(serde_json::Value::as_array)
        .is_some_and(|files| files.iter().any(|entry| {
            entry.get("path").and_then(serde_json::Value::as_str) == Some(token.path.as_str())
                && entry.get("kind").and_then(serde_json::Value::as_str) == Some("file")
                && entry.get("sha256").and_then(serde_json::Value::as_str) == Some(token.source_sha256.as_str())
        }));
    if !declared { return Err("feedback suite metadata omits selected source bytes".into()); }
    let result = duckdb_rust::Database::memory().and_then(|db| runner::run_file_report(&db, &path));
    let value = match result {
        Ok(file) if matches!(file.status, runner::FileStatus::Passed) && file.passed > 0 => json!({
            "version": 1, "status": "passed", "path": token.path,
            "passed_records": file.passed, "source_sha256": token.source_sha256,
            "suite_sha256": token.suite_sha256,
        }),
        Ok(file) => json!({"version": 1, "status": "not-passed", "path": token.path,
            "passed_records": file.passed, "skipped_records": file.skipped}),
        Err(error) => json!({"version": 1, "status": "failed", "path": token.path,
            "error": error.to_string()}),
    };
    use std::io::Write;
    let mut report = std::fs::OpenOptions::new().write(true).create_new(true).open(report_path)
        .map_err(|error| format!("create feedback report: {error}"))?;
    report.write_all(&serde_json::to_vec(&value).expect("JSON report"))
        .map_err(|error| format!("write feedback report: {error}"))?;
    if value["status"] != "passed" { return Err("feedback SQLLogic assertion failed".into()); }
    println!("{} records passed; 0 skipped", value["passed_records"]);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|argument| argument == "--feedback-token") {
        if arguments.len() != 4 || arguments[2] != "--feedback-report" {
            eprintln!("Usage: sqllogictest --feedback-token TOKEN --feedback-report REPORT");
            return std::process::ExitCode::FAILURE;
        }
        return match feedback(std::path::Path::new(&arguments[1]), std::path::Path::new(&arguments[3])) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error}"); std::process::ExitCode::FAILURE }
        };
    }
    if arguments.is_empty() {
        eprintln!("Usage: sqllogictest [TEST_ROOT] FILE.test [...]");
        return std::process::ExitCode::FAILURE;
    }
    let first = std::path::PathBuf::from(&arguments[0]);
    let paths: Vec<_> = if arguments.len() > 1 && first.is_dir() {
        arguments[1..].iter().map(|path| first.join(path)).collect()
    } else {
        arguments
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect()
    };
    let mut passed = 0;
    let mut skipped = 0;
    let mut generated = 0;
    for path in paths {
        let result =
            duckdb_rust::Database::memory().and_then(|db| runner::run_file_report(&db, &path));
        match result {
            Ok(report) => {
                for line in &report.output {
                    println!("{line}");
                }
                match report.status {
                    runner::FileStatus::Passed => {
                        passed += report.passed;
                        println!("PASS {} ({} records)", path.display(), report.passed);
                    }
                    runner::FileStatus::Skipped(reason) => {
                        skipped += report.skipped.max(1);
                        generated += report.generated;
                        println!("SKIP {} ({reason})", path.display());
                    }
                    runner::FileStatus::GeneratedOutput(reason) => {
                        passed += report.passed;
                        generated += report.generated;
                        println!("GENERATED {} ({reason})", path.display());
                    }
                }
            }
            Err(e) => {
                eprintln!("{e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    if generated == 0 {
        // Keep the ordinary machine-readable verdict stable for Gate P and
        // other callers. Generated output gets an explicit third field below.
        println!("{passed} records passed; {skipped} skipped");
    } else {
        println!("{passed} records passed; {skipped} skipped; {generated} generated");
    }
    std::process::ExitCode::SUCCESS
}
