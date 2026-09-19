use super::*;
use duckdb_rust::common::cast::{CastFunction, CastMode, CastRegistry, CastSpec};
use std::io::Write;

#[derive(Debug)]
struct CsvSelectedInteger;

#[derive(Debug)]
struct CsvSelectedIdentity(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for CsvSelectedIdentity {
    fn name(&self) -> &'static str {
        "csv-selected-identity"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar && spec.target == DataType::Varchar
    }
    fn cast(&self, value: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(value.clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for CsvSelectedInteger {
    fn name(&self) -> &'static str {
        "csv-selected-integer"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target == DataType::Integer
            && spec.mode == CastMode::Explicit
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        Ok(Value::Integer(77))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sql_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn explicit_schema_csv_reads_quotes_nulls_boundaries_and_prepared_reopens() -> Result<()> {
    let mut file = tempfile::NamedTempFile::new()?;
    let long = "x".repeat(4096 + 23);
    write!(
        file,
        "id,note,value\r\n1,\"has, comma\",\\N\r\n2,\"{long}\",\"\\N\"\r\n"
    )?;
    let path = sql_path(file.path());
    let sql = format!(
        "SELECT * FROM read_csv('{path}', columns={{'id':'INTEGER','note':'VARCHAR','value':'VARCHAR'}}, auto_detect=false, header=true, nullstr='\\N')"
    );
    let mut connection = DatabaseBuilder::new().batch_size(1).build()?.connect();
    let expected = vec![
        vec![
            Value::Integer(1),
            Value::Varchar("has, comma".into()),
            Value::Null,
        ],
        vec![Value::Integer(2), Value::Varchar(long), Value::Null],
    ];
    assert_eq!(connection.query(&sql)?.rows, expected);
    let quoted_nulls_disabled = sql.replacen(
        "nullstr='\\N')",
        "nullstr='\\N', allow_quoted_nulls=false)",
        1,
    );
    assert_eq!(
        connection.query(&quoted_nulls_disabled)?.rows[1][2],
        Value::Varchar("\\N".into())
    );
    let prepared = connection.prepare(&sql)?;
    assert_eq!(connection.execute_prepared(&prepared, &[])?.rows.len(), 2);
    assert_eq!(connection.execute_prepared(&prepared, &[])?.rows.len(), 2);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn explicit_schema_csv_reports_open_parse_and_shape_failures() -> Result<()> {
    let missing = DatabaseBuilder::new().build()?.connect().query(
        "SELECT * FROM read_csv('/definitely/not/a/duckdb-rust-csv-file.csv', columns={'id':'INTEGER'}, auto_detect=false)",
    );
    assert!(matches!(missing, Err(Error::Io(_))));

    let mut malformed = tempfile::NamedTempFile::new()?;
    malformed.write_all(b"1,\"unterminated")?;
    let malformed_path = sql_path(malformed.path());
    let mut connection = DatabaseBuilder::new().build()?.connect();
    assert!(connection
        .query(&format!(
            "SELECT * FROM read_csv('{malformed_path}', columns={{'id':'INTEGER','note':'VARCHAR'}}, auto_detect=false)"
        ))
        .is_err());

    let mut short = tempfile::NamedTempFile::new()?;
    short.write_all(b"1\n")?;
    let short_path = sql_path(short.path());
    assert!(connection
        .query(&format!(
            "SELECT * FROM read_csv('{short_path}', columns={{'id':'INTEGER','note':'VARCHAR'}}, auto_detect=false)"
        ))
        .is_err());

    let directory = tempfile::tempdir()?;
    assert!(matches!(
        connection.query(&format!(
            "SELECT * FROM read_csv('{}', columns={{'id':'INTEGER'}}, auto_detect=false)",
            sql_path(directory.path())
        )),
        Err(Error::Io(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn explicit_schema_csv_handles_empty_blank_header_and_duplicate_options() -> Result<()> {
    let mut blank = tempfile::NamedTempFile::new()?;
    blank.write_all(b"a\n\nb\n")?;
    let path = sql_path(blank.path());
    let mut connection = DatabaseBuilder::new().build()?.connect();
    assert_eq!(
        connection
            .query(&format!(
                "SELECT * FROM read_csv('{path}', columns={{'v':'VARCHAR'}}, auto_detect=false)"
            ))?
            .rows,
        vec![
            vec![Value::Varchar("a".into())],
            vec![Value::Null],
            vec![Value::Varchar("b".into())]
        ]
    );
    assert!(connection
        .query(&format!(
            "SELECT * FROM read_csv('{path}', columns={{'v':'VARCHAR'}}, header=false, header=true)"
        ))
        .is_err());
    assert!(
        connection
            .query(&format!(
                "SELECT * FROM read_csv('{path}', columns={{'v':'VARCHAR'}})"
            ))
            .is_err()
    );
    let header_only = tempfile::NamedTempFile::new()?;
    assert!(
        connection
            .query(&format!(
            "SELECT * FROM read_csv('{}', columns={{'v':'VARCHAR'}}, auto_detect=false, header=true)",
                sql_path(header_only.path())
            ))?
            .rows
            .is_empty()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn explicit_schema_csv_uses_the_selected_varchar_cast() -> Result<()> {
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(b"not an integer\n")?;
    let mut casts = CastRegistry::builtins();
    casts.replace(
        CastSpec {
            source: DataType::Varchar,
            target: DataType::Integer,
            mode: CastMode::Explicit,
        },
        Arc::new(CsvSelectedInteger),
    )?;
    let mut connection = DatabaseBuilder::new().casts(casts).build()?.connect();
    assert_eq!(
        connection
            .query(&format!(
                "SELECT * FROM read_csv('{}', columns={{'v':'INTEGER'}}, auto_detect=false)",
                sql_path(file.path())
            ))?
            .rows,
        vec![vec![Value::Integer(77)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn csv_owned_columns_retain_selected_identity_callbacks_and_nulls() -> Result<()> {
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all("α,first\nNULL,second\nlast,NULL\n".as_bytes())?;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut casts = CastRegistry::builtins();
    casts.replace(
        CastSpec {
            source: DataType::Varchar,
            target: DataType::Varchar,
            mode: CastMode::Explicit,
        },
        Arc::new(CsvSelectedIdentity(calls.clone())),
    )?;
    let mut connection = DatabaseBuilder::new()
        .casts(casts)
        .batch_size(2)
        .build()?
        .connect();
    let query = format!(
        "SELECT * FROM read_csv('{}', columns={{'a':'VARCHAR','b':'VARCHAR'}}, auto_detect=false, nullstr='NULL')",
        sql_path(file.path()),
    );
    assert_eq!(
        connection.query(&query)?.rows,
        vec![
            vec![Value::Varchar("α".into()), Value::Varchar("first".into())],
            vec![Value::Null, Value::Varchar("second".into())],
            vec![Value::Varchar("last".into()), Value::Null],
        ]
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    Ok(())
}

#[derive(Debug)]
struct CsvOrderedCall(Arc<std::sync::Mutex<Vec<Value>>>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for CsvOrderedCall {
    fn name(&self) -> &'static str {
        "csv-ordered-call"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar && spec.target == DataType::Varchar
    }
    fn null_handling(&self, _: &CastSpec) -> duckdb_rust::common::cast::CastNullHandling {
        duckdb_rust::common::cast::CastNullHandling::Call
    }
    fn cast(&self, value: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        self.0.lock().unwrap().push(value.clone());
        if value == &Value::Varchar("stop".into()) {
            return Err(Error::Conversion("ordered CSV stop".into()));
        }
        Ok(value.clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn csv_arena_preserves_row_major_callbacks_null_calls_and_first_cast_error() -> Result<()> {
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(b"a,NULL\nc,stop\nd,e\n")?;
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut casts = CastRegistry::builtins();
    casts.replace(
        CastSpec {
            source: DataType::Varchar,
            target: DataType::Varchar,
            mode: CastMode::Explicit,
        },
        Arc::new(CsvOrderedCall(calls.clone())),
    )?;
    let mut connection = DatabaseBuilder::new()
        .casts(casts)
        .batch_size(8)
        .build()?
        .connect();
    let error = connection.query(&format!(
        "SELECT * FROM read_csv('{}', columns={{'a':'VARCHAR','b':'VARCHAR'}}, auto_detect=false, nullstr='NULL')",
        sql_path(file.path()),
    ));
    assert!(matches!(error, Err(Error::Conversion(message)) if message == "ordered CSV stop"));
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            Value::Varchar("a".into()),
            Value::Null,
            Value::Varchar("c".into()),
            Value::Varchar("stop".into())
        ],
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn csv_packed_projected_ctas_owns_values_after_reader_and_other_columns_drop() -> Result<()> {
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all("1,α,ignored\n2,NULL,also ignored\n3,\"\",more\n4,🦆,last\n".as_bytes())?;
    let mut connection = DatabaseBuilder::new().batch_size(2).build()?.connect();
    connection.execute(&format!(
        "CREATE TABLE retained AS SELECT label FROM read_csv('{}', columns={{'id':'INTEGER','label':'VARCHAR','unused':'VARCHAR'}}, auto_detect=false, nullstr='NULL')",
        sql_path(file.path()),
    ))?;
    file.close()?;
    assert_eq!(
        connection.query("SELECT label FROM retained")?.rows,
        vec![
            vec![Value::Varchar("α".into())],
            vec![Value::Null],
            vec![Value::Varchar("".into())],
            vec![Value::Varchar("🦆".into())],
        ]
    );
    assert_eq!(
        connection
            .query("SELECT count(label), sum(length(label)) FROM retained")?
            .rows,
        vec![vec![Value::Integer(3), Value::Integer(2)]]
    );
    assert_eq!(
        connection
            .query("SELECT label FROM retained WHERE label = 'α'")?
            .rows,
        vec![vec![Value::Varchar("α".into())]]
    );
    Ok(())
}
