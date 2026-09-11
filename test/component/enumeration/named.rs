use duckdb_rust::{Database, Error, Result, Value};

fn varchar(value: &str) -> Value {
    Value::Varchar(value.into())
}

#[test]
fn sql_named_enum_conflicts_replacement_and_drop_keep_concrete_table_types() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute("CREATE TYPE empty AS ENUM (); CREATE TABLE empty_values(x empty)")?;
    connection.execute("INSERT INTO empty_values VALUES (NULL)")?;
    assert_eq!(
        connection.query("SELECT count(*) FROM empty_values")?.rows,
        vec![vec![Value::Integer(1)]]
    );

    connection.execute("CREATE TYPE mood AS ENUM ('old'); CREATE TABLE mood(x mood)")?;
    connection.execute("INSERT INTO mood VALUES ('old')")?;
    connection.execute("CREATE TYPE IF NOT EXISTS mood AS ENUM ('ignored')")?;
    assert!(
        connection
            .execute("INSERT INTO mood VALUES ('ignored')")
            .is_err()
    );
    assert_eq!(
        connection.query("SELECT x::VARCHAR FROM mood")?.rows,
        vec![vec![varchar("old")]]
    );

    let duplicate = connection.execute("CREATE TYPE duplicate AS ENUM ('x', 'x')");
    assert!(matches!(duplicate, Err(Error::InvalidInput(_))));
    connection.execute("CREATE TABLE old_values(x mood); INSERT INTO old_values VALUES ('old')")?;
    connection.execute("CREATE OR REPLACE TYPE mood AS ENUM ('new')")?;
    connection.execute("CREATE TABLE new_values(x mood); INSERT INTO new_values VALUES ('new')")?;
    assert_eq!(
        connection.query("SELECT x::VARCHAR FROM old_values")?.rows,
        vec![vec![varchar("old")]]
    );
    assert_eq!(
        connection.query("SELECT x::VARCHAR FROM new_values")?.rows,
        vec![vec![varchar("new")]]
    );

    connection.execute("DROP TYPE mood RESTRICT")?;
    connection.execute("INSERT INTO old_values VALUES ('old')")?;
    assert_eq!(
        connection
            .query("SELECT x::VARCHAR FROM old_values ORDER BY ALL")?
            .rows,
        vec![vec![varchar("old")], vec![varchar("old")]]
    );
    connection.execute("DROP TYPE IF EXISTS mood")?;
    Ok(())
}

#[test]
fn sql_named_enum_search_path_qualification_and_rejections_are_explicit() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute("CREATE SCHEMA s; SET search_path='s'")?;
    connection.execute("CREATE TYPE mood AS ENUM ('s'); CREATE TABLE values_in_s(x mood)")?;
    connection.execute("INSERT INTO values_in_s VALUES ('s')")?;
    assert_eq!(
        connection
            .query("SELECT x::VARCHAR FROM s.values_in_s")?
            .rows,
        vec![vec![varchar("s")]]
    );
    connection.execute("CREATE TABLE main.qualified_value(x s.mood)")?;
    connection.execute("INSERT INTO main.qualified_value VALUES ('s')")?;

    connection.execute("CREATE TYPE main.mood AS ENUM ('main')")?;
    assert!(connection.execute("DROP TYPE s.missing").is_err());
    connection.execute("CREATE TABLE main.main_value(x main.mood)")?;
    connection.execute("INSERT INTO main.main_value VALUES ('main')")?;

    for sql in [
        "CREATE TYPE temp.nope AS ENUM ('x')",
        "CREATE TYPE db.main.nope AS ENUM ('x')",
        "DROP TYPE db.main.mood",
        "DROP TYPE mood CASCADE",
        "DROP TYPE main.mood, s.mood",
        "CREATE TYPE alias AS INTEGER",
        "CREATE TYPE selected AS ENUM (SELECT 'x')",
    ] {
        assert!(connection.execute(sql).is_err(), "{sql}");
    }
    Ok(())
}

#[test]
fn sql_named_enum_transaction_and_prepared_rebinding_follow_catalog_visibility() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute("BEGIN; CREATE TYPE rolled_back AS ENUM ('x'); ROLLBACK")?;
    assert!(
        connection
            .execute("CREATE TABLE unavailable(x rolled_back)")
            .is_err()
    );

    connection.execute("CREATE TYPE rebound AS ENUM ('old')")?;
    connection.execute("CREATE TABLE retained(x rebound); INSERT INTO retained VALUES ('old')")?;
    let prepared = connection.prepare("CREATE TABLE rebound_after_replace(x rebound)")?;
    connection.execute("CREATE OR REPLACE TYPE rebound AS ENUM ('new')")?;
    connection.execute_prepared(&prepared, &[])?;
    connection.execute("INSERT INTO rebound_after_replace VALUES ('new')")?;
    assert_eq!(
        connection.query("SELECT x::VARCHAR FROM retained")?.rows,
        vec![vec![varchar("old")]]
    );
    assert_eq!(
        connection
            .query("SELECT x::VARCHAR FROM rebound_after_replace")?
            .rows,
        vec![vec![varchar("new")]]
    );
    Ok(())
}
