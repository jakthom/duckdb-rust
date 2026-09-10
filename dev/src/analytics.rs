//! Optional SQL analysis through the external, version-checked DuckDB CLI.
//! Neither the recorder nor the production engine links DuckDB or depends on it.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Instant, UNIX_EPOCH},
};

const SCHEMA: &str = "{kind:'VARCHAR',seq:'UBIGINT',at_ns:'UBIGINT',site:'UBIGINT',span:'UBIGINT',parent:'UBIGINT',elapsed_ns:'UBIGINT',operation:'VARCHAR',module:'VARCHAR',file:'VARCHAR',line:'UBIGINT',thread:'VARCHAR',thread_name:'VARCHAR',fields:'JSON',statement:'JSON',pid:'UBIGINT',unix_ns:'UBIGINT',command:'JSON',run:'VARCHAR',source:'VARCHAR',profile:'VARCHAR',schema:'UBIGINT',timing:'VARCHAR',follows:'UBIGINT'}";
const SPANS: &str = "SELECT s.filename, s.span, s.parent, s.site, s.at_ns AS started_ns,
    e.at_ns AS ended_ns, e.elapsed_ns, e.fields->>'outcome' AS outcome, e.fields->>'error' AS error,
    m.operation, m.module, m.file, m.line, s.thread, s.thread_name,
    s.fields AS input_fields, e.fields AS output_fields
    FROM events s LEFT JOIN events e ON e.filename=s.filename AND e.span=s.span AND e.kind='end'
    LEFT JOIN events m ON m.filename=s.filename AND m.site=s.site AND m.kind='site'
    WHERE s.kind='start'";
const STATS: &str = "SELECT module, operation, file, line, count(elapsed_ns) AS calls,
    count(*) FILTER (WHERE elapsed_ns IS NULL) AS open_spans, count(*) FILTER (WHERE outcome='error') AS errors,
    count(*) FILTER (WHERE outcome='panic') AS panics, sum(elapsed_ns) AS total_ns, max(elapsed_ns) AS max_ns
    FROM spans GROUP BY module, operation, file, line";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct Input {
    path: PathBuf,
    bytes: u64,
    modified_ns: u128,
}

#[derive(Deserialize, Serialize)]
struct Cache {
    inputs: Vec<Input>,
    analyzer: String,
}

fn analyzer() -> String {
    format!("{:x}", Sha256::digest(include_bytes!("analytics.rs")))
}

fn inputs(directory: &Path) -> io::Result<Vec<Input>> {
    let files = crate::report::trace_files(directory)?;
    if files.is_empty() {
        return Err(io::Error::other("no trace files in this directory"));
    }
    files
        .into_iter()
        .map(|path| {
            let metadata = fs::metadata(&path)?;
            Ok(Input {
                path: path.canonicalize()?,
                bytes: metadata.len(),
                modified_ns: metadata
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .map_err(io::Error::other)?
                    .as_nanos(),
            })
        })
        .collect()
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn reader(inputs: &[Input]) -> io::Result<String> {
    let paths = inputs
        .iter()
        .map(|input| {
            input
                .path
                .to_str()
                .map(literal)
                .ok_or_else(|| io::Error::other("DuckDB trace paths must be UTF-8"))
        })
        .collect::<io::Result<Vec<_>>>()?
        .join(",");
    Ok(format!(
        "read_ndjson([{paths}], columns={SCHEMA}, auto_detect=false, filename=true, ignore_errors=false)"
    ))
}

fn backend() -> io::Result<PathBuf> {
    let executable = env::var_os("DUCKDB_DEV_DUCKDB")
        .map(PathBuf::from)
        .unwrap_or_else(|| "duckdb".into());
    let output = Command::new(&executable).arg("--version").output()?;
    let version = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || version.split_whitespace().next() != Some("v1.5.5") {
        return Err(io::Error::other(format!(
            "trace analysis requires DuckDB v1.5.5; found {}",
            version.trim()
        )));
    }
    eprintln!("trace analysis: {}", version.trim());
    Ok(executable)
}

fn execute(
    executable: &Path,
    database: Option<&Path>,
    read_only: bool,
    sql: &str,
) -> io::Result<()> {
    let mut command = Command::new(executable);
    command.args(["-batch", "-bail", "-json", "-init", "/dev/null"]);
    if read_only {
        command.arg("-readonly");
    }
    if let Some(database) = database {
        command.arg(database);
    }
    let mut child = command.stdin(Stdio::piped()).spawn()?;
    let write = child
        .stdin
        .take()
        .expect("piped SQL input")
        .write_all(sql.as_bytes());
    let status = child.wait()?;
    write?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "DuckDB trace query failed: {status}"
        )));
    }
    Ok(())
}

/// Run SQL over raw records, or use a verified, up-to-date columnar cache.
/// The raw path reparses JSON; importing once avoids that cost for repeated queries.
pub fn query(directory: &Path, sql: &str) -> io::Result<()> {
    let executable = backend()?;
    let before = inputs(directory)?;
    let database = directory.join("trace.duckdb");
    let cached = if database.is_file() {
        let saved: Cache = serde_json::from_slice(&fs::read(directory.join("trace.inputs.json"))?)?;
        if saved.inputs != before || saved.analyzer != analyzer() {
            return Err(io::Error::other(
                "trace changed since import or analyzer changed; run cargo dev index again",
            ));
        }
        true
    } else {
        false
    };
    let setup = if cached {
        String::new()
    } else {
        format!(
            "CREATE TEMP VIEW events AS SELECT * FROM {};\nCREATE TEMP VIEW spans AS {SPANS};\nCREATE TEMP VIEW operation_stats AS {STATS};\n",
            reader(&before)?
        )
    };
    let started = Instant::now();
    execute(
        &executable,
        cached.then_some(database.as_path()),
        cached,
        &format!("{setup}\n{sql}\n"),
    )?;
    if before != inputs(directory)? {
        return Err(io::Error::other(
            "trace changed during query; result covers a moving prefix",
        ));
    }
    eprintln!(
        "trace query: {:.3} s ({})",
        started.elapsed().as_secs_f64(),
        if cached { "DuckDB cache" } else { "raw JSONL" }
    );
    Ok(())
}

/// Explicit import keeps the recording path Rust-only and avoids an expensive
/// full scan after every iteration. Publish the cache only after successful import.
pub fn index(directory: &Path) -> io::Result<()> {
    let executable = backend()?;
    let before = inputs(directory)?;
    struct Lock(PathBuf);
    impl Drop for Lock {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let lock = directory.join("trace.index.lock");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)?;
    let _lock = Lock(lock);
    let temporary = directory.join(format!("trace.import-{}.duckdb", std::process::id()));
    if temporary.exists() {
        return Err(io::Error::other("unfinished trace import already exists"));
    }
    let started = Instant::now();
    let sql = format!(
        "SET memory_limit='2GB'; SET threads=4; SET preserve_insertion_order=false;
        BEGIN; CREATE TABLE events AS SELECT * FROM {};
        SELECT CASE WHEN count(*) != count(DISTINCT seq) OR min(seq) != 1 OR max(seq) != count(*)
            THEN error('trace sequence gap or duplicate') END FROM events GROUP BY filename;
        CREATE TABLE spans AS {SPANS};
        CREATE TABLE operation_stats AS {STATS}; COMMIT; CHECKPOINT;",
        reader(&before)?
    );
    let result = execute(&executable, Some(&temporary), false, &sql).and_then(|()| {
        if before != inputs(directory)? {
            return Err(io::Error::other(
                "trace changed during import; wait for statement completion",
            ));
        }
        fs::rename(&temporary, directory.join("trace.duckdb"))?;
        crate::statement::atomic_json(
            &directory.join("trace.inputs.json"),
            &Cache {
                inputs: before.clone(),
                analyzer: analyzer(),
            },
        )
    });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(temporary.with_extension("duckdb.wal"));
    }
    result?;
    eprintln!(
        "trace import: {} bytes in {:.3} s; {}",
        before.iter().map(|input| input.bytes).sum::<u64>(),
        started.elapsed().as_secs_f64(),
        directory.join("trace.duckdb").display()
    );
    Ok(())
}
