//! Context composition, not native parsed DEFAULT codec acceptance.
#[path = "stored_log_context.rs"]
mod logging;
use super::*;
use duckdb_rust::{
    main::settings::{Configuration, SnapshotConfiguration},
    optimizer::IdentityOptimizer,
    storage::{
        format::{FormatId, JSON_FORMAT, SnapshotFormat},
        log::Commit,
        recovery::{Recovery, RecoveryInput},
    },
};

#[derive(Debug)]
struct SettingFunction;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for SettingFunction {
    fn name(&self) -> &str {
        "stored_setting"
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, _: &[Value], query: &QueryContext) -> Result<Value> {
        Ok(query.settings().get("default_order", query)?.clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn probe(query: &QueryContext) -> Result<Value> {
    let snapshot = Snapshot::new(query.type_registry());
    query.stored_expressions()?.evaluate(
        &call("stored_setting", vec![]),
        &DataType::Varchar,
        &snapshot,
        query,
    )
}

#[derive(Debug)]
struct ContextFunction;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ContextFunction {
    fn name(&self) -> &str {
        "stored_context"
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, _: &[Value], query: &QueryContext) -> Result<Value> {
        probe(query)
    }
}

struct ContextDurability(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Durability for ContextDurability {
    fn name(&self) -> &'static str {
        "context-durability"
    }
    fn load(&self, _: Arc<TypeRegistry>) -> Result<Snapshot> {
        Err(Error::Unsupported("legacy load is not selected".into()))
    }
    fn load_with_context(&self, query: &QueryContext) -> Result<Snapshot> {
        assert_eq!(probe(query)?, Value::Varchar("DESC".into()));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Snapshot::new(query.type_registry()))
    }
    fn publish(&self, _: Commit<'_>) -> Result<()> {
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn functions() -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(SettingFunction))?;
    functions.register_scalar(Arc::new(ContextFunction))?;
    Ok(functions)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_services_follow_startup_session_and_prepared_settings() -> Result<()> {
    let configuration = Arc::new(SnapshotConfiguration::default());
    DatabaseBuilder::new()
        .configuration(configuration.clone())
        .build()?
        .connect()
        .execute("SET GLOBAL default_order='DESC'")?;
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let database = DatabaseBuilder::new()
            .configuration(configuration.clone())
            .functions(functions()?)
            .expressions(evaluator)
            .optimizer(Arc::new(IdentityOptimizer))
            .durability(Arc::new(ContextDurability(calls.clone())))
            .build()?;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let mut connection = database.connect();
        let prepared = connection.prepare("SELECT stored_context()")?;
        assert_eq!(
            connection.execute_prepared(&prepared, &[])?.rows[0][0],
            Value::Varchar("DESC".into())
        );
        connection.execute("SET SESSION default_order='ASC'")?;
        assert_eq!(
            connection.execute_prepared(&prepared, &[])?.rows[0][0],
            Value::Varchar("ASC".into())
        );
        assert_eq!(
            database.connect().query("SELECT stored_context()")?.rows[0][0],
            Value::Varchar("DESC".into())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    assert!(matches!(
        DatabaseBuilder::new()
            .configuration(configuration)
            .functions(functions()?)
            .binder(Arc::new(OrdinaryBinder))
            .durability(Arc::new(ContextDurability(calls.clone())))
            .build(),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

struct ContextFormat(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotFormat for ContextFormat {
    fn name(&self) -> &'static str {
        "context-format"
    }
    fn format_id(&self) -> FormatId {
        JSON_FORMAT
    }
    fn decode(&self, _: Vec<u8>, _: Arc<TypeRegistry>) -> Result<Snapshot> {
        Err(Error::Unsupported("legacy decode is not selected".into()))
    }
    fn decode_with_context(&self, bytes: Vec<u8>, query: &QueryContext) -> Result<Snapshot> {
        assert_eq!(probe(query)?, Value::Varchar("ASCENDING".into()));
        self.0.fetch_add(1, Ordering::SeqCst);
        JsonSnapshotFormat.decode(bytes, query.type_registry())
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        JsonSnapshotFormat.encode(snapshot)
    }
}

struct ContextRecovery;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Recovery for ContextRecovery {
    fn name(&self) -> &'static str {
        "context-recovery"
    }
    fn format_id(&self) -> FormatId {
        JSON_FORMAT
    }
    fn recover(
        &self,
        input: RecoveryInput,
        format: &dyn SnapshotFormat,
        query: &QueryContext,
    ) -> Result<Snapshot> {
        assert_eq!(probe(query)?, Value::Varchar("ASCENDING".into()));
        assert_eq!(input.log, b"selected recovery input");
        format.decode_with_context(input.checkpoint, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn file_load_and_read_only_recovery_keep_selected_context_and_input_bytes() -> Result<()> {
    for with_log in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("context.duckdb");
        let bytes = JsonSnapshotFormat.encode(&Snapshot::default())?;
        std::fs::write(&path, &bytes)?;
        let log = path.with_extension("duckdb.wal");
        if with_log {
            std::fs::write(&log, b"selected recovery input")?;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let file = FileCheckpoint::open(
            &path,
            OpenMode::ReadOnly,
            Arc::new(ContextFormat(calls.clone())),
        )?
        .with_recovery(Arc::new(ContextRecovery))?;
        let database = DatabaseBuilder::new()
            .functions(functions()?)
            .durability(Arc::new(file))
            .build()?;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            database.connect().query("SELECT stored_context()")?.rows[0][0],
            Value::Varchar("ASCENDING".into())
        );
        assert_eq!(std::fs::read(&path)?, bytes);
        if with_log {
            assert_eq!(std::fs::read(&log)?, b"selected recovery input");
        }
    }
    Ok(())
}

#[derive(Debug)]
struct FailedConfiguration;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Configuration for FailedConfiguration {
    fn name(&self) -> &'static str {
        "failed-initial-configuration"
    }
    fn connect(&self) -> Box<dyn duckdb_rust::main::settings::ConfigurationSession> {
        Box::new(Self)
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::main::settings::ConfigurationSession for FailedConfiguration {
    fn snapshot(&self, _: &QueryContext) -> Result<duckdb_rust::main::settings::SettingsSnapshot> {
        Err(Error::Resource("initial settings failed".into()))
    }
    fn apply(
        &mut self,
        _: &duckdb_rust::main::settings::SettingChange,
        _: &QueryContext,
    ) -> Result<()> {
        unreachable!()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn invalid_startup_configuration_precedes_durability_and_cancelled_load_does_not_start()
-> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    assert!(matches!(
        DatabaseBuilder::new()
            .configuration(Arc::new(FailedConfiguration))
            .durability(Arc::new(ContextDurability(calls.clone())))
            .build(),
        Err(Error::Resource(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let handle = InterruptHandle::default();
    let query = QueryContext::new(handle.clone(), None, 1, 1)?;
    handle.interrupt();
    assert!(matches!(
        MemoryDurability.load_with_context(&query),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        SnapshotTransactions::configured_with_context(
            Arc::new(ContextDurability(calls.clone())),
            Arc::new(duckdb_rust::execution::index::HashIndexFactory),
            &query
        ),
        Err(Error::Interrupted)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}
