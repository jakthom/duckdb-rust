mod casts;
mod compression;
mod execution;
mod operators;
mod subqueries;
mod types;

use duckdb_rust::{Error, Result};

fn main() -> Result<()> {
    let mut rows = 50_000usize;
    let mut iterations = 5usize;
    let mut batch_size = 256usize;
    let mut suite = "execution".to_owned();
    let mut arguments = std::env::args().skip(1);
    while let Some(option) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| Error::Parse(format!("{option} needs a value")))?;
        if option == "--suite" {
            suite = value;
            continue;
        }
        let target = match option.as_str() {
            "--rows" => &mut rows,
            "--iterations" => &mut iterations,
            "--batch-size" => &mut batch_size,
            _ => return Err(Error::Parse(format!("unknown benchmark option {option}"))),
        };
        *target = value
            .parse()
            .map_err(|_| Error::Parse(format!("{option} needs a positive integer")))?;
    }
    if rows == 0 || rows > 1_000_000 || iterations == 0 || batch_size == 0 {
        return Err(Error::Resource(
            "benchmark requires 1..1000000 rows and positive iterations/batch size".into(),
        ));
    }
    let report = match suite.as_str() {
        "execution" => execution::run(rows, iterations, batch_size)?,
        "compression" => compression::run(rows, iterations)?,
        "casts" => casts::run(rows, iterations, batch_size)?,
        "types" => types::run(rows, iterations, batch_size)?,
        "operators" => operators::run(rows, iterations, batch_size)?,
        "subqueries" => subqueries::run(rows, iterations, batch_size)?,
        _ => return Err(Error::Parse(format!("unknown benchmark suite {suite}"))),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| Error::Execution(e.to_string()))?
    );
    Ok(())
}
