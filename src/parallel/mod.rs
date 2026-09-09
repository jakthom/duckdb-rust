use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::common::{Error, Result};

#[derive(Clone, Default)]
pub struct InterruptHandle(Arc<AtomicBool>);

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
}

impl QueryContext {
    pub fn types(&self) -> &crate::common::type_registry::TypeRegistry {
        &self.types
    }
    pub fn type_registry(&self) -> Arc<crate::common::type_registry::TypeRegistry> {
        self.types.clone()
    }
    pub fn with_types(mut self, types: Arc<crate::common::type_registry::TypeRegistry>) -> Self {
        self.types = types;
        self
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

/// A task is driven exactly once; errors and cancellation reach its caller.
pub trait Scheduler: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, context: &QueryContext, task: &mut dyn FnMut() -> Result<()>) -> Result<()>;
}

#[derive(Default)]
pub struct InlineScheduler;
impl Scheduler for InlineScheduler {
    fn name(&self) -> &'static str {
        "inline"
    }
    fn run(&self, context: &QueryContext, task: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        context.check()?;
        task()
    }
}
