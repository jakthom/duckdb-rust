use super::*;
use duckdb_rust::{
    DataType,
    catalog::SearchPath,
    main::settings::{SettingRegistry, SettingScope},
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn search_path_setting_is_normalized_session_scoped_and_bounded() -> Result<()> {
    let query = QueryContext::background();
    let registry = Arc::new(SettingRegistry::builtins());
    let definition = registry.definition("search_path")?;
    assert_eq!(definition.data_type, DataType::Varchar);
    assert_eq!(definition.default, Value::Varchar(String::new()));
    assert_eq!(definition.default_scope, SettingScope::Session);
    assert!(!definition.global);
    assert!(definition.session);

    let change = registry.bind(
        "SEARCH_PATH",
        None,
        Some(Value::Varchar("analytics,\"dot.schema\",analytics".into())),
        &query,
    )?;
    assert_eq!(change.name(), "search_path");
    assert_eq!(
        change.value(),
        Some(&Value::Varchar("analytics,\"dot.schema\",analytics".into()))
    );
    assert!(matches!(
        registry.bind(
            "search_path",
            Some(SettingScope::Global),
            Some(Value::Varchar("main".into())),
            &query,
        ),
        Err(Error::Unsupported(_))
    ));
    assert!(
        registry
            .bind(
                "search_path",
                None,
                Some(Value::Varchar("bad..path".into())),
                &query,
            )
            .is_err()
    );
    assert!(
        registry
            .bind("search_path", None, Some(Value::Null), &query)
            .is_err()
    );

    for provider in super::contracts::providers(&registry) {
        let mut session = provider.connect();
        session.apply(&change, &query)?;
        assert_eq!(
            session.snapshot(&query)?.search_path(&query)?,
            SearchPath::from_setting("analytics,\"dot.schema\",analytics")?
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sql_search_path_drives_lookup_creation_dml_and_prepared_rebinding() -> Result<()> {
    for configuration in configurations() {
        let database = DatabaseBuilder::new()
            .configuration(configuration)
            .build()?;
        let mut a = database.connect();
        let mut b = database.connect();
        a.execute(
            "CREATE SCHEMA analytics;
             CREATE SCHEMA warehouse;
             CREATE TABLE main.events(i INTEGER);
             INSERT INTO main.events VALUES (1);
             CREATE TABLE analytics.events(i INTEGER);
             INSERT INTO analytics.events VALUES (2);
             CREATE TABLE warehouse.other(i INTEGER);
             INSERT INTO warehouse.other VALUES (3)",
        )?;

        let selected = a.prepare("SELECT i FROM events")?;
        assert_eq!(
            a.execute_prepared(&selected, &[])?.rows,
            vec![vec![Value::Integer(1)]]
        );
        a.execute("SET search_path='analytics,warehouse'")?;
        assert_eq!(
            a.query("SELECT current_setting('search_path')")?.rows,
            vec![vec![Value::Varchar("analytics,warehouse".into())]]
        );
        assert_eq!(
            a.execute_prepared(&selected, &[])?.rows,
            vec![vec![Value::Integer(2)]]
        );
        assert_eq!(
            a.query("SELECT i FROM other")?.rows,
            vec![vec![Value::Integer(3)]]
        );
        assert_eq!(
            b.execute_prepared(&selected, &[])?.rows,
            vec![vec![Value::Integer(1)]]
        );

        a.execute("INSERT INTO events VALUES (4); CREATE TABLE created(i INTEGER)")?;
        assert_eq!(
            a.query("SELECT i FROM analytics.events ORDER BY i")?.rows,
            vec![vec![Value::Integer(2)], vec![Value::Integer(4)]]
        );
        a.execute("INSERT INTO created VALUES (5)")?;
        assert_eq!(
            a.query("SELECT i FROM analytics.created")?.rows,
            vec![vec![Value::Integer(5)]]
        );
        assert_eq!(
            a.query(
                "WITH events AS (SELECT 9 AS i)
                 SELECT i FROM events"
            )?
            .rows,
            vec![vec![Value::Integer(9)]]
        );

        a.execute("DROP TABLE events")?;
        assert_eq!(
            a.execute_prepared(&selected, &[])?.rows,
            vec![vec![Value::Integer(1)]]
        );
        a.execute("SET search_path='warehouse,main'")?;
        assert_eq!(
            a.execute_prepared(&selected, &[])?.rows,
            vec![vec![Value::Integer(1)]]
        );
        assert!(matches!(
            a.execute("SET search_path='missing'"),
            Err(Error::Catalog(_))
        ));
        assert_eq!(
            a.query("SELECT current_setting('search_path')")?.rows,
            vec![vec![Value::Varchar("warehouse,main".into())]]
        );
        assert!(matches!(
            a.execute("SET search_path='other_catalog.main'"),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            a.execute("SET GLOBAL search_path='main'"),
            Err(Error::Unsupported(_))
        ));
        a.execute("RESET search_path")?;
        assert_eq!(
            a.execute_prepared(&selected, &[])?.rows,
            vec![vec![Value::Integer(1)]]
        );
    }
    Ok(())
}
