use duckdb_rust::{DatabaseBuilder, Result, Value};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn from_first_implicitly_projects_the_bound_relation() -> Result<()> {
    let mut connection = DatabaseBuilder::new().build()?.connect();
    connection.execute("CREATE TABLE items(id INTEGER, name VARCHAR); INSERT INTO items VALUES (2, 'two'), (1, 'one')")?;

    assert_eq!(
        connection
            .query("FROM items WHERE id > 0 ORDER BY id")?
            .rows,
        vec![
            vec![Value::Integer(1), Value::Varchar("one".into())],
            vec![Value::Integer(2), Value::Varchar("two".into())],
        ]
    );
    assert_eq!(
        connection
            .query("FROM items AS i SELECT i.name ORDER BY i.id LIMIT 1")?
            .rows,
        vec![vec![Value::Varchar("one".into())]]
    );
    assert_eq!(
        connection
            .query("SELECT * FROM (FROM items ORDER BY id LIMIT 1) AS nested")?
            .rows,
        vec![vec![Value::Integer(1), Value::Varchar("one".into())]]
    );

    let prepared = connection.prepare("FROM items WHERE id = $1 ORDER BY id")?;
    assert_eq!(
        connection
            .execute_prepared(&prepared, &[Value::Integer(2)])?
            .rows,
        vec![vec![Value::Integer(2), Value::Varchar("two".into())]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn create_table_as_from_values_retains_values_types_and_names() -> Result<()> {
    let mut connection = DatabaseBuilder::new().build()?.connect();
    connection
        .execute("CREATE TABLE copied AS FROM VALUES (2, 'two'), (1, 'one') AS source(id, name)")?;
    assert_eq!(
        connection.query("SELECT * FROM copied ORDER BY id")?.rows,
        vec![
            vec![Value::Integer(1), Value::Varchar("one".into())],
            vec![Value::Integer(2), Value::Varchar("two".into())],
        ]
    );
    assert!(connection.query("FROM items").is_err());
    Ok(())
}
