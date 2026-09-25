use duckdb_rust::{
    DatabaseBuilder, Error, Result, Value,
    execution::{
        Executor, MaterializingExecutor, PullExecutor,
        expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    },
    main::settings::{Configuration, LockedConfiguration, SnapshotConfiguration},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};
use std::sync::Arc;
#[path = "settings/contracts.rs"]
mod contracts;
#[path = "settings/controls.rs"]
mod controls;
#[path = "settings/ieee.rs"]
mod ieee;
#[path = "../runner/mod.rs"]
mod runner;
#[path = "settings/search_path.rs"]
mod search_path;
#[path = "settings/values.rs"]
mod values;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn configurations() -> Vec<Arc<dyn Configuration>> {
    vec![
        Arc::new(SnapshotConfiguration::default()),
        Arc::new(LockedConfiguration::default()),
    ]
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ordering_configuration_runs_through_both_providers_and_execution_compositions() -> Result<()> {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/sql/settings.test");
    for configuration in configurations() {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for expressions in [
                Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                Arc::new(BatchedEvaluator),
            ] {
                for executor in [
                    Arc::new(PullExecutor) as Arc<dyn Executor>,
                    Arc::new(MaterializingExecutor),
                ] {
                    for batch_size in [1, 7, 2048] {
                        let database = DatabaseBuilder::new()
                            .configuration(configuration.clone())
                            .optimizer(optimizer.clone())
                            .expressions(expressions.clone())
                            .executor(executor.clone())
                            .batch_size(batch_size)
                            .build()?;
                        let mut connection = database.connect();
                        connection.execute("RESET default_order; RESET default_null_order")?;
                        assert_eq!(runner::run_file(&database, &corpus)?, 40);
                        connection.execute("RESET default_order; RESET default_null_order")?;
                        assert_eq!(
                            runner::run_file(
                                &database,
                                &corpus.with_file_name("settings_sessions.test")
                            )?,
                            25
                        );
                        assert_eq!(
                            runner::run_file(&database, &corpus.with_file_name("ordering.test"))?,
                            20
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn global_session_and_prepared_views_preserve_scope_and_transaction_lifetimes() -> Result<()> {
    for configuration in configurations() {
        let database = DatabaseBuilder::new()
            .configuration(configuration)
            .build()?;
        let mut a = database.connect();
        let mut b = database.connect();
        let setting = a.prepare("SELECT current_setting('default_null_order')")?;
        let ordered = a.prepare("SELECT i FROM (VALUES (1),(NULL),(2)) t(i) ORDER BY i")?;
        a.execute("SET GLOBAL default_null_order=first")?;
        assert_eq!(
            b.query("SELECT current_setting('default_null_order')")?
                .rows[0][0],
            Value::Varchar("NULLS_FIRST".into())
        );
        a.execute("SET SESSION default_null_order=last")?;
        assert_eq!(
            a.execute_prepared(&setting, &[])?.rows[0][0],
            Value::Varchar("NULLS_LAST".into())
        );
        assert_eq!(
            a.execute_prepared(&ordered, &[])?.rows[0][0],
            Value::Integer(1)
        );
        a.execute("BEGIN; SET SESSION default_null_order=first; ROLLBACK")?;
        assert_eq!(a.execute_prepared(&ordered, &[])?.rows[0][0], Value::Null);
        b.execute("SET GLOBAL default_null_order=last")?;
        assert_eq!(
            a.execute_prepared(&setting, &[])?.rows[0][0],
            Value::Varchar("NULLS_FIRST".into())
        );
        a.execute("RESET SESSION default_null_order")?;
        assert_eq!(
            a.execute_prepared(&setting, &[])?.rows[0][0],
            Value::Varchar("NULLS_LAST".into())
        );
        a.execute("SET SESSION default_null_order=first")?;
        drop(a);
        assert_eq!(
            database
                .connect()
                .query("SELECT current_setting('default_null_order')")?
                .rows[0][0],
            Value::Varchar("NULLS_LAST".into())
        );
        let parameter = b.prepare("SET default_null_order=$1")?;
        assert!(matches!(
            b.execute_prepared(&parameter, &[Value::Varchar("first".into())]),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            b.query("SET LOCAL default_null_order=first"),
            Err(Error::Unsupported(_))
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn zero_parameter_prepared_queries_refresh_snapshots_and_invalidate_cache_keys() -> Result<()> {
    for configuration in configurations() {
        let database = DatabaseBuilder::new()
            .configuration(configuration)
            .build()?;
        let mut connection = database.connect();
        connection.execute(
            "CREATE TABLE prepared_cache(i INTEGER); INSERT INTO prepared_cache VALUES (2),(1)",
        )?;
        let rows = connection.prepare("SELECT i FROM prepared_cache ORDER BY i")?;

        // The second call is a warm cache hit, but table data is read through
        // a fresh transaction and freshly-opened physical operator state.
        assert_eq!(
            connection.execute_prepared(&rows, &[])?.rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        connection.execute("INSERT INTO prepared_cache VALUES (3)")?;
        assert_eq!(
            connection.execute_prepared(&rows, &[])?.rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(2)],
                vec![Value::Integer(3)]
            ]
        );

        // A catalog version change must rebind rather than execute ordinal
        // bindings from the previous catalog snapshot.
        connection.execute("DROP TABLE prepared_cache; CREATE TABLE prepared_cache(j INTEGER)")?;
        assert!(connection.execute_prepared(&rows, &[]).is_err());
        connection.execute("DROP TABLE prepared_cache; CREATE TABLE prepared_cache(i INTEGER); INSERT INTO prepared_cache VALUES (9)")?;
        assert_eq!(
            connection.execute_prepared(&rows, &[])?.rows,
            vec![vec![Value::Integer(9)]]
        );

        let ordered =
            connection.prepare("SELECT i FROM (VALUES (1),(NULL),(2)) t(i) ORDER BY i")?;
        assert_eq!(
            connection.execute_prepared(&ordered, &[])?.rows[0][0],
            Value::Integer(1)
        );
        connection.execute("SET default_null_order=first")?;
        assert_eq!(
            connection.execute_prepared(&ordered, &[])?.rows[0][0],
            Value::Null
        );

        // An explicit transaction must keep its own snapshot and not retain
        // execution state from the preceding cached invocation.
        connection.execute("BEGIN; INSERT INTO prepared_cache VALUES (10)")?;
        assert_eq!(
            connection.execute_prepared(&rows, &[])?.rows,
            vec![vec![Value::Integer(9)], vec![Value::Integer(10)]]
        );
        connection.execute("ROLLBACK")?;
        assert_eq!(
            connection.execute_prepared(&rows, &[])?.rows,
            vec![vec![Value::Integer(9)]]
        );

        // A bind failure before `run` must return ownership of an explicit
        // transaction to the connection.
        let missing = connection.prepare("SELECT * FROM missing_prepared_cache")?;
        connection.execute("BEGIN")?;
        assert!(connection.execute_prepared(&missing, &[]).is_err());
        connection.execute("INSERT INTO prepared_cache VALUES (11); ROLLBACK")?;

        // A cached physical plan still uses the ordinary failed-transaction
        // state on an execution error and remains reusable after rollback.
        connection.execute(
            "CREATE TABLE prepared_runtime(v VARCHAR); INSERT INTO prepared_runtime VALUES ('1')",
        )?;
        let runtime = connection.prepare("SELECT CAST(v AS INTEGER) FROM prepared_runtime")?;
        assert_eq!(
            connection.execute_prepared(&runtime, &[])?.rows,
            vec![vec![Value::Integer(1)]]
        );
        connection.execute("BEGIN; INSERT INTO prepared_runtime VALUES ('bad')")?;
        assert!(connection.execute_prepared(&runtime, &[]).is_err());
        connection.execute("ROLLBACK")?;
        assert_eq!(
            connection.execute_prepared(&runtime, &[])?.rows,
            vec![vec![Value::Integer(1)]]
        );
    }
    Ok(())
}
