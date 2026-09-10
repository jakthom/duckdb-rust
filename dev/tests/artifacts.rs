use duckdb_dev::artifacts::{self, Session};
use std::{fs, process::Command};

#[test]
fn temporary_runs_cleanup_on_success_and_early_return_without_touching_other_files() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("target/dev-traces");
    let session = Session::begin(root.path()).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("trace.jsonl"), b"test").unwrap();
    fs::write(root.path().join("target/keep.db"), b"database").unwrap();
    session.finish(false).unwrap();
    assert!(!cache.exists());
    assert_eq!(
        fs::read(root.path().join("target/keep.db")).unwrap(),
        b"database"
    );
    {
        let _session = Session::begin(root.path()).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("partial.jsonl"), b"partial").unwrap();
    }
    assert!(!cache.exists());
}

#[test]
fn keeping_a_run_replaces_history_and_rejects_oversized_retention() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("target/dev-traces");
    let session = Session::begin(root.path()).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("old.jsonl"), b"old").unwrap();
    session.finish(true).unwrap();
    assert!(cache.exists());
    let session = Session::begin(root.path()).unwrap();
    assert!(!cache.exists());
    fs::create_dir_all(&cache).unwrap();
    fs::File::create(cache.join("large.jsonl"))
        .unwrap()
        .set_len(257 * 1024 * 1024)
        .unwrap();
    assert!(
        session
            .finish(true)
            .unwrap_err()
            .to_string()
            .contains("retention limit")
    );
    assert!(!cache.exists());
}

#[test]
fn active_leases_prevent_cleanup_and_readers_can_inspect_recording() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("target/dev-traces");
    let session = Session::begin(root.path()).unwrap();
    fs::create_dir_all(&cache).unwrap();
    let reader = artifacts::read_lease(root.path()).unwrap();
    assert!(artifacts::clean(root.path()).is_err());
    assert!(Session::begin(root.path()).is_err());
    session.finish(true).unwrap();
    assert!(artifacts::clean(root.path()).is_err());
    drop(reader);
    artifacts::clean(root.path()).unwrap();
    assert!(!cache.exists());
}

#[test]
fn recording_write_budget_fails_explicitly_instead_of_filling_disk() {
    if let Some(directory) = std::env::var_os("DUCKDB_BUDGET_TEST_WORKER") {
        let log =
            duckdb_dev::FileLog::create(&std::path::PathBuf::from(directory).join("trace.jsonl"))
                .unwrap();
        use tracing_subscriber::layer::SubscriberExt;
        tracing::subscriber::with_default(
            tracing_subscriber::Registry::default().with(duckdb_dev::TraceLayer::new(log)),
            || {
                duckdb_dev::value("oversized", &"x".repeat(8192));
                duckdb_dev::flush();
            },
        );
        panic!("write budget was not enforced");
    }
    // The parent owns cleanup because the recorder exits without unwinding.
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "recording_write_budget_fails_explicitly_instead_of_filling_disk",
            "--nocapture",
        ])
        .env("DUCKDB_BUDGET_TEST_WORKER", directory.path())
        .env("DUCKDB_DEV_MAX_BYTES", "4096")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(74));
    assert!(String::from_utf8_lossy(&output.stderr).contains("trace write budget exceeded"));
}
