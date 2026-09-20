//! One-shot two-connection D1 workload worker. Setup and both transaction
//! outcomes are inside the process so the public measurement includes the
//! snapshot/rebase/publication path rather than an isolated SQL kernel.
use duckdb_rust::{Database, Result};

fn scalar(connection: &mut duckdb_rust::Connection, sql: &str) -> Result<i128> {
    Ok(connection.query(sql)?.rows.into_iter().flatten().try_fold(0_i128, |sum, value| Ok(sum + value.as_i128()?))?)
}

fn main() -> Result<()> {
    let id = std::env::args().nth(1).ok_or_else(|| duckdb_rust::Error::Parse("expected D1 workload id".into()))?;
    let database = Database::memory()?; let mut left = database.connect(); let mut right = database.connect();
    let (rows, checksum, conflicts) = match id.as_str() {
        "d1-disjoint-row-writers" => { left.execute("CREATE TABLE t(i BIGINT PRIMARY KEY, v BIGINT); INSERT INTO t SELECT i,0 FROM range(10000) x(i)")?; left.execute("BEGIN; UPDATE t SET v=1 WHERE i=1")?; right.execute("BEGIN; UPDATE t SET v=2 WHERE i=2")?; left.execute("COMMIT")?; right.execute("COMMIT")?; (scalar(&mut left, "SELECT count(*) FROM t")?, scalar(&mut left, "SELECT sum(v) FROM t")?, 0) }
        "d1-contended-row-writers" => { left.execute("CREATE TABLE t(i BIGINT PRIMARY KEY, v BIGINT); INSERT INTO t VALUES (1,0)")?; left.execute("BEGIN; UPDATE t SET v=1 WHERE i=1")?; right.execute("BEGIN; DELETE FROM t WHERE i=1")?; left.execute("COMMIT")?; let conflict = right.execute("COMMIT").is_err(); if !conflict { return Err(duckdb_rust::Error::Execution("contended D1 workload accepted two winners".into())); }; (scalar(&mut left, "SELECT count(*) FROM t")?, scalar(&mut left, "SELECT sum(v) FROM t")?, 1) }
        "d1-catalog-disjoint-and-contended" => { left.execute("BEGIN; CREATE TABLE a(i BIGINT)")?; right.execute("BEGIN; CREATE TABLE b(i BIGINT)")?; left.execute("COMMIT")?; right.execute("COMMIT")?; left.execute("BEGIN; CREATE TABLE same(i BIGINT)")?; right.execute("BEGIN; CREATE TABLE same(i BIGINT)")?; left.execute("COMMIT")?; let conflict = right.execute("COMMIT").is_err(); if !conflict { return Err(duckdb_rust::Error::Execution("same catalog object accepted two winners".into())); }; (scalar(&mut left, "SELECT count(*) FROM information_schema.tables WHERE table_name IN ('a','b','same')")?, 0, 1) }
        "d1-retained-reader-publication" => { left.execute("CREATE TABLE t(i BIGINT); INSERT INTO t SELECT i FROM range(10000) x(i)")?; right.execute("BEGIN")?; left.execute("INSERT INTO t VALUES (10000)")?; let old = scalar(&mut right, "SELECT count(*) FROM t")?; right.execute("COMMIT")?; let fresh = scalar(&mut left, "SELECT count(*) FROM t")?; if old != 10000 || fresh != 10001 { return Err(duckdb_rust::Error::Execution("retained reader visibility differs".into())); }; (fresh, old + fresh, 0) }
        _ => return Err(duckdb_rust::Error::InvalidInput("unknown D1 workload".into())),
    };
    println!("{}", serde_json::json!({"schema":"d1-worker-v1","engine":"rust","id":id,"rows":rows,"checksum":checksum.to_string(),"conflicts":conflicts}));
    Ok(())
}
