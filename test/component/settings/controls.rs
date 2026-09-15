use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn verification_pragmas_are_session_scoped_resettable_and_prepared_aware() -> Result<()> {
    for configuration in configurations() {
        let database = DatabaseBuilder::new()
            .configuration(configuration)
            .build()?;
        let mut enabled = database.connect();
        let mut other = database.connect();
        let prepared = enabled.prepare("SELECT i * 2 FROM range(3) t(i) ORDER BY i")?;

        enabled.execute("PRAGMA enable_verification")?;
        assert_eq!(
            enabled
                .query("SELECT current_setting('enable_verification')")?
                .rows[0][0],
            Value::Boolean(true)
        );
        assert_eq!(
            other
                .query("SELECT current_setting('enable_verification')")?
                .rows[0][0],
            Value::Boolean(false)
        );
        assert_eq!(enabled.execute_prepared(&prepared, &[])?.rows.len(), 3);
        assert!(matches!(
            enabled.execute("SET GLOBAL enable_verification=true"),
            Err(Error::Unsupported(_))
        ));

        enabled.execute("RESET enable_verification")?;
        assert_eq!(
            enabled
                .query("SELECT current_setting('enable_verification')")?
                .rows[0][0],
            Value::Boolean(false)
        );
        enabled.execute("SET enable_verification=true")?;
        assert_eq!(enabled.execute_prepared(&prepared, &[])?.rows.len(), 3);
        enabled.execute("PRAGMA disable_verification")?;
        assert_eq!(enabled.execute_prepared(&prepared, &[])?.rows.len(), 3);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn profiling_emits_measured_output_and_rejects_unimplemented_renderers() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let output = directory.path().join("profile.json");
    let bad_output = directory.path().join("profile.db");
    let database = DatabaseBuilder::new().build()?;
    let mut connection = database.connect();

    connection.execute(&format!(
        "SET profiling_output='{}'; PRAGMA enable_profiling='json'",
        output.display()
    ))?;
    assert_eq!(connection.query("SELECT * FROM range(3)")?.rows.len(), 3);
    let profile: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output)?).unwrap();
    assert_eq!(profile["rows_returned"], 3);
    assert_eq!(profile["verification_enabled"], false);
    assert!(profile["latency_seconds"].as_f64().is_some());

    connection.execute("RESET enable_profiling")?;
    let before = std::fs::read_to_string(&output)?;
    connection.query("SELECT 42")?;
    assert_eq!(std::fs::read_to_string(&output)?, before);

    connection.execute("RESET profiling_output; SET profiling_mode='all'")?;
    assert_eq!(
        connection
            .query("SELECT current_setting('profiling_mode')")?
            .rows[0][0],
        Value::Varchar("standard".into())
    );
    connection.execute("RESET profiling_mode")?;

    connection.execute("SET enable_profiling='html'")?;
    assert!(matches!(
        connection.query("SELECT 1"),
        Err(Error::Unsupported(message)) if message.contains("profiling renderer html")
    ));
    connection.execute("RESET enable_profiling")?;

    connection.execute(&format!(
        "SET profiling_output='{}'; SET enable_profiling='json'",
        bad_output.display()
    ))?;
    assert!(matches!(connection.query("SELECT 1"), Err(Error::Parse(_))));
    connection.execute("PRAGMA disable_profiling; RESET profiling_output")?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn force_external_and_streaming_controls_fail_closed_instead_of_being_ignored() -> Result<()> {
    let database = DatabaseBuilder::new().build()?;
    let mut connection = database.connect();
    let prepared = connection.prepare("SELECT * FROM range(3)")?;

    connection.execute("SET debug_force_external=true")?;
    assert!(matches!(
        connection.execute_prepared(&prepared, &[]),
        Err(Error::Unsupported(message)) if message.contains("external/spill-capable")
    ));
    assert!(matches!(
        connection.query_batches("SELECT 1", |_, _| unreachable!()),
        Err(Error::Unsupported(message)) if message.contains("external/spill-capable")
    ));
    connection.execute("RESET debug_force_external")?;
    assert_eq!(connection.execute_prepared(&prepared, &[])?.rows.len(), 3);

    connection.execute("PRAGMA debug_force_external=true")?;
    assert!(matches!(
        connection.query("SELECT 1"),
        Err(Error::Unsupported(_))
    ));
    connection.execute("RESET debug_force_external")?;

    connection.execute("PRAGMA enable_verification")?;
    assert!(matches!(
        connection.query_batches("SELECT 1", |_, _| unreachable!()),
        Err(Error::Unsupported(message)) if message.contains("materialized query execution")
    ));
    connection.execute("PRAGMA disable_verification")?;

    connection.execute("SET enable_profiling='no_output'")?;
    assert!(matches!(
        connection.query_batches("SELECT 1", |_, _| unreachable!()),
        Err(Error::Unsupported(message)) if message.contains("materialized query execution")
    ));
    connection.execute("PRAGMA disable_profiling")?;
    Ok(())
}
