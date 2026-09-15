#[path = "mod.rs"]
mod runner;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
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
    for path in paths {
        let result =
            duckdb_rust::Database::memory().and_then(|db| runner::run_file_report(&db, &path));
        match result {
            Ok(report) => match report.status {
                runner::FileStatus::Passed => {
                    passed += report.passed;
                    println!("PASS {} ({} records)", path.display(), report.passed);
                }
                runner::FileStatus::Skipped(reason) => {
                    skipped += report.skipped.max(1);
                    println!("SKIP {} ({reason})", path.display());
                }
            },
            Err(e) => {
                eprintln!("{e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    println!("{passed} records passed; {skipped} skipped");
    std::process::ExitCode::SUCCESS
}
