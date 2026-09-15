//! JSON-lines verification transport. Every request uses the public Rust API.
use duckdb_rust::{Connection, Database, Error, Result, Value};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    hash::{BuildHasher, Hasher},
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Deserialize)]
struct Request {
    operation: String,
    #[serde(default)]
    sql: String,
    #[serde(default)]
    connection: String,
    path: Option<PathBuf>,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    streams: Vec<Vec<ConcurrentRequest>>,
    #[serde(default)]
    max_threads: usize,
}

#[derive(Clone, Deserialize)]
struct ConcurrentRequest {
    operation: String,
    #[serde(default)]
    sql: String,
    #[serde(default)]
    expect_error: bool,
}

struct Session {
    database: Option<Database>,
    named_databases: BTreeMap<String, Database>,
    path: Option<PathBuf>,
    read_only: bool,
    connections: BTreeMap<String, (String, Connection)>,
}

/// SQLLogicTest's `(empty)` and NUL escaping apply to rendered values, not
/// only VARCHAR payloads (upstream test/sqlite/result_helper.cpp).
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn logic_value(value: &Value) -> String {
    let text = match value {
        Value::Boolean(value) => u8::from(*value).to_string(),
        _ => value.to_string(),
    };
    if text.is_empty() {
        "(empty)".into()
    } else {
        text.replace('\0', "\\0")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Session {
    fn connection(&mut self, name: &str) -> Result<&mut Connection> {
        let (connection_name, target) = if name.contains(':') {
            let mut parts = name.split(':');
            let database_name = parts.next().unwrap_or_default();
            let connection_name = parts.next().unwrap_or_default();
            if database_name.is_empty() || connection_name.is_empty() || parts.next().is_some() {
                return Err(Error::Execution(
                    "Expected either connection name or database:connection".into(),
                ));
            }
            if !self.named_databases.contains_key(database_name)
                && self.connections.contains_key(connection_name)
            {
                return Err(Error::Execution(
                    "Database did not exist, but named connection already existed".into(),
                ));
            }
            if !self.named_databases.contains_key(database_name) {
                self.named_databases
                    .insert(database_name.to_string(), Database::memory()?);
            }
            (connection_name, database_name)
        } else {
            (name, "")
        };
        if let Some((existing_target, _)) = self.connections.get(connection_name)
            && existing_target != target
        {
            return Err(Error::Execution(
                "Named connection has been started with different target databases".into(),
            ));
        }
        if !self.connections.contains_key(connection_name) {
            let database = if target.is_empty() {
                self.database
                    .as_ref()
                    .ok_or_else(|| Error::Execution("no open test database".into()))?
            } else {
                self.named_databases
                    .get(target)
                    .expect("named database inserted")
            };
            self.connections.insert(
                connection_name.to_string(),
                (target.to_string(), database.connect()),
            );
        }
        Ok(&mut self
            .connections
            .get_mut(connection_name)
            .expect("connection inserted")
            .1)
    }

    fn open(&mut self, path: Option<PathBuf>, read_only: bool, fresh: bool) -> Result<()> {
        let path = path.filter(|path| path.as_os_str() != ":memory:");
        if let Some(path) = &path {
            let root = std::env::current_dir()?;
            let absolute = root.join(path);
            if !absolute
                .parent()
                .is_some_and(|p| p.canonicalize().is_ok_and(|p| p.starts_with(&root)))
                || absolute
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
                || std::fs::symlink_metadata(&absolute).is_ok_and(|m| m.is_symlink())
            {
                return Err(Error::Unsupported(
                    "test database path outside its scratch directory".into(),
                ));
            }
        }
        self.connections.clear();
        self.database = None;
        self.path = path;
        self.read_only = read_only;
        if fresh
            && !read_only
            && let Some(path) = &self.path
        {
            for suffix in ["", ".wal", ".wal.checkpoint"] {
                let mut file = path.as_os_str().to_os_string();
                file.push(suffix);
                if let Err(error) = std::fs::remove_file(file)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(error.into());
                }
            }
        }
        self.database = Some(match &self.path {
            Some(path) if read_only => Database::open_read_only(path)?,
            Some(path) => Database::open(path)?,
            None if read_only => {
                return Err(Error::Unsupported(
                    "read-only in-memory test database".into(),
                ));
            }
            None => Database::memory()?,
        });
        Ok(())
    }
    fn run(&mut self, request: Request) -> Result<serde_json::Value> {
        match request.operation.as_str() {
            "describe" => {
                let database = self
                    .database
                    .as_ref()
                    .ok_or_else(|| Error::Execution("no open test database".into()))?;
                return Ok(json!({"ok":true,"adapters":database.adapters()}));
            }
            "load" => {
                self.open(request.path, request.read_only, true)?;
                return Ok(json!({"ok":true}));
            }
            "restart" => {
                self.open(self.path.clone(), self.read_only, false)?;
                return Ok(json!({"ok":true}));
            }
            "reconnect" => {
                // Pinned SQLLogicTestRunner::Reconnect replaces only `con`.
                // Named connections remain live, including their transactions.
                self.connections.remove("");
                return Ok(json!({"ok":true}));
            }
            "concurrent" => {
                if request.streams.is_empty() {
                    return Err(Error::Execution(
                        "concurrent request requires at least one stream".into(),
                    ));
                }
                let database = self
                    .database
                    .as_ref()
                    .ok_or_else(|| Error::Execution("no open test database".into()))?
                    .clone();
                let max_threads = if request.max_threads == 0 {
                    request.streams.len()
                } else {
                    request.max_threads.max(1)
                };
                let streams = request.streams;
                let mut responses = vec![None; streams.len()];
                let order = shuffled_indexes(streams.len());
                let finished = AtomicBool::new(false);
                for batch in order.chunks(max_threads) {
                    let completed = std::thread::scope(|scope| {
                        let mut threads = Vec::with_capacity(batch.len());
                        for index in batch {
                            let database = database.clone();
                            let stream = &streams[*index];
                            let finished = &finished;
                            threads.push((
                                *index,
                                scope.spawn(move || {
                                    let mut connection = database.connect();
                                    let mut output = Vec::with_capacity(stream.len());
                                    for request in stream {
                                        if finished.load(Ordering::Acquire) {
                                            break;
                                        }
                                        let response = run_concurrent_request(
                                            &mut connection,
                                            request.clone(),
                                        );
                                        if !request.expect_error
                                            && !response["ok"].as_bool().unwrap_or(false)
                                        {
                                            finished.store(true, Ordering::Release);
                                        }
                                        output.push(response);
                                    }
                                    output
                                }),
                            ));
                        }
                        threads
                            .into_iter()
                            .map(|(index, thread)| {
                                thread.join().map(|value| (index, value)).map_err(|_| {
                                    Error::Internal("concurrent worker stream panicked".into())
                                })
                            })
                            .collect::<Result<Vec<_>>>()
                    })?;
                    for (index, stream) in completed {
                        responses[index] = Some(stream);
                    }
                }
                let responses: Vec<_> = responses.into_iter().map(Option::unwrap).collect();
                return Ok(json!({"ok":true,"streams":responses}));
            }
            "foreach" => {
                let connection = self.connection(&request.connection)?;
                let name = request.sql.replace('\'', "''");
                let result = connection
                    .query(&format!("SELECT unnest(getvariable('{name}')::VARCHAR[])"))?;
                let values = result
                    .rows
                    .iter()
                    .map(|row| {
                        row.first()
                            .map(logic_value)
                            .unwrap_or_else(|| "NULL".into())
                    })
                    .collect::<Vec<_>>();
                return Ok(json!({"ok":true,"values":values}));
            }
            "query" | "statement" => {}
            _ => {
                return Err(Error::Unsupported(
                    "unknown test transport operation".into(),
                ));
            }
        }
        let connection = self.connection(&request.connection)?;
        if request.operation == "statement" {
            connection.execute(&request.sql)?;
            return Ok(json!({"ok":true}));
        }
        let result = connection.query(&request.sql)?;
        for value in result.rows.iter().flatten() {
            duckdb_rust::common::temporal::check_text_renderable(value, &mut || Ok(()))?;
        }
        let rows: Vec<Vec<String>> = result
            .rows
            .iter()
            .map(|row| row.iter().map(logic_value).collect())
            .collect();
        Ok(
            json!({"ok":true,"columns":result.columns.iter().map(|f|f.data_type.to_string()).collect::<Vec<_>>(),"rows":rows}),
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn shuffled_indexes(count: usize) -> Vec<usize> {
    let state = std::collections::hash_map::RandomState::new();
    let mut random = state.build_hasher();
    random.write_usize(count);
    let mut indexes: Vec<_> = (0..count).collect();
    for upper in (1..count).rev() {
        let next = random.finish() as usize % (upper + 1);
        indexes.swap(upper, next);
        random.write_usize(upper);
    }
    indexes
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn run_concurrent_request(
    connection: &mut Connection,
    request: ConcurrentRequest,
) -> serde_json::Value {
    let result = match request.operation.as_str() {
        "statement" => connection.execute(&request.sql).map(|_| json!({"ok":true})),
        "query" => connection.query(&request.sql).and_then(|result| {
            for value in result.rows.iter().flatten() {
                duckdb_rust::common::temporal::check_text_renderable(value, &mut || Ok(()))?;
            }
            let rows: Vec<Vec<String>> = result
                .rows
                .iter()
                .map(|row| row.iter().map(logic_value).collect())
                .collect();
            Ok(json!({
                "ok":true,
                "columns":result.columns.iter().map(|field|field.data_type.to_string()).collect::<Vec<_>>(),
                "rows":rows
            }))
        }),
        _ => Err(Error::Unsupported(
            "concurrent streams support only statement and query".into(),
        )),
    };
    match result {
        Ok(response) => response,
        Err(error) => error_response(error),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn error_response(error: Error) -> serde_json::Value {
    json!({"ok":false,"unsupported":matches!(error,Error::Unsupported(_)),"message":error.to_string()})
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn main() -> Result<()> {
    let mut session = Session {
        database: Some(Database::memory()?),
        named_databases: BTreeMap::new(),
        path: None,
        read_only: false,
        connections: BTreeMap::new(),
    };
    for line in io::stdin().lock().lines() {
        let result = serde_json::from_str::<Request>(&line?)
            .map_err(|error| Error::Parse(error.to_string()))
            .and_then(|request| session.run(request));
        let response = match result {
            Ok(value) => value,
            Err(error) => error_response(error),
        };
        println!("{response}");
        io::stdout().flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn request(value: serde_json::Value) -> Request {
        serde_json::from_value(value).unwrap()
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn supported_rejections_remain_distinct_from_missing_capabilities() {
        for (error, unsupported, message) in [
            (
                Error::NotImplemented("recognized invalid combination".into()),
                false,
                "Not implemented Error: recognized invalid combination",
            ),
            (
                Error::Unsupported("missing adapter".into()),
                true,
                "Not implemented: missing adapter",
            ),
            (
                Error::Bind("type mismatch".into()),
                false,
                "Binder Error: type mismatch",
            ),
            (
                Error::Resource("budget".into()),
                false,
                "Resource limit exceeded: budget",
            ),
            (
                Error::Internal("invariant".into()),
                false,
                "Internal Error: invariant",
            ),
        ] {
            assert_eq!(
                error_response(error),
                json!({"ok":false,"unsupported":unsupported,"message":message})
            );
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn logic_empty_and_nul_rules_apply_after_rendering() -> Result<()> {
        for (value, expected) in [
            (Value::Null, "NULL"),
            (Value::Boolean(false), "0"),
            (Value::Boolean(true), "1"),
            (Value::Varchar(String::new()), "(empty)"),
            (Value::Blob(vec![]), "(empty)"),
            (Value::Blob(vec![0]), "\\x00"),
            (Value::Varchar("a\0b".into()), "a\\0b"),
            (Value::Varchar("NULL".into()), "NULL"),
            (Value::Varchar("(empty)".into()), "(empty)"),
        ] {
            assert_eq!(logic_value(&value), expected);
        }
        let mut session = Session {
            database: Some(Database::memory()?),
            named_databases: BTreeMap::new(),
            path: None,
            read_only: false,
            connections: BTreeMap::new(),
        };
        let request = serde_json::from_value(json!({
            "operation":"query",
            "sql":"SELECT from_base64(''),base64(''::BLOB),from_base64(NULL),from_base64('AA=='),[NULL::VARCHAR]"
        }))
        .unwrap();
        assert_eq!(
            session.run(request)?["rows"],
            json!([["(empty)", "(empty)", "NULL", "\\x00", "[NULL]"]])
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn reconnect_preserves_named_sessions_and_restart_resets_memory() -> Result<()> {
        let mut session = Session {
            database: Some(Database::memory()?),
            named_databases: BTreeMap::new(),
            path: None,
            read_only: false,
            connections: BTreeMap::new(),
        };
        session.run(request(json!({"operation":"statement","connection":"writer","sql":"CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (1)"})))?;
        session.run(request(
            json!({"operation":"statement","connection":"reader","sql":"BEGIN"}),
        ))?;
        session.run(request(
            json!({"operation":"statement","connection":"writer","sql":"INSERT INTO t VALUES (2)"}),
        ))?;
        session.run(request(json!({"operation":"reconnect"})))?;
        assert_eq!(
            session.run(request(
                json!({"operation":"query","connection":"reader","sql":"SELECT count(*) FROM t"})
            ))?["rows"],
            json!([["1"]])
        );
        assert_eq!(
            session.run(request(
                json!({"operation":"query","sql":"SELECT count(*) FROM t"})
            ))?["rows"],
            json!([["2"]])
        );

        session.run(request(json!({"operation":"statement","connection":"aux:c1","sql":"CREATE TABLE isolated(i INTEGER); INSERT INTO isolated VALUES (7)"})))?;
        assert_eq!(
            session.run(request(
                json!({"operation":"query","connection":"aux:c2","sql":"SELECT i FROM isolated"})
            ))?["rows"],
            json!([["7"]])
        );
        assert!(
            session
                .run(request(
                    json!({"operation":"query","sql":"SELECT i FROM isolated"})
                ))
                .is_err()
        );

        session.run(request(json!({"operation":"restart"})))?;
        assert!(
            session
                .run(request(
                    json!({"operation":"query","sql":"SELECT * FROM t"})
                ))
                .is_err()
        );
        assert_eq!(
            session.run(request(
                json!({"operation":"query","connection":"aux:c3","sql":"SELECT i FROM isolated"})
            ))?["rows"],
            json!([["7"]])
        );
        session.run(request(
            json!({"operation":"query","connection":"other:x","sql":"SELECT 1"}),
        ))?;
        let error = session
            .run(request(
                json!({"operation":"query","connection":"other:c3","sql":"SELECT 1"}),
            ))
            .unwrap_err();
        assert!(error.to_string().contains("different target databases"));
        session.run(request(
            json!({"operation":"query","connection":"plain","sql":"SELECT 1"}),
        ))?;
        let error = session
            .run(request(
                json!({"operation":"query","connection":"newdb:plain","sql":"SELECT 1"}),
            ))
            .unwrap_err();
        assert!(error.to_string().contains("Database did not exist"));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn concurrent_stream_failure_short_circuits_later_commands() -> Result<()> {
        let mut session = Session {
            database: Some(Database::memory()?),
            named_databases: BTreeMap::new(),
            path: None,
            read_only: false,
            connections: BTreeMap::new(),
        };
        let response = session.run(request(json!({
            "operation":"concurrent",
            "max_threads":1,
            "streams":[
                [
                    {"operation":"query","sql":"SELECT missing"},
                    {"operation":"query","sql":"SELECT 1"}
                ]
            ]
        })))?;
        let streams = response["streams"].as_array().unwrap();
        assert_eq!(streams[0].as_array().unwrap().len(), 1);
        assert!(!streams[0][0]["ok"].as_bool().unwrap());
        Ok(())
    }
}
