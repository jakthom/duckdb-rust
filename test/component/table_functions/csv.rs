use super::*;
use std::io::Write;

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
        "SELECT * FROM read_csv('{path}', columns={{'id':'INTEGER','note':'VARCHAR','value':'VARCHAR'}}, header=true, nullstr='\\N')"
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
        "SELECT * FROM read_csv('/definitely/not/a/duckdb-rust-csv-file.csv', columns={'id':'INTEGER'})",
    );
    assert!(matches!(missing, Err(Error::Io(_))));

    let mut malformed = tempfile::NamedTempFile::new()?;
    malformed.write_all(b"1,\"unterminated")?;
    let malformed_path = sql_path(malformed.path());
    let mut connection = DatabaseBuilder::new().build()?.connect();
    assert!(connection
        .query(&format!(
            "SELECT * FROM read_csv('{malformed_path}', columns={{'id':'INTEGER','note':'VARCHAR'}})"
        ))
        .is_err());

    let mut short = tempfile::NamedTempFile::new()?;
    short.write_all(b"1\n")?;
    let short_path = sql_path(short.path());
    assert!(connection
        .query(&format!(
            "SELECT * FROM read_csv('{short_path}', columns={{'id':'INTEGER','note':'VARCHAR'}})"
        ))
        .is_err());
    Ok(())
}
