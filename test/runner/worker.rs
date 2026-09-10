//! JSON-lines verification transport. Every request uses the public Rust API.
use duckdb_rust::{Connection, Database, Error, Result, Value};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{self, BufRead, Write},
    path::PathBuf,
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
}

struct Session {
    database: Option<Database>,
    path: Option<PathBuf>,
    read_only: bool,
    connections: BTreeMap<String, Connection>,
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
                if self.path.is_none() {
                    return Err(Error::Unsupported(
                        "restart requires a file database".into(),
                    ));
                }
                self.open(self.path.clone(), self.read_only, false)?;
                return Ok(json!({"ok":true}));
            }
            "reconnect" => {
                self.connections.clear();
                return Ok(json!({"ok":true}));
            }
            "query" | "statement" => {}
            _ => {
                return Err(Error::Unsupported(
                    "unknown test transport operation".into(),
                ));
            }
        }
        let database = self
            .database
            .as_ref()
            .ok_or_else(|| Error::Execution("no open test database".into()))?;
        let connection = self
            .connections
            .entry(request.connection)
            .or_insert_with(|| database.connect());
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
fn main() -> Result<()> {
    let mut session = Session {
        database: Some(Database::memory()?),
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
            Err(error) => {
                json!({"ok":false,"unsupported":matches!(error,Error::Unsupported(_)),"message":error.to_string()})
            }
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
}
