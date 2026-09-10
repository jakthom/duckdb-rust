use std::{
    io::{self, Read, Write},
    process::ExitCode,
};

use duckdb_rust::storage::checkpoint::policy::{
    CheckpointPolicy, CommitCountCheckpoint, LogSizeCheckpoint,
};
use duckdb_rust::{
    DatabaseBuilder, Error, Result,
    main::QueryResult,
    storage::{
        checkpoint::{Durability, FileCheckpoint},
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn run() -> Result<()> {
    let mut path = None;
    let mut sql = None;
    let mut json = false;
    let mut adapters = false;
    let mut format = "duckdb".to_owned();
    let mut mode = OpenMode::ReadWrite;
    let mut logged = false;
    let mut checkpoint_policy: Option<Arc<dyn CheckpointPolicy>> = None;
    let mut subqueries: Arc<dyn duckdb_rust::execution::subquery::SubqueryExecutor> =
        Arc::new(duckdb_rust::execution::subquery::StreamingSubqueries);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "Usage: duckdb-rust [DATABASE] [-c SQL] [--json] [--read-only] [--format duckdb|snapshot] [--durability checkpoint|wal]\nFiles use DuckDB checkpoints by default. Omit the path or use :memory: for memory.\nWithout -c, reads SQL from standard input. Unsupported file features return errors.\n--durability wal appends native transaction logs; requires a writable DuckDB file.\n--checkpoint-bytes N or --checkpoint-commits N selects automatic checkpoint scheduling.\nCHECKPOINT publishes acknowledged log work without closing the database.\n--subqueries streaming|materializing selects nested query consumption.\n--adapters prints the selected implementations instead of executing SQL."
                );
                return Ok(());
            }
            "-c" | "--command" => {
                sql = Some(
                    args.next()
                        .ok_or_else(|| Error::Parse("-c requires SQL".into()))?,
                )
            }
            "--json" | "-json" => json = true,
            "--adapters" => adapters = true,
            "--subqueries" => {
                use duckdb_rust::execution::subquery::{
                    MaterializingSubqueries, StreamingSubqueries,
                };
                subqueries = match args.next().as_deref() {
                    Some("streaming") => Arc::new(StreamingSubqueries),
                    Some("materializing") => Arc::new(MaterializingSubqueries),
                    _ => {
                        return Err(Error::Parse(
                            "--subqueries requires streaming or materializing".into(),
                        ));
                    }
                };
            }
            "--read-only" => mode = OpenMode::ReadOnly,
            "--durability" => {
                logged = match args.next().as_deref() {
                    Some("wal") => true,
                    Some("checkpoint") => false,
                    _ => {
                        return Err(Error::Parse(
                            "--durability requires checkpoint or wal".into(),
                        ));
                    }
                };
            }
            "--checkpoint-bytes" | "--checkpoint-commits" => {
                if checkpoint_policy.is_some() {
                    return Err(Error::Parse("select only one checkpoint policy".into()));
                }
                let limit = args
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .and_then(std::num::NonZeroU64::new)
                    .ok_or_else(|| Error::Parse(format!("{arg} requires a positive integer")))?;
                checkpoint_policy = Some(if arg == "--checkpoint-bytes" {
                    Arc::new(LogSizeCheckpoint(limit))
                } else {
                    Arc::new(CommitCountCheckpoint(limit))
                });
            }
            "--format" => {
                format = args
                    .next()
                    .ok_or_else(|| Error::Parse("--format requires duckdb or snapshot".into()))?
            }
            value if value.starts_with('-') => {
                return Err(Error::Parse(format!("unknown option {value}")));
            }
            value if path.is_none() => path = Some(value.to_string()),
            _ => return Err(Error::Parse("too many database paths".into())),
        }
    }
    let format: Arc<dyn SnapshotFormat> = match format.as_str() {
        "duckdb" => Arc::new(DuckDbFormat::default()),
        "snapshot" => Arc::new(JsonSnapshotFormat),
        _ => return Err(Error::Parse("--format requires duckdb or snapshot".into())),
    };
    if checkpoint_policy.is_some() && !logged {
        return Err(Error::Parse(
            "checkpoint policies require --durability wal".into(),
        ));
    }
    if logged
        && (mode == OpenMode::ReadOnly
            || format.format_id() != duckdb_rust::storage::format::DUCKDB_FORMAT
            || path.as_deref().is_none_or(|path| path == ":memory:"))
    {
        return Err(Error::Parse(
            "WAL durability requires a writable DuckDB file".into(),
        ));
    }
    let builder = DatabaseBuilder::new().subqueries(subqueries);
    let database = match path.as_deref() {
        None | Some(":memory:") => {
            if mode == OpenMode::ReadOnly {
                return Err(Error::Parse("--read-only requires a database file".into()));
            }
            builder.build()?
        }
        Some(path) => {
            let native = format.format_id() == duckdb_rust::storage::format::DUCKDB_FORMAT;
            let mut checkpoint = FileCheckpoint::open(path, mode, format)?;
            if native {
                checkpoint = checkpoint.with_recovery(Arc::new(
                    duckdb_rust::storage::duckdb::wal::DuckDbWalRecovery,
                ))?;
            }
            let durability: Arc<dyn Durability> = if logged {
                let mut wal = duckdb_rust::storage::logged::FileWal::new(
                    checkpoint,
                    Arc::new(duckdb_rust::storage::duckdb::wal::writer::DuckDbTransactionLog),
                )?;
                if let Some(policy) = checkpoint_policy {
                    wal = wal.with_checkpoint_policy(Some(policy));
                }
                Arc::new(wal)
            } else {
                Arc::new(checkpoint)
            };
            builder.durability(durability).build()?
        }
    };
    if adapters {
        if sql.is_some() {
            return Err(Error::Parse(
                "--adapters cannot be combined with a SQL command".into(),
            ));
        }
        let selected = database.adapters();
        serde_json::to_writer(io::stdout(), &selected)
            .map_err(|e| Error::Execution(e.to_string()))?;
        println!();
        return Ok(());
    }
    let mut connection = database.connect();
    let sql = match sql {
        Some(sql) => sql,
        None => {
            let mut sql = String::new();
            io::stdin().read_to_string(&mut sql)?;
            sql
        }
    };
    for result in connection.execute(&sql)? {
        print_result(&result, json)?;
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn print_result(result: &QueryResult, json: bool) -> Result<()> {
    if result.columns.is_empty() {
        return Ok(());
    }
    if result
        .rows
        .iter()
        .flatten()
        .any(|v| matches!(v, duckdb_rust::Value::Extension(_)))
    {
        return Err(Error::Unsupported(
            "shell output of registered values requires an explicit cast".into(),
        ));
    }
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if json {
        let mut names = std::collections::HashSet::new();
        if result.columns.iter().any(|c| !names.insert(&c.name)) {
            return Err(Error::Execution(
                "JSON objects require distinct column names; add aliases".into(),
            ));
        }
        let rows: Vec<serde_json::Value> = result
            .rows
            .iter()
            .map(|row| {
                let mut object = serde_json::Map::new();
                for (field, value) in result.columns.iter().zip(row) {
                    let value = match value {
                        duckdb_rust::Value::Extension(_) => {
                            unreachable!("rejected registered value output")
                        }
                        duckdb_rust::Value::Null => serde_json::Value::Null,
                        duckdb_rust::Value::Date(v) => serde_json::Value::String(v.to_string()),
                        duckdb_rust::Value::Blob(_) | duckdb_rust::Value::Uuid(_) => {
                            serde_json::Value::String(value.to_string())
                        }
                        duckdb_rust::Value::Boolean(v) => serde_json::Value::Bool(*v),
                        duckdb_rust::Value::Integer(v) => serde_json::from_str(&v.to_string())
                            .unwrap_or_else(|_| serde_json::Value::String(v.to_string())),
                        duckdb_rust::Value::Unsigned(_) | duckdb_rust::Value::Decimal { .. } => {
                            serde_json::from_str(&value.to_string())
                                .unwrap_or_else(|_| serde_json::Value::String(value.to_string()))
                        }
                        duckdb_rust::Value::Float(v) if v.is_finite() => serde_json::json!(v),
                        duckdb_rust::Value::Float(v) => serde_json::Value::String(v.to_string()),
                        duckdb_rust::Value::Double(v) if v.is_finite() => serde_json::json!(v),
                        duckdb_rust::Value::Double(v) => serde_json::Value::String(v.to_string()),
                        duckdb_rust::Value::Varchar(v) => serde_json::Value::String(v.clone()),
                    };
                    object.insert(field.name.clone(), value);
                }
                serde_json::Value::Object(object)
            })
            .collect();
        serde_json::to_writer(&mut output, &rows).map_err(|e| Error::Execution(e.to_string()))?;
        writeln!(output)?;
    } else {
        writeln!(
            output,
            "{}",
            result
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join("\t")
        )?;
        for row in &result.rows {
            writeln!(
                output,
                "{}",
                row.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\t")
            )?;
        }
    }
    Ok(())
}
