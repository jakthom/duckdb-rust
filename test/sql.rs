#[path = "runner/mod.rs"]
mod runner;

#[test]
fn sql_logic_corpus() -> duckdb_rust::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/sql");
    let mut paths = std::fs::read_dir(root)?
        .map(|p| p.map(|p| p.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    assert!(!paths.is_empty());
    for path in paths {
        if path.extension().is_some_and(|s| s == "test") {
            runner::run_file(&mut duckdb_rust::Database::memory()?.connect(), &path)?;
        }
    }
    Ok(())
}
