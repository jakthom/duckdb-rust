use duckdb_rust::{Database, Error, Result};
use std::{collections::BTreeMap, path::Path};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Each file owns its default and named sessions over the selected database.
/// A named session persists across records; transactions and settings never
/// transfer between sessions. All connections use the database's composition.
pub fn run_file(database: &Database, path: &Path) -> Result<usize> {
    let source = std::fs::read_to_string(path)?;
    let mut connections = BTreeMap::new();
    let lines: Vec<_> = source.lines().collect();
    let mut position = 0;
    let mut records = 0;
    while position < lines.len() {
        let line = lines[position].trim();
        position += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let location = position;
        let directive: Vec<_> = line.split_whitespace().collect();
        let mut sql = Vec::new();
        while position < lines.len()
            && !lines[position].trim().is_empty()
            && lines[position].trim() != "----"
        {
            sql.push(lines[position]);
            position += 1;
        }
        let mut expected = Vec::new();
        if position < lines.len() && lines[position].trim() == "----" {
            position += 1;
            while position < lines.len() && !lines[position].is_empty() {
                expected.push(lines[position].to_string());
                position += 1;
            }
        }
        let sql = sql.join("\n");
        let fail = |message: String| {
            Error::Execution(format!("{}:{location}: {message}\n{sql}", path.display()))
        };
        let name = match directive.as_slice() {
            ["statement", "ok" | "error", name] => *name,
            ["query", _, name] if !matches!(*name, "nosort" | "rowsort" | "valuesort") => *name,
            _ => "",
        };
        let connection = connections
            .entry(name.to_string())
            .or_insert_with(|| database.connect());
        match directive.as_slice() {
            ["statement", "ok"] | ["statement", "ok", _] => {
                connection.execute(&sql).map_err(|e| fail(e.to_string()))?;
            }
            ["statement", "error"] | ["statement", "error", _] => {
                let error = match connection.execute(&sql) {
                    Err(e @ Error::Unsupported(_)) => return Err(fail(e.to_string())),
                    Err(e) => e.to_string(),
                    Ok(_) => return Err(fail("expected an error".into())),
                };
                if !expected.is_empty() && !error.contains(&expected.join("\n")) {
                    return Err(fail(format!(
                        "error {error:?} does not contain {expected:?}"
                    )));
                }
            }
            ["query", types] | ["query", types, _] => {
                let result = connection.query(&sql).map_err(|e| fail(e.to_string()))?;
                if result.columns.len() != types.len() {
                    return Err(fail(format!(
                        "expected {} columns, got {}",
                        types.len(),
                        result.columns.len()
                    )));
                }
                for (kind, field) in types.chars().zip(&result.columns) {
                    let valid = match kind {
                        'I' => {
                            field.data_type.is_integer()
                                || field.data_type == duckdb_rust::DataType::Boolean
                                || field.data_type == duckdb_rust::DataType::Null
                        }
                        'R' => field.data_type.is_numeric(),
                        'T' => true,
                        _ => false,
                    };
                    if !valid {
                        return Err(fail(format!(
                            "type {kind} does not match {}",
                            field.data_type
                        )));
                    }
                }
                let mut rows: Vec<Vec<String>> = result
                    .rows
                    .iter()
                    .map(|r| {
                        r.iter()
                            .map(|v| match v {
                                duckdb_rust::Value::Boolean(b) => i32::from(*b).to_string(),
                                _ => v.to_string(),
                            })
                            .collect()
                    })
                    .collect();
                if directive.get(2) == Some(&"rowsort") {
                    rows.sort();
                }
                let mut actual: Vec<String> = rows.into_iter().flatten().collect();
                let mut expected: Vec<String> = expected
                    .iter()
                    .flat_map(|line| line.split('\t').map(str::to_string))
                    .collect();
                if directive.get(2) == Some(&"valuesort") {
                    actual.sort();
                    expected.sort();
                }
                if actual != expected {
                    return Err(fail(format!("expected {expected:?}, got {actual:?}")));
                }
            }
            _ => {
                return Err(fail(format!(
                    "unsupported test directive {line:?}; it was not skipped"
                )));
            }
        }
        records += 1;
    }
    if records == 0 {
        return Err(Error::Execution(format!(
            "{} contains no test records",
            path.display()
        )));
    }
    Ok(records)
}
