use super::*;
use duckdb_rust::{
    DataType,
    main::settings::{
        Setting, SettingDefinition, SettingRegistry, SettingScope, SettingValues, SettingsSnapshot,
    },
    parallel::{InterruptHandle, QueryContext, Scheduler},
    planner::BoundStatement,
};
use std::collections::BTreeMap;

#[derive(Debug)]
struct IntegerSetting {
    bad: bool,
}
impl Setting for IntegerSetting {
    fn definition(&self) -> SettingDefinition {
        SettingDefinition {
            name: "answer".into(),
            aliases: vec!["answer_alias".into()],
            data_type: DataType::BigInt,
            default: Value::Integer(10),
            default_scope: SettingScope::Session,
            global: true,
            session: true,
        }
    }
    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if self.bad && value == &Value::Integer(99) {
            return Ok(Value::Varchar("invalid".into()));
        }
        let value = value.as_i128()?;
        if !(0..=100).contains(&value) {
            return Err(Error::Bind("answer must be between 0 and 100".into()));
        }
        Ok(Value::Integer(value))
    }
}
fn registry(bad: bool) -> Result<Arc<SettingRegistry>> {
    let mut registry = SettingRegistry::builtins();
    registry.register(Arc::new(IntegerSetting { bad }))?;
    Ok(Arc::new(registry))
}
pub(super) fn providers(registry: &Arc<SettingRegistry>) -> Vec<Arc<dyn Configuration>> {
    vec![
        Arc::new(SnapshotConfiguration::new(registry.clone())),
        Arc::new(LockedConfiguration::new(registry.clone())),
    ]
}

#[test]
fn configuration_providers_share_typed_registration_snapshot_and_failure_contracts() -> Result<()> {
    let query = QueryContext::background();
    let registry = registry(true)?;
    for provider in providers(&registry) {
        let mut a = provider.connect();
        let b = provider.connect();
        let old = a.snapshot(&query)?;
        let global = registry.bind(
            "answer_alias",
            Some(SettingScope::Global),
            Some(Value::Integer(20)),
            &query,
        )?;
        a.apply(&global, &query)?;
        assert_eq!(old.get("answer", &query)?, &Value::Integer(10));
        assert_eq!(
            b.snapshot(&query)?.get("ANSWER", &query)?,
            &Value::Integer(20)
        );
        a.apply(
            &registry.bind("answer", None, Some(Value::Integer(30)), &query)?,
            &query,
        )?;
        assert_eq!(
            a.snapshot(&query)?.get("answer", &query)?,
            &Value::Integer(30)
        );
        assert_eq!(
            b.snapshot(&query)?.get("answer", &query)?,
            &Value::Integer(20)
        );
        a.apply(&registry.bind("answer", None, None, &query)?, &query)?;
        assert_eq!(
            a.snapshot(&query)?.get("answer", &query)?,
            &Value::Integer(20)
        );
        let interrupt = InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
        interrupt.interrupt();
        assert!(matches!(
            a.apply(&global, &cancelled),
            Err(Error::Interrupted)
        ));
        assert!(matches!(a.snapshot(&cancelled), Err(Error::Interrupted)));
        assert!(matches!(
            registry.bind("answer", None, Some(Value::Integer(99)), &query),
            Err(Error::Internal(_))
        ));
        assert!(
            registry
                .bind("answer", None, Some(Value::Integer(101)), &query)
                .is_err()
        );
        assert!(
            registry
                .bind("answer", None, Some(Value::Varchar("bad".into())), &query)
                .is_err()
        );
        let foreign =
            self::registry(false)?.bind("answer", None, Some(Value::Integer(1)), &query)?;
        assert!(matches!(a.apply(&foreign, &query), Err(Error::Bind(_))));
        let retained = a.snapshot(&query)?;
        drop(a);
        drop(b);
        drop(provider);
        assert_eq!(retained.get("answer", &query)?, &Value::Integer(20));
    }
    let invalid = |values: SettingValues| {
        SettingsSnapshot::new(
            registry.clone(),
            Arc::new(values),
            Arc::new(BTreeMap::new()),
            &query,
        )
    };
    assert!(
        invalid(BTreeMap::from([(
            "answer".into(),
            Value::Varchar("wrong".into())
        )]))
        .is_err()
    );
    assert!(invalid(BTreeMap::from([("answer_alias".into(), Value::Integer(1))])).is_err());
    assert!(invalid(BTreeMap::from([("unregistered".into(), Value::Integer(1))])).is_err());
    Ok(())
}

#[test]
fn contextual_functions_retain_typed_settings_across_batches_and_other_writers() -> Result<()> {
    let registry = registry(false)?;
    for provider in providers(&registry) {
        let database = DatabaseBuilder::new()
            .configuration(provider)
            .batch_size(1)
            .build()?;
        let mut a = database.connect();
        let mut b = database.connect();
        let query = a.prepare("SELECT current_setting('answer_alias')")?;
        let initial = a.execute_prepared(&query, &[])?;
        assert_eq!(initial.columns[0].data_type, DataType::BigInt);
        assert_eq!(initial.rows[0][0], Value::Integer(10));
        a.execute("SET answer_alias='25'")?;
        assert_eq!(
            a.execute_prepared(&query, &[])?.rows[0][0],
            Value::Integer(25)
        );
        a.execute("RESET SESSION answer")?;
        let mut values = Vec::new();
        a.query_batches(
            "SELECT current_setting('answer') FROM range(5)",
            |_, batch| {
                if values.is_empty() {
                    b.execute("SET GLOBAL answer=40")?;
                }
                values.extend(batch.rows().map(|row| row[0].clone()));
                Ok(duckdb_rust::execution::StreamControl::Continue)
            },
        )?;
        assert_eq!(values, vec![Value::Integer(10); 5]);
        assert_eq!(
            a.execute_prepared(&query, &[])?.rows[0][0],
            Value::Integer(40)
        );
        let change = registry.bind(
            "answer",
            None,
            Some(Value::Integer(60)),
            &QueryContext::background(),
        )?;
        a.execute_plan(BoundStatement::Configure(change))?;
        assert_eq!(
            a.execute_prepared(&query, &[])?.rows[0][0],
            Value::Integer(60)
        );
        assert!(a.query("SET answer=101").is_err());
        assert_eq!(
            a.execute_prepared(&query, &[])?.rows[0][0],
            Value::Integer(60)
        );
    }
    Ok(())
}

struct FailedScheduler(usize);
impl Scheduler for FailedScheduler {
    fn name(&self) -> &'static str {
        "failed-settings-scheduler"
    }
    fn run(&self, _: &QueryContext, task: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        match self.0 {
            0 => Ok(()),
            1 => {
                task()?;
                task()
            }
            _ => {
                task()?;
                Err(Error::Resource("after task".into()))
            }
        }
    }
}
#[test]
fn scheduler_failures_never_publish_nontransactional_settings() -> Result<()> {
    let query = QueryContext::background();
    for configuration in configurations() {
        for mode in 0..3 {
            let database = DatabaseBuilder::new()
                .configuration(configuration.clone())
                .scheduler(Arc::new(FailedScheduler(mode)))
                .build()?;
            let mut connection = database.connect();
            connection.execute("BEGIN")?;
            assert!(
                connection
                    .execute("SET GLOBAL default_null_order=first")
                    .is_err()
            );
            let snapshot = configuration.connect().snapshot(&query)?;
            assert_eq!(
                snapshot.get("default_null_order", &query)?,
                &Value::Varchar("NULLS_LAST".into())
            );
            assert!(matches!(
                connection.query("SELECT 1"),
                Err(Error::Transaction(_))
            ));
            connection.execute("ROLLBACK")?;
        }
    }
    Ok(())
}

struct InvalidEvaluator;
impl ExpressionEvaluator for InvalidEvaluator {
    fn name(&self) -> &'static str {
        "invalid-setting-evaluator"
    }
    fn evaluate(
        &self,
        _: &duckdb_rust::planner::BoundExpr,
        _: &duckdb_rust::common::Row,
        context: &dyn duckdb_rust::execution::expression_executor::EvaluationContext,
    ) -> Result<Value> {
        context.query().check()?;
        Ok(Value::Integer(7))
    }
}

#[derive(Debug)]
struct BrokenBinding {
    bound: bool,
}
impl duckdb_rust::function::ScalarFunction for BrokenBinding {
    fn name(&self) -> &str {
        "broken_binding"
    }
    fn bind(
        &self,
        _: &dyn duckdb_rust::function::ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn duckdb_rust::function::ScalarFunction>>> {
        Ok(Some(Arc::new(Self { bound: true })))
    }
    fn return_type(
        &self,
        _: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if self.bound {
            Ok(DataType::BigInt)
        } else {
            Err(Error::Bind("function was not bound".into()))
        }
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Ok(Value::Varchar("invalid".into()))
    }
}

#[test]
fn configuration_and_contextual_functions_reject_invalid_adapter_values() -> Result<()> {
    for configuration in configurations() {
        let database = DatabaseBuilder::new()
            .configuration(configuration.clone())
            .expressions(Arc::new(InvalidEvaluator))
            .build()?;
        let mut connection = database.connect();
        for sql in [
            "SET default_null_order=first",
            "SELECT current_setting('default_order')",
        ] {
            assert!(
                matches!(connection.query(sql), Err(Error::Internal(_))),
                "{sql}"
            );
        }
        let query = QueryContext::background();
        assert_eq!(
            configuration
                .connect()
                .snapshot(&query)?
                .get("default_null_order", &query)?,
            &Value::Varchar("NULLS_LAST".into())
        );
    }
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut functions = duckdb_rust::function::FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(BrokenBinding { bound: false }))?;
        let mut connection = DatabaseBuilder::new()
            .functions(functions)
            .expressions(expressions)
            .build()?
            .connect();
        assert_eq!(
            connection
                .query("SELECT CASE WHEN false THEN broken_binding() ELSE 1 END")?
                .rows[0][0],
            Value::Integer(1)
        );
        assert!(matches!(
            connection.query("SELECT TRY_CAST(broken_binding() AS BIGINT)"),
            Err(Error::Internal(_))
        ));
    }
    Ok(())
}
