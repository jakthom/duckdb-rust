//! Measurement worker: setup is untimed; each request executes one prepared query.
use duckdb_rust::{Database, Error, Result};
use std::io::{self, BufRead, Write};
use std::time::Instant;

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 {
        return Err(Error::Parse("expected setup and query file paths".into()));
    }
    let database = Database::memory()?;
    let mut connection = database.connect();
    connection.execute(&std::fs::read_to_string(&arguments[0])?)?;
    let statement = connection.prepare(&std::fs::read_to_string(&arguments[1])?)?;
    println!(
        "{}",
        serde_json::json!({"ready":true,"engine":"rust","adapters":database.adapters()})
    );
    io::stdout().flush()?;
    for request in io::stdin().lock().lines() {
        if request? != "sample" {
            return Err(Error::Parse("expected sample request".into()));
        }
        let start = Instant::now();
        let result = connection.execute_prepared(&statement, &[])?;
        let mut sum = 0_i128;
        for row in &result.rows {
            for value in row {
                sum = sum
                    .checked_add(value.as_i128()?)
                    .ok_or_else(|| Error::Execution("checksum overflow".into()))?;
            }
        }
        let elapsed_ns = start.elapsed().as_nanos();
        println!(
            "{}",
            serde_json::json!({"elapsed_ns":elapsed_ns,"rows":result.rows.len(),"sum":sum.to_string()})
        );
        io::stdout().flush()?;
    }
    Ok(())
}
