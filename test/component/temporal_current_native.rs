//! Independently produced native defaults retain the special SQL value node
//! separately from its callable FUNCTION aliases.
use super::*;
use duckdb_rust::catalog::{TableName, expression::StoredExpressionKind};
use std::{fs, io::Read};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixture(target: &str, destination: &std::path::Path) -> Result<()> {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("test/data/duckdb/current-timestamp-{target}"))
        .join("current_timestamp.duckdb.gz");
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(source)?).read_to_end(&mut bytes)?;
    fs::write(destination, bytes)?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn assert_batch(connection: &mut duckdb_rust::Connection, first: i32, last: i32) -> Result<()> {
    let rows = connection
        .query(&format!(
            "SELECT keyword=get_call,keyword=now_call,keyword=transaction_call,keyword
             FROM reference_current_defaults
             WHERE id BETWEEN {first} AND {last} ORDER BY id"
        ))?
        .rows;
    assert_eq!(rows.len(), usize::try_from(last - first + 1).unwrap());
    let timestamp = rows[0][3].clone();
    for row in rows {
        assert_eq!(
            row,
            vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                timestamp.clone(),
            ]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn reference_current_defaults_insert_checkpoint_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for target in ["development", "release"] {
        let path = directory.path().join(format!("current-{target}.duckdb"));
        fixture(target, &path)?;
        let mut connection = Database::open(&path)?.connect();
        let definition =
            connection.resolve_table(&TableName::main("reference_current_defaults"))?;
        let columns = &definition.definition().columns;
        assert!(matches!(
            columns[1].default.as_ref().map(|default| &default.kind),
            Some(StoredExpressionKind::CurrentTimestamp)
        ));
        for (column, name) in
            columns[2..]
                .iter()
                .zip(["get_current_timestamp", "now", "transaction_timestamp"])
        {
            assert!(
                matches!(column.default.as_ref().map(|default| &default.kind),
                    Some(StoredExpressionKind::Function { name: parts, arguments, .. })
                    if parts == &[name] && arguments.is_empty())
            );
        }
        assert_batch(&mut connection, 1, 2)?;

        connection.execute(
            "BEGIN TRANSACTION;
             INSERT INTO reference_current_defaults(id) VALUES (3);
             INSERT INTO reference_current_defaults(id) VALUES (4);
             COMMIT",
        )?;
        assert_batch(&mut connection, 3, 4)?;
        connection.checkpoint()?;
        drop(connection);

        let mut reopened = Database::open_read_only(&path)?.connect();
        assert_batch(&mut reopened, 1, 2)?;
        assert_batch(&mut reopened, 3, 4)?;
    }
    Ok(())
}
