use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    function::{FunctionEffects, FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
    storage::{checkpoint::FileCheckpoint, filesystem::OpenMode, format::JsonSnapshotFormat},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct EffectState {
    calls: AtomicUsize,
    fail_on: AtomicUsize,
}

#[derive(Debug)]
struct LifecycleDefault(Arc<EffectState>);

impl std::fmt::Debug for EffectState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EffectState")
            .field("calls", &self.calls.load(Ordering::SeqCst))
            .field("fail_on", &self.fail_on.load(Ordering::SeqCst))
            .finish()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for LifecycleDefault {
    fn name(&self) -> &str {
        "lifecycle_default"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: true,
        }
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if !arguments.is_empty() {
            return Err(Error::Bind("lifecycle_default accepts no arguments".into()));
        }
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if !arguments.is_empty() {
            return Err(Error::Internal("bound lifecycle_default arguments".into()));
        }
        let call = self.0.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.0.fail_on.load(Ordering::SeqCst) == call {
            return Err(Error::Execution(format!(
                "lifecycle default failed on call {call}"
            )));
        }
        let Value::Varchar(order) = query.settings().get("default_order", query)? else {
            return Err(Error::Internal("default_order is not VARCHAR".into()));
        };
        Ok(Value::Varchar(format!("{order}:{call}")))
    }
}

fn functions(state: Arc<EffectState>) -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(LifecycleDefault(state)))?;
    Ok(functions)
}

fn integer(value: i128) -> Value {
    Value::Integer(value)
}

fn string(value: &str) -> Value {
    Value::Varchar(value.into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn prepared_omissions_use_execution_settings_and_failed_rows_remain_atomic() -> Result<()> {
    let state = Arc::new(EffectState::default());
    let mut connection = DatabaseBuilder::new()
        .functions(functions(state.clone())?)
        .build()?
        .connect();
    connection
        .execute("CREATE TABLE t(id INTEGER, observed VARCHAR DEFAULT lifecycle_default())")?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);

    let omitted = connection.prepare("INSERT INTO t(id) VALUES ($1),($2)")?;
    connection.execute("SET SESSION default_order='ASC'")?;
    connection.execute_prepared(&omitted, &[integer(1), integer(2)])?;
    connection.execute("SET SESSION default_order='DESC'")?;
    connection.execute_prepared(&omitted, &[integer(3), integer(4)])?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 4);

    let supplied = connection.prepare("INSERT INTO t VALUES ($1,$2)")?;
    connection.execute_prepared(&supplied, &[integer(5), string("supplied")])?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        connection.query("SELECT * FROM t ORDER BY id ASC")?.rows,
        vec![
            vec![integer(1), string("ASC:1")],
            vec![integer(2), string("ASC:2")],
            vec![integer(3), string("DESC:3")],
            vec![integer(4), string("DESC:4")],
            vec![integer(5), string("supplied")],
        ]
    );

    state.fail_on.store(6, Ordering::SeqCst);
    assert!(
        connection
            .execute_prepared(&omitted, &[integer(10), integer(11)])
            .is_err()
    );
    assert_eq!(state.calls.load(Ordering::SeqCst), 6);
    assert_eq!(
        connection.query("SELECT count(*) FROM t")?.rows,
        vec![vec![integer(5)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn default_ddl_rollback_and_old_transactions_preserve_their_catalog_snapshot() -> Result<()> {
    let state = Arc::new(EffectState::default());
    let database = DatabaseBuilder::new()
        .functions(functions(state.clone())?)
        .build()?;
    let mut old = database.connect();
    let mut current = database.connect();
    current.execute("CREATE TABLE t(id INTEGER, observed VARCHAR DEFAULT lifecycle_default())")?;
    old.execute("SET SESSION default_order='DESC'; BEGIN")?;
    current.execute("ALTER TABLE t ALTER COLUMN observed SET DEFAULT 'new'")?;

    old.execute("INSERT INTO t(id) VALUES (1)")?;
    assert_eq!(
        old.query("SELECT * FROM t")?.rows,
        vec![vec![integer(1), string("DESC:1")]]
    );
    old.execute("ROLLBACK")?;
    current.execute("INSERT INTO t(id) VALUES (2)")?;
    assert_eq!(
        current.query("SELECT * FROM t")?.rows,
        vec![vec![integer(2), string("new")]]
    );

    current.execute(
        "BEGIN; ALTER TABLE t ALTER COLUMN observed SET DEFAULT lifecycle_default(); \
         INSERT INTO t(id) VALUES (3)",
    )?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    current.execute("ROLLBACK")?;
    current.execute("INSERT INTO t(id) VALUES (4)")?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        current.query("SELECT * FROM t ORDER BY id")?.rows,
        vec![
            vec![integer(2), string("new")],
            vec![integer(4), string("new")],
        ]
    );

    current.execute(
        "BEGIN; CREATE TABLE transient(v VARCHAR DEFAULT lifecycle_default()); ROLLBACK",
    )?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    assert!(current.query("SELECT * FROM transient").is_err());

    current.execute(
        "CREATE TABLE failing(v INTEGER DEFAULT CAST('bad' AS INTEGER)); \
         INSERT INTO failing VALUES (7)",
    )?;
    assert!(
        current
            .execute("INSERT INTO failing DEFAULT VALUES")
            .is_err()
    );
    assert_eq!(
        current.query("SELECT * FROM failing")?.rows,
        vec![vec![integer(7)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn retained_function_default_is_rebound_after_reopen_without_creation_effects() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("default-lifecycle.json");
    let state = Arc::new(EffectState::default());
    let open = || {
        DatabaseBuilder::new()
            .functions(functions(state.clone())?)
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()
    };
    {
        let mut connection = open()?.connect();
        connection.execute(
            "CREATE TABLE persisted(id INTEGER, observed VARCHAR DEFAULT lifecycle_default()); \
             INSERT INTO persisted VALUES (1,'supplied')",
        )?;
    }
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
    {
        let mut connection = open()?.connect();
        connection.execute("SET SESSION default_order='DESC'")?;
        connection.execute("INSERT INTO persisted(id) VALUES (2),(3)")?;
        assert_eq!(
            connection
                .query("SELECT * FROM persisted ORDER BY id ASC")?
                .rows,
            vec![
                vec![integer(1), string("supplied")],
                vec![integer(2), string("DESC:1")],
                vec![integer(3), string("DESC:2")],
            ]
        );
    }
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    Ok(())
}
