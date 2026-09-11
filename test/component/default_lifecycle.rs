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

#[derive(Debug)]
struct OrderedDefault(Arc<EffectState>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for OrderedDefault {
    fn name(&self) -> &str {
        "ordered_default"
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
        if arguments != [DataType::Varchar] {
            return Err(Error::Bind(
                "ordered_default accepts one VARCHAR argument".into(),
            ));
        }
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [Value::Varchar(label)] = arguments else {
            return Err(Error::Internal("bound ordered_default arguments".into()));
        };
        let call = self.0.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.0.fail_on.load(Ordering::SeqCst) == call {
            return Err(Error::Execution(format!(
                "ordered default failed on call {call}"
            )));
        }
        let Value::Varchar(setting) = query.settings().get("default_order", query)? else {
            return Err(Error::Internal("default_order is not VARCHAR".into()));
        };
        Ok(string(&format!("{setting}:{label}:{call}")))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn insert_defaults_run_column_major_and_stage_failures_before_rows() -> Result<()> {
    let state = Arc::new(EffectState::default());
    let mut registry = FunctionRegistry::builtins();
    registry.register_scalar(Arc::new(OrderedDefault(state.clone())))?;
    let mut connection = DatabaseBuilder::new()
        .functions(registry)
        .build()?
        .connect();
    connection.execute("SET SESSION default_order='DESC'")?;
    connection.execute(
        "CREATE TABLE ordered(
            id INTEGER,
            first VARCHAR DEFAULT ordered_default('first'),
            second VARCHAR DEFAULT ordered_default('second'),
            supplied VARCHAR DEFAULT ordered_default('bypass')
        )",
    )?;
    connection.execute(
        "INSERT INTO ordered(id,supplied) VALUES
            (1,'one'),(2,'two'),(3,'three')",
    )?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 6);
    assert_eq!(
        connection
            .query("SELECT * FROM ordered ORDER BY id ASC")?
            .rows,
        vec![
            vec![
                integer(1),
                string("DESC:first:1"),
                string("DESC:second:4"),
                string("one")
            ],
            vec![
                integer(2),
                string("DESC:first:2"),
                string("DESC:second:5"),
                string("two")
            ],
            vec![
                integer(3),
                string("DESC:first:3"),
                string("DESC:second:6"),
                string("three")
            ],
        ]
    );

    connection.execute(
        "INSERT INTO ordered(id,supplied)
         SELECT 99,'empty' WHERE false",
    )?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 6);
    connection.execute(
        "CREATE TABLE singleton(
            first VARCHAR DEFAULT ordered_default('single-first'),
            second VARCHAR DEFAULT ordered_default('single-second')
        ); INSERT INTO singleton DEFAULT VALUES",
    )?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 8);
    assert_eq!(
        connection.query("SELECT * FROM singleton")?.rows,
        vec![vec![
            string("DESC:single-first:7"),
            string("DESC:single-second:8")
        ]]
    );

    state.fail_on.store(13, Ordering::SeqCst);
    assert!(
        connection
            .execute(
                "INSERT INTO ordered(id,supplied) VALUES
                    (10,'ten'),(11,'eleven'),(12,'twelve')"
            )
            .is_err()
    );
    // Calls 9..11 finish `first`; `second` fails on its second row. The third
    // second-column default and every bypassed default remain unevaluated.
    assert_eq!(state.calls.load(Ordering::SeqCst), 13);
    assert_eq!(
        connection.query("SELECT count(*) FROM ordered")?.rows,
        vec![vec![integer(3)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn insert_defaults_advance_by_duckdb_standard_vectors() -> Result<()> {
    let state = Arc::new(EffectState::default());
    let mut registry = FunctionRegistry::builtins();
    registry.register_scalar(Arc::new(OrderedDefault(state.clone())))?;
    let mut connection = DatabaseBuilder::new()
        .functions(registry)
        // Execution batching is configurable, but DuckDB default effects use
        // its fixed standard-vector boundary.
        .batch_size(7)
        .build()?
        .connect();
    connection.execute("SET SESSION default_order='DESC'")?;
    connection.execute(
        "CREATE TABLE vector_order(
            id INTEGER,
            first VARCHAR DEFAULT ordered_default('first'),
            second VARCHAR DEFAULT ordered_default('second')
        );
        INSERT INTO vector_order(id)
        SELECT range::INTEGER FROM range(2050)",
    )?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 4100);
    assert_eq!(
        connection
            .query(
                "SELECT * FROM vector_order
                 WHERE id IN (0,2047,2048,2049)
                 ORDER BY id ASC"
            )?
            .rows,
        vec![
            vec![
                integer(0),
                string("DESC:first:1"),
                string("DESC:second:2049")
            ],
            vec![
                integer(2047),
                string("DESC:first:2048"),
                string("DESC:second:4096")
            ],
            vec![
                integer(2048),
                string("DESC:first:4097"),
                string("DESC:second:4099")
            ],
            vec![
                integer(2049),
                string("DESC:first:4098"),
                string("DESC:second:4100")
            ],
        ]
    );

    state.calls.store(0, Ordering::SeqCst);
    connection.execute(
        "CREATE TABLE vector_order_3000(
            id INTEGER,
            first VARCHAR DEFAULT ordered_default('first'),
            second VARCHAR DEFAULT ordered_default('second')
        );
        INSERT INTO vector_order_3000(id)
        SELECT range::INTEGER FROM range(3000)",
    )?;
    assert_eq!(state.calls.load(Ordering::SeqCst), 6000);
    assert_eq!(
        connection
            .query(
                "SELECT * FROM vector_order_3000
                 WHERE id IN (0,2047,2048,2999)
                 ORDER BY id ASC"
            )?
            .rows,
        vec![
            vec![
                integer(0),
                string("DESC:first:1"),
                string("DESC:second:2049")
            ],
            vec![
                integer(2047),
                string("DESC:first:2048"),
                string("DESC:second:4096")
            ],
            vec![
                integer(2048),
                string("DESC:first:4097"),
                string("DESC:second:5049")
            ],
            vec![
                integer(2999),
                string("DESC:first:5048"),
                string("DESC:second:6000")
            ],
        ]
    );

    state.calls.store(0, Ordering::SeqCst);
    state.fail_on.store(2050, Ordering::SeqCst);
    connection.execute(
        "CREATE TABLE failing_vector(
            id INTEGER,
            first VARCHAR DEFAULT ordered_default('first'),
            second VARCHAR DEFAULT ordered_default('second')
        )",
    )?;
    assert!(
        connection
            .execute(
                "INSERT INTO failing_vector(id)
                 SELECT range::INTEGER FROM range(2050)"
            )
            .is_err()
    );
    // The second column begins after the first 2048-row vector. Its second
    // evaluation fails before the next vector or any row reaches storage.
    assert_eq!(state.calls.load(Ordering::SeqCst), 2050);
    assert_eq!(
        connection
            .query("SELECT count(*) FROM failing_vector")?
            .rows,
        vec![vec![integer(0)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn insert_source_and_defaults_share_duckdb_vector_pipeline() -> Result<()> {
    use duckdb_rust::execution::{Executor, MaterializingExecutor, PullExecutor};

    for executor in [
        Arc::new(PullExecutor) as Arc<dyn Executor>,
        Arc::new(MaterializingExecutor),
    ] {
        let state = Arc::new(EffectState::default());
        let mut registry = FunctionRegistry::builtins();
        registry.register_scalar(Arc::new(OrderedDefault(state.clone())))?;
        let mut connection = DatabaseBuilder::new()
            .functions(registry)
            .executor(executor)
            .batch_size(7)
            .build()?
            .connect();
        connection.execute("SET SESSION default_order='DESC'")?;
        connection.execute(
            "CREATE TABLE vector_source(
                id INTEGER,
                source VARCHAR,
                first VARCHAR DEFAULT ordered_default('first'),
                second VARCHAR DEFAULT ordered_default('second')
            );
            INSERT INTO vector_source(id,source)
            SELECT range::INTEGER,ordered_default('source') FROM range(2050)",
        )?;
        assert_eq!(state.calls.load(Ordering::SeqCst), 6150);
        assert_eq!(
            connection
                .query(
                    "SELECT * FROM vector_source
                     WHERE id IN (0,2047,2048,2049)
                     ORDER BY id ASC"
                )?
                .rows,
            vec![
                vec![
                    integer(0),
                    string("DESC:source:1"),
                    string("DESC:first:2049"),
                    string("DESC:second:4097")
                ],
                vec![
                    integer(2047),
                    string("DESC:source:2048"),
                    string("DESC:first:4096"),
                    string("DESC:second:6144")
                ],
                vec![
                    integer(2048),
                    string("DESC:source:6145"),
                    string("DESC:first:6147"),
                    string("DESC:second:6149")
                ],
                vec![
                    integer(2049),
                    string("DESC:source:6146"),
                    string("DESC:first:6148"),
                    string("DESC:second:6150")
                ],
            ]
        );

        state.calls.store(0, Ordering::SeqCst);
        connection.execute(
            "CREATE TABLE vector_union(
                id INTEGER,
                first VARCHAR DEFAULT ordered_default('first'),
                second VARCHAR DEFAULT ordered_default('second')
            );
            INSERT INTO vector_union(id)
            SELECT range::INTEGER FROM range(1000)
            UNION ALL
            SELECT (range+1000)::INTEGER FROM range(1050)",
        )?;
        assert_eq!(state.calls.load(Ordering::SeqCst), 4100);
        assert_eq!(
            connection
                .query(
                    "SELECT * FROM vector_union
                     WHERE id IN (0,999,1000,2047,2048,2049)
                     ORDER BY id ASC"
                )?
                .rows,
            vec![
                vec![
                    integer(0),
                    string("DESC:first:1"),
                    string("DESC:second:1001")
                ],
                vec![
                    integer(999),
                    string("DESC:first:1000"),
                    string("DESC:second:2000")
                ],
                vec![
                    integer(1000),
                    string("DESC:first:2001"),
                    string("DESC:second:3051")
                ],
                vec![
                    integer(2047),
                    string("DESC:first:3048"),
                    string("DESC:second:4098")
                ],
                vec![
                    integer(2048),
                    string("DESC:first:3049"),
                    string("DESC:second:4099")
                ],
                vec![
                    integer(2049),
                    string("DESC:first:3050"),
                    string("DESC:second:4100")
                ],
            ]
        );
    }
    Ok(())
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
