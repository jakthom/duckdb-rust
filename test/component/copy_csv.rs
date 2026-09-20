use duckdb_rust::{DatabaseBuilder, Error, Result, Value};

fn literal_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn copy_csv_streams_table_and_query_bytes_with_pinned_defaults() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("rows.csv");
    let path = literal_path(&path);
    let mut connection = DatabaseBuilder::new().batch_size(1).build()?.connect();
    connection.execute("CREATE TABLE t(id INTEGER, note VARCHAR)")?;
    connection.execute("INSERT INTO t VALUES (1, 'plain'), (2, 'comma, \"quoted\"\nline'), (3, NULL), (4, '')")?;
    let result = connection.query(&format!("COPY t TO '{path}'"))?;
    assert_eq!(result.affected_rows, 4);
    assert_eq!(
        std::fs::read_to_string(&path)?,
        "id,note\n1,plain\n2,\"comma, \"\"quoted\"\"\nline\"\n3,\n4,\"\"\n"
    );
    assert_eq!(
        connection.query(&format!(
            "SELECT * FROM read_csv('{path}', columns={{'id':'INTEGER','note':'VARCHAR'}}, auto_detect=false, header=true) ORDER BY id"
        ))?.rows,
        vec![
            vec![Value::Integer(1), Value::Varchar("plain".into())],
            vec![Value::Integer(2), Value::Varchar("comma, \"quoted\"\nline".into())],
            vec![Value::Integer(3), Value::Null],
            vec![Value::Integer(4), Value::Varchar(String::new())],
        ]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn copy_csv_table_projection_query_options_and_failures_preserve_existing_target() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("selected.csv");
    let path = literal_path(&path);
    let mut connection = DatabaseBuilder::new().batch_size(2).build()?.connect();
    connection.execute("CREATE TABLE t(id INTEGER, note VARCHAR)")?;
    connection.execute("INSERT INTO t VALUES (1, 'x'), (2, NULL)")?;
    let result = connection.query(&format!(
        "COPY t (note, id) TO '{path}' WITH (DELIMITER '|', HEADER false, NULL '\\N')"
    ))?;
    assert_eq!(result.affected_rows, 2);
    assert_eq!(std::fs::read_to_string(&path)?, "x|1\n\\N|2\n");
    let result = connection.query(&format!(
        "COPY (SELECT id FROM t WHERE id = 2) TO '{path}' WITH (HEADER false)"
    ))?;
    assert_eq!(result.affected_rows, 1);
    assert_eq!(std::fs::read_to_string(&path)?, "2\n");
    std::fs::write(&path, b"old\n")?;
    assert!(connection.query(&format!(
        "COPY (SELECT CAST('bad' AS INTEGER)) TO '{path}'"
    )).is_err());
    assert_eq!(std::fs::read_to_string(&path)?, "old\n");
    for sql in [
        format!("COPY t FROM '{path}'"),
        "COPY t TO STDOUT".to_owned(),
        format!("COPY t TO '{path}' WITH (FORMAT parquet)"),
    ] {
        assert!(matches!(connection.query(&sql), Err(Error::Unsupported(_)) | Err(Error::Bind(_)) | Err(Error::Parse(_))), "{sql}");
    }
    Ok(())
}
