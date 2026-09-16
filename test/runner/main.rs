#[path = "mod.rs"]
mod runner;

use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Read;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedbackToken {
    kind: String,
    version: u8,
    target: String,
    revision: String,
    workload_id: String,
    sample_id: String,
    source_root: std::path::PathBuf,
    path: String,
    source_sha256: String,
    source_bytes: u64,
    suite_path: std::path::PathBuf,
    suite_sha256: String,
    runner_path: std::path::PathBuf,
    runner_binary_sha256: String,
    runner_source_sha256: String,
    runner_profile: String,
    provenance_sha256: String,
    timeout_ms: u64,
}

fn sha256(path: &std::path::Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut hasher = Sha256::new(); let mut block = [0; 8192];
    loop { let count = file.read(&mut block).map_err(|error| format!("hash {}: {error}", path.display()))?;
        if count == 0 { break; } hasher.update(&block[..count]); }
    Ok(format!("{:x}", hasher.finalize()))
}

fn regular(path: &std::path::Path, label: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| format!("{label}: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() { return Err(format!("{label} must be a regular file")); }
    Ok(())
}

fn hex_hash(value: &str) -> bool { value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) }

/// Execute exactly one cache-attested upstream SQLLogic file in this process.
/// This is intentionally separate from the ordinary CLI so its small JSON
/// report cannot be mistaken for a suite-campaign report.
fn feedback(token_path: &std::path::Path, expected_token_sha256: &str, report_path: &std::path::Path) -> Result<(), String> {
    regular(token_path, "feedback token")?;
    if !hex_hash(expected_token_sha256) || sha256(token_path)? != expected_token_sha256 { return Err("feedback token changed".into()); }
    let token: FeedbackToken = serde_json::from_slice(
        &std::fs::read(token_path).map_err(|error| format!("read token: {error}"))?,
    ).map_err(|error| format!("parse token: {error}"))?;
    if token.kind != "duckdb-rust-selected-feedback" || token.version != 1
        || !matches!(token.target.as_str(), "release" | "development") || token.revision.len() != 40
        || token.workload_id.is_empty() || token.sample_id.is_empty() || token.timeout_ms == 0
        || !hex_hash(&token.source_sha256) || !hex_hash(&token.suite_sha256)
        || !hex_hash(&token.runner_binary_sha256) || !hex_hash(&token.runner_source_sha256)
        || !hex_hash(&token.provenance_sha256) || token.runner_profile != "release"
        || std::path::Path::new(&token.path).is_absolute() || token.path.contains("..") {
        return Err("invalid feedback token identity".into());
    }
    regular(&token.suite_path, "feedback suite metadata")?;
    let suite_bytes = std::fs::read(&token.suite_path)
        .map_err(|error| format!("read suite metadata: {error}"))?;
    if format!("{:x}", Sha256::digest(&suite_bytes)) != token.suite_sha256 {
        return Err("feedback suite metadata changed".into());
    }
    let root = token.source_root.canonicalize().map_err(|error| format!("source root: {error}"))?;
    let path = root.join(&token.path).canonicalize().map_err(|error| format!("source path: {error}"))?;
    regular(&path, "feedback source")?;
    if !path.starts_with(&root) || std::fs::metadata(&path).map_err(|e| e.to_string())?.len() != token.source_bytes || sha256(&path)? != token.source_sha256 {
        return Err("feedback source bytes changed".into());
    }
    let executable = std::env::current_exe().map_err(|error| format!("current runner: {error}"))?.canonicalize().map_err(|error| error.to_string())?;
    if executable != token.runner_path.canonicalize().map_err(|error| format!("token runner: {error}"))?
        || sha256(&executable)? != token.runner_binary_sha256 { return Err("feedback runner identity changed".into()); }
    let suite: serde_json::Value = serde_json::from_slice(&suite_bytes)
        .map_err(|error| format!("parse suite metadata: {error}"))?;
    let declared = suite.pointer("/identity").is_some_and(|identity| identity["target"] == token.target && identity["revision"] == token.revision
        && identity["paths"] == json!([token.path]) && identity["files"] == json!([{"path": token.path, "kind": "file", "sha256": token.source_sha256}]))
        && suite.pointer("/manifest").is_some_and(|manifest| manifest["revision"] == token.revision
            && manifest["files"] == json!([{"path": token.path, "kind": "file", "sha256": token.source_sha256}])
            && manifest["tests"] == json!([{"id": token.path, "kind": "sqllogictest", "path": token.path, "line": 1}])
            && manifest["counts"] == json!({"sqllogictest": 1}));
    if !declared { return Err("feedback suite metadata omits selected source bytes".into()); }
    let result = duckdb_rust::Database::memory().and_then(|db| runner::run_file_report(&db, &path));
    let value = match result {
        Ok(file) if matches!(file.status, runner::FileStatus::Passed) && file.passed > 0 && file.skipped == 0 && file.generated == 0 => json!({
            "kind": token.kind, "version": 1, "status": "passed", "target": token.target, "revision": token.revision,
            "workload_id": token.workload_id, "sample_id": token.sample_id, "path": token.path,
            "declarations": file.declarations, "passed_records": file.passed, "skipped_records": file.skipped, "generated_records": file.generated,
            "source_sha256": token.source_sha256, "suite_sha256": token.suite_sha256,
            "token_sha256": expected_token_sha256, "runner_binary_sha256": token.runner_binary_sha256,
        }),
        Ok(file) => json!({"version": 1, "status": "not-passed", "path": token.path,
            "passed_records": file.passed, "skipped_records": file.skipped}),
        Err(error) => json!({"version": 1, "status": "failed", "path": token.path,
            "error": error.to_string()}),
    };
    if sha256(token_path)? != expected_token_sha256 || sha256(&path)? != token.source_sha256 || sha256(&token.suite_path)? != token.suite_sha256 {
        return Err("feedback inputs changed during execution".into());
    }
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
        if arguments.len() != 6 || arguments[2] != "--token-sha256" || arguments[4] != "--feedback-report" {
            eprintln!("Usage: sqllogictest --feedback-token TOKEN --token-sha256 SHA256 --feedback-report REPORT");
            return std::process::ExitCode::FAILURE;
        }
        return match feedback(std::path::Path::new(&arguments[1]), &arguments[3].to_string_lossy(), std::path::Path::new(&arguments[5])) {
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
