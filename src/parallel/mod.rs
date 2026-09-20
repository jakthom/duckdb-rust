use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::common::{Error, Result};

/// Database-wide requested-owned-storage admission control.  The pool records
/// only reservations made by participating execution paths; it deliberately
/// does not pretend to account for allocator overhead or uninstrumented
/// operators.
#[derive(Debug, Default)]
pub struct MemoryPool {
    state: Mutex<MemoryPoolState>,
}

#[derive(Debug, Default)]
struct MemoryPoolState {
    limit: Option<usize>,
    used: usize,
}

#[derive(Clone, Debug)]
pub enum Reservation {
    Single(Arc<ReservationInner>),
    /// Aggregating existing charges never re-admits bytes or creates a gap in
    /// accounting. Drop recursively releases the original pool charges.
    Many(Arc<[Reservation]>),
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Reservation {
    pub fn bytes(&self) -> usize {
        match self {
            Self::Single(inner) => inner.bytes,
            Self::Many(tokens) => tokens
                .iter()
                .fold(0usize, |bytes, token| bytes.saturating_add(token.bytes())),
        }
    }
    pub fn merge(tokens: Vec<Reservation>) -> Self {
        Self::Many(tokens.into())
    }
}

#[derive(Debug)]
pub struct ReservationInner {
    pool: Arc<MemoryPool>,
    bytes: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Drop for ReservationInner {
    fn drop(&mut self) {
        // A poisoned accounting lock must not make destruction panic. The
        // process is already unable to make reliable admission decisions.
        if let Ok(mut state) = self.pool.state.lock() {
            state.used = state.used.saturating_sub(self.bytes);
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl MemoryPool {
    /// Serialize a configuration publication with its cap update. The caller
    /// performs no externally visible write before this method checks the
    /// proposed cap; reservations cannot interleave the commit.
    pub fn publish_with<T>(
        &self,
        limit: Option<usize>,
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("memory pool lock poisoned".into()))?;
        if limit.is_some_and(|limit| state.used > limit) {
            return Err(Error::Resource(
                "cannot lower memory limit below retained reservations".into(),
            ));
        }
        let value = publish()?;
        state.limit = limit;
        Ok(value)
    }
    pub fn reserve(self: &Arc<Self>, bytes: usize, query: &QueryContext) -> Result<Reservation> {
        query.check()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("memory pool lock poisoned".into()))?;
        let next = state
            .used
            .checked_add(bytes)
            .ok_or_else(|| Error::Resource("memory reservation accounting overflow".into()))?;
        if state.limit.is_some_and(|limit| next > limit) {
            return Err(Error::Resource("memory limit exceeded".into()));
        }
        state.used = next;
        Ok(Reservation::Single(Arc::new(ReservationInner {
            pool: self.clone(),
            bytes,
        })))
    }

    pub fn publish_limit(&self, limit: Option<usize>) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("memory pool lock poisoned".into()))?;
        if limit.is_some_and(|limit| state.used > limit) {
            return Err(Error::Resource(
                "cannot lower memory limit below retained reservations".into(),
            ));
        }
        state.limit = limit;
        Ok(())
    }

    pub fn limit(&self) -> Result<Option<usize>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| Error::Internal("memory pool lock poisoned".into()))?
            .limit)
    }
    pub fn used(&self) -> Result<usize> {
        Ok(self
            .state
            .lock()
            .map_err(|_| Error::Internal("memory pool lock poisoned".into()))?
            .used)
    }
}

#[derive(Clone, Default)]
pub struct InterruptHandle(Arc<AtomicBool>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl InterruptHandle {
    pub fn interrupt(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn reset(&self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct QueryContext {
    interrupt: InterruptHandle,
    deadline: Option<Instant>,
    batch_size: usize,
    max_intermediate_rows: usize,
    types: Arc<crate::common::type_registry::TypeRegistry>,
    bound_types:
        Arc<RwLock<HashMap<crate::common::DataType, crate::common::type_registry::BoundType>>>,
    settings: crate::main::settings::SettingsSnapshot,
    stored_expressions: Option<Arc<dyn crate::catalog::expression::StoredExpressionEvaluator>>,
    transaction_timestamp_micros: Option<i64>,
    memory_pool: Arc<MemoryPool>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl QueryContext {
    pub fn with_stored_expressions(
        mut self,
        expressions: Arc<dyn crate::catalog::expression::StoredExpressionEvaluator>,
    ) -> Self {
        self.stored_expressions = Some(expressions);
        self
    }
    /// Attach the already sampled transaction-start instant. Construction does
    /// not consult a clock; transaction ownership remains with the runtime.
    pub fn with_transaction_timestamp(mut self, micros: i64) -> Self {
        self.transaction_timestamp_micros = Some(micros);
        self
    }
    /// Return the transaction-start UTC instant in microseconds since the Unix
    /// epoch. Background contexts reject this capability instead of consulting
    /// the host clock implicitly.
    pub fn transaction_timestamp_micros(&self) -> Result<i64> {
        self.check()?;
        self.transaction_timestamp_micros.ok_or_else(|| {
            Error::Unsupported("no transaction timestamp is available in this context".into())
        })
    }
    /// Missing composition is a capability error, never a request to construct
    /// ambient builtin functions/casts in a decoder or recovery implementation.
    pub fn stored_expressions(
        &self,
    ) -> Result<&dyn crate::catalog::expression::StoredExpressionEvaluator> {
        self.check()?;
        self.stored_expressions
            .as_deref()
            .ok_or_else(|| Error::Unsupported("no catalog expression evaluator selected".into()))
    }
    pub fn settings(&self) -> &crate::main::settings::SettingsSnapshot {
        &self.settings
    }
    pub fn with_settings(mut self, settings: crate::main::settings::SettingsSnapshot) -> Self {
        self.settings = settings;
        self
    }
    pub fn types(&self) -> &crate::common::type_registry::TypeRegistry {
        &self.types
    }
    pub fn type_registry(&self) -> Arc<crate::common::type_registry::TypeRegistry> {
        self.types.clone()
    }
    /// Reuse immutable type bindings within one query. Binding remains fully
    /// validated on the first request; execution no longer rebuilds nested
    /// adapter state for every value from the same bound expression.
    pub fn bind_type(
        &self,
        data_type: &crate::common::DataType,
    ) -> Result<crate::common::type_registry::BoundType> {
        self.check()?;
        if let Some(bound) = self
            .bound_types
            .read()
            .map_err(|_| Error::Internal("query type cache lock poisoned".into()))?
            .get(data_type)
            .cloned()
        {
            return Ok(bound);
        }
        let bound = self.types.bind(data_type)?;
        let mut cache = self
            .bound_types
            .write()
            .map_err(|_| Error::Internal("query type cache lock poisoned".into()))?;
        Ok(cache
            .entry(data_type.clone())
            .or_insert_with(|| bound.clone())
            .clone())
    }
    pub fn with_types(mut self, types: Arc<crate::common::type_registry::TypeRegistry>) -> Self {
        self.types = types;
        self.bound_types = Arc::new(RwLock::new(HashMap::new()));
        self
    }
    pub fn with_memory_pool(mut self, memory_pool: Arc<MemoryPool>) -> Self {
        self.memory_pool = memory_pool;
        self
    }
    pub fn memory_pool(&self) -> &Arc<MemoryPool> {
        &self.memory_pool
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }
    pub fn max_intermediate_rows(&self) -> usize {
        self.max_intermediate_rows
    }
    pub fn batch_demand(&self, requested: usize) -> Result<usize> {
        self.check()?;
        if requested == 0 {
            return Err(Error::Internal("batch demand must be positive".into()));
        }
        Ok(requested.min(self.max_intermediate_rows))
    }
    /// Synchronous maintenance outside a client request. File adapters apply
    /// their own size limits; this context has no deadline or row-count limit.
    pub fn background() -> Self {
        Self {
            interrupt: InterruptHandle::default(),
            deadline: None,
            batch_size: 2048,
            max_intermediate_rows: usize::MAX,
            types: crate::common::type_registry::builtin_types(),
            bound_types: Arc::new(RwLock::new(HashMap::new())),
            settings: crate::main::settings::SettingsSnapshot::default(),
            stored_expressions: None,
            transaction_timestamp_micros: None,
            memory_pool: Arc::new(MemoryPool::default()),
        }
    }
    pub fn new(
        interrupt: InterruptHandle,
        timeout: Option<Duration>,
        batch_size: usize,
        max_intermediate_rows: usize,
    ) -> Result<Self> {
        if batch_size == 0 || max_intermediate_rows == 0 {
            return Err(Error::Resource(
                "batch size and row limit must be positive".into(),
            ));
        }
        Ok(Self {
            interrupt,
            deadline: timeout.and_then(|d| Instant::now().checked_add(d)),
            batch_size,
            max_intermediate_rows,
            types: crate::common::type_registry::builtin_types(),
            bound_types: Arc::new(RwLock::new(HashMap::new())),
            settings: crate::main::settings::SettingsSnapshot::default(),
            stored_expressions: None,
            transaction_timestamp_micros: None,
            memory_pool: Arc::new(MemoryPool::default()),
        })
    }
    pub fn check(&self) -> Result<()> {
        if self.interrupt.0.load(Ordering::Acquire)
            || self.deadline.is_some_and(|d| Instant::now() >= d)
        {
            Err(Error::Interrupted)
        } else {
            Ok(())
        }
    }
    pub fn check_rows(&self, count: usize) -> Result<()> {
        self.check()?;
        if count > self.max_intermediate_rows {
            Err(Error::Resource(format!(
                "{count} rows exceed the configured intermediate row limit {}",
                self.max_intermediate_rows
            )))
        } else {
            Ok(())
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// A task is driven exactly once; errors and cancellation reach its caller.
pub trait Scheduler: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, context: &QueryContext, task: &mut dyn FnMut() -> Result<()>) -> Result<()>;
}

#[derive(Default)]
pub struct InlineScheduler;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Scheduler for InlineScheduler {
    fn name(&self) -> &'static str {
        "inline"
    }
    fn run(&self, context: &QueryContext, task: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        context.check()?;
        task()
    }
}
