//! Retained defaults produced independently by both pinned C++ cores contain
//! post-v1.5 UNBOUND TypeExpression metadata for nested cast targets.
use super::*;
use std::{fs, io::Read};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixture(target: &str, destination: &std::path::Path) -> Result<()> {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("test/data/duckdb/unbound-default-{target}"))
        .join("unbound_default.duckdb.gz");
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(source)?).read_to_end(&mut bytes)?;
    fs::write(destination, bytes)?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn rows(connection: &mut duckdb_rust::Connection) -> Result<Vec<Vec<Value>>> {
    Ok(connection
        .query(
            "SELECT id,list_default::VARCHAR,array_cast_default::VARCHAR,
                    sorted_default::VARCHAR,array_sorted_default::VARCHAR
             FROM reference_list_defaults ORDER BY id",
        )?
        .rows
        .into_rows())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn reference_unbound_list_defaults_rebind_insert_checkpoint_and_reopen() -> Result<()> {
    let expected_row = vec![
        Value::Integer(1),
        Value::Varchar("[3, NULL, 1]".into()),
        Value::Varchar("[3, NULL, 1]".into()),
        Value::Varchar("[1, 3, NULL]".into()),
        Value::Varchar("[NULL, 3, 1]".into()),
    ];
    let directory = tempfile::tempdir()?;
    for target in ["development", "release"] {
        let path = directory.path().join(format!("{target}-default.duckdb"));
        fixture(target, &path)?;
        let mut connection = Database::open(&path)?.connect();
        assert_eq!(rows(&mut connection)?, vec![expected_row.clone()]);
        connection.execute("INSERT INTO reference_list_defaults(id) VALUES (2)")?;
        let mut expected = vec![expected_row.clone(), expected_row.clone()];
        expected[1][0] = Value::Integer(2);
        assert_eq!(rows(&mut connection)?, expected);
        connection.checkpoint()?;
        drop(connection);
        assert_eq!(
            rows(&mut Database::open_read_only(&path)?.connect())?,
            expected,
            "{target} reference-origin defaults after Rust checkpoint"
        );
    }
    Ok(())
}
