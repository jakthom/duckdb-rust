//! Measurement worker: setup is untimed; each request executes one prepared query.
use duckdb_rust::{Database, Error, Result};
use std::io::{self, BufRead, Write};
use std::time::Instant;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if !matches!(arguments.len(), 2 | 4) {
        return Err(Error::Parse(
            "expected setup/query and optional reset/verification paths".into(),
        ));
    }
    let database = Database::memory()?;
    let mut connection = database.connect();
    connection.execute(&std::fs::read_to_string(&arguments[0])?)?;
    let phases = if arguments.len() == 4 {
        let reset = std::fs::read_to_string(&arguments[2])?;
        let verify = std::fs::read_to_string(&arguments[3])?;
        connection.execute(&reset)?;
        Some((reset, verify))
    } else {
        None
    };
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
        if let Some((reset, _)) = &phases {
            connection.execute(reset)?;
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
        let mut rows = result.rows.len();
        if let Some((_, verify)) = &phases {
            if rows != 0 {
                return Err(Error::Execution(
                    "DDL measurement unexpectedly returned rows".into(),
                ));
            }
            // Verify the mutation's effect after timing. Every sample starts
            // from the declared reset state, so repeated ALTERs are real work.
            let result = connection.query(verify)?;
            rows = result.rows.len();
            for row in &result.rows {
                for value in row {
                    sum = sum
                        .checked_add(value.as_i128()?)
                        .ok_or_else(|| Error::Execution("checksum overflow".into()))?;
                }
            }
        }
        println!(
            "{}",
            serde_json::json!({"elapsed_ns":elapsed_ns,"rows":rows,"sum":sum.to_string()})
        );
        io::stdout().flush()?;
    }
    Ok(())
}
