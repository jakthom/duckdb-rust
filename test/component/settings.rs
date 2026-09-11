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
