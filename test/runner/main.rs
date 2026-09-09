#[path = "mod.rs"]
mod runner;

fn main() -> std::process::ExitCode {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.is_empty() {
        eprintln!("Usage: sqllogictest FILE.test [...]");
        return std::process::ExitCode::FAILURE;
    }
    let mut passed = 0;
    for path in paths {
        let path = std::path::PathBuf::from(path);
        let result = duckdb_rust::Database::memory()
            .and_then(|db| runner::run_file(&mut db.connect(), &path));
        match result {
            Ok(count) => {
                passed += count;
                println!("PASS {} ({count} records)", path.display());
            }
            Err(e) => {
                eprintln!("{e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    println!("{passed} records passed; 0 skipped");
    std::process::ExitCode::SUCCESS
}
