use super::*;

/// Composition is explicit; adapters receive only the contracts they consume.
pub struct DatabaseBuilder {
    durability: Option<Arc<dyn Durability>>,
    transactions: Option<Arc<dyn TransactionManager>>,
    indexes: Option<Arc<dyn IndexFactory>>,
    parser: Arc<dyn Parser>,
    binder: Arc<dyn Binder>,
    optimizer: Arc<dyn Optimizer>,
    physical_planner: Arc<dyn PhysicalPlanner>,
    executor: Arc<dyn Executor>,
    expressions: Arc<dyn ExpressionEvaluator>,
    subqueries: Arc<dyn SubqueryExecutor>,
    scheduler: Arc<dyn Scheduler>,
    configuration: Arc<dyn settings::Configuration>,
    functions: FunctionRegistry,
    casts: CastRegistry,
    operators: OperatorRegistry,
    types: Option<Arc<crate::common::type_registry::TypeRegistry>>,
    batch_size: usize,
    max_intermediate_rows: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for DatabaseBuilder {
    fn default() -> Self {
        Self {
            durability: None,
            transactions: None,
            indexes: None,
            parser: Arc::new(DuckDbParser),
            binder: Arc::new(SqlBinder),
            optimizer: Arc::new(PipelineOptimizer::default()),
            physical_planner: Arc::new(NativePhysicalPlanner::default()),
            executor: Arc::new(PullExecutor),
            expressions: Arc::new(BatchedEvaluator),
            subqueries: Arc::new(StreamingSubqueries),
            scheduler: Arc::new(InlineScheduler),
            configuration: Arc::new(settings::SnapshotConfiguration::default()),
            functions: FunctionRegistry::builtins(),
            casts: CastRegistry::builtins(),
            operators: OperatorRegistry::builtins(),
            types: None,
            batch_size: 2048,
            max_intermediate_rows: 10_000_000,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DatabaseBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn durability(mut self, adapter: Arc<dyn Durability>) -> Self {
        self.durability = Some(adapter);
        self
    }
    pub fn transactions(mut self, adapter: Arc<dyn TransactionManager>) -> Self {
        self.transactions = Some(adapter);
        self
    }
    pub fn indexes(mut self, adapter: Arc<dyn IndexFactory>) -> Self {
        self.indexes = Some(adapter);
        self
    }
    pub fn parser(mut self, adapter: Arc<dyn Parser>) -> Self {
        self.parser = adapter;
        self
    }
    pub fn binder(mut self, adapter: Arc<dyn Binder>) -> Self {
        self.binder = adapter;
        self
    }
    pub fn optimizer(mut self, adapter: Arc<dyn Optimizer>) -> Self {
        self.optimizer = adapter;
        self
    }
    pub fn physical_planner(mut self, adapter: Arc<dyn PhysicalPlanner>) -> Self {
        self.physical_planner = adapter;
        self
    }
    pub fn executor(mut self, adapter: Arc<dyn Executor>) -> Self {
        self.executor = adapter;
        self
    }
    pub fn expressions(mut self, adapter: Arc<dyn ExpressionEvaluator>) -> Self {
        self.expressions = adapter;
        self
    }
    pub fn subqueries(mut self, adapter: Arc<dyn SubqueryExecutor>) -> Self {
        self.subqueries = adapter;
        self
    }
    pub fn scheduler(mut self, adapter: Arc<dyn Scheduler>) -> Self {
        self.scheduler = adapter;
        self
    }
    pub fn configuration(mut self, adapter: Arc<dyn settings::Configuration>) -> Self {
        self.configuration = adapter;
        self
    }
    pub fn types(mut self, types: Arc<crate::common::type_registry::TypeRegistry>) -> Self {
        self.types = Some(types);
        self
    }
    pub fn casts(mut self, casts: CastRegistry) -> Self {
        self.casts = casts;
        self
    }
    pub fn operators(mut self, operators: OperatorRegistry) -> Self {
        self.operators = operators;
        self
    }
    pub fn functions(mut self, functions: FunctionRegistry) -> Self {
        self.functions = functions;
        self
    }
    pub fn batch_size(mut self, count: usize) -> Self {
        self.batch_size = count;
        self
    }
    pub fn max_intermediate_rows(mut self, count: usize) -> Self {
        self.max_intermediate_rows = count;
        self
    }
    pub fn build(self) -> Result<Database> {
        if self.batch_size == 0 || self.max_intermediate_rows == 0 {
            return Err(Error::Resource(
                "batch size and row limit must be positive".into(),
            ));
        }
        if self.transactions.is_some()
            && (self.durability.is_some() || self.indexes.is_some() || self.types.is_some())
        {
            return Err(Error::Unsupported(
                "select durability, indexes and types through the custom transaction manager's own composition".into(),
            ));
        }
        let transactions = match self.transactions {
            Some(manager) => manager,
            None => Arc::new(SnapshotTransactions::configured(
                self.durability
                    .unwrap_or_else(|| Arc::new(MemoryDurability)),
                self.indexes.unwrap_or_else(|| Arc::new(HashIndexFactory)),
                self.types
                    .unwrap_or_else(crate::common::type_registry::builtin_types),
            )?),
        };
        let query = QueryContext::background().with_types(transactions.types());
        let settings = self.configuration.connect().snapshot(&query)?;
        settings.ordering(None, None, &query)?;
        Ok(Database {
            services: Arc::new(Services {
                transactions,
                parser: self.parser,
                binder: self.binder,
                optimizer: self.optimizer,
                physical_planner: self.physical_planner,
                executor: self.executor,
                expressions: self.expressions,
                subqueries: self.subqueries,
                scheduler: self.scheduler,
                configuration: self.configuration,
                functions: self.functions,
                casts: self.casts,
                operators: self.operators,
                batch_size: self.batch_size,
                max_intermediate_rows: self.max_intermediate_rows,
            }),
        })
    }
}

#[derive(Clone)]
pub struct Database {
    pub(super) services: Arc<Services>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Database {
    pub fn memory() -> Result<Self> {
        DatabaseBuilder::new().build()
    }
    /// Opens or creates a native DuckDB checkpoint for reading and writing.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        DatabaseBuilder::new()
            .durability(Arc::new(
                FileCheckpoint::open(path, OpenMode::ReadWrite, Arc::new(DuckDbFormat::default()))?
                    .with_recovery(Arc::new(crate::storage::duckdb::wal::DuckDbWalRecovery))?,
            ))
            .build()
    }
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        DatabaseBuilder::new()
            .durability(Arc::new(
                FileCheckpoint::open(path, OpenMode::ReadOnly, Arc::new(DuckDbFormat::default()))?
                    .with_recovery(Arc::new(crate::storage::duckdb::wal::DuckDbWalRecovery))?,
            ))
            .build()
    }
    /// Open native files with append-and-sync transaction WAL durability.
    /// Reopening first checkpoints recovered work; ordinary commits append logs.
    pub fn open_logged(path: impl AsRef<Path>) -> Result<Self> {
        use crate::storage::{
            duckdb::wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
            logged::FileWal,
        };
        let checkpoint =
            FileCheckpoint::open(path, OpenMode::ReadWrite, Arc::new(DuckDbFormat::default()))?
                .with_recovery(Arc::new(DuckDbWalRecovery))?;
        DatabaseBuilder::new()
            .durability(Arc::new(FileWal::new(
                checkpoint,
                Arc::new(DuckDbTransactionLog),
            )?))
            .build()
    }
    /// Opens the private Rust format, which supports every native constraint.
    pub fn open_snapshot(path: impl AsRef<Path>) -> Result<Self> {
        DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()
    }
    pub fn connect(&self) -> Connection {
        Connection {
            services: self.services.clone(),
            session: Session::Idle,
            interrupt: InterruptHandle::default(),
            timeout: None,
            configuration: self.services.configuration.connect(),
        }
    }
    pub fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let s = &self.services;
        let mut adapters = s.transactions.adapters();
        adapters.extend(s.casts.adapters());
        adapters.extend(s.operators.adapters());
        adapters.extend(s.physical_planner.adapters());
        adapters.extend(s.optimizer.adapters());
        adapters.extend([
            ("parser", s.parser.name()),
            ("binder", s.binder.name()),
            ("executor", s.executor.name()),
            ("expressions", s.expressions.name()),
            ("subqueries", s.subqueries.name()),
            ("scheduler", s.scheduler.name()),
            ("configuration", s.configuration.name()),
        ]);
        adapters
    }
}
