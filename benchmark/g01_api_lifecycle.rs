//! Public Database/Connection lifecycle Gate P candidate.
use duckdb_rust::{Database, Error, Result};

fn scalar(result: duckdb_rust::QueryResult) -> Result<i128> {
    result
        .rows
        .iter()
        .next()
        .and_then(|row| row.first())
        .ok_or_else(|| Error::Execution("missing lifecycle value".into()))?
        .as_i128()
}

fn main() -> Result<()> {
    let db = Database::memory()?;
    let mut survivor = db.connect();
    survivor.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (1)")?;
    {
        let mut short_lived = db.connect();
        short_lived.execute("BEGIN; UPDATE t SET i=2; CREATE TABLE rolled_back(i INTEGER)")?;
    }
    if scalar(survivor.query("SELECT i FROM t")?)? != 1 {
        return Err(Error::Execution("dropped transaction committed".into()));
    }
    if survivor.query("SELECT * FROM rolled_back").is_ok() {
        return Err(Error::Execution("dropped transaction retained DDL".into()));
    }
    survivor.execute("CREATE TABLE after_drop(i INTEGER); INSERT INTO after_drop VALUES (7)")?;
    if scalar(survivor.query("SELECT i FROM after_drop")?)? != 7 {
        return Err(Error::Execution("surviving connection unusable".into()));
    }
    println!("G01_API_LIFECYCLE_PASS 3");
    Ok(())
}
