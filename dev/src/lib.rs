//! Development-only, lossless operation records. No engine dependency or network service.
pub mod analytics;
pub mod artifacts;
mod budget;
pub mod coverage;
mod recorder;
pub mod report;
pub mod source;
pub mod statement;

pub use duckdb_dev_macros::{instrument, statement};
pub use recorder::{FileLog, TraceLayer};
pub use tracing;

use std::{fmt::Display, path::PathBuf, sync::OnceLock};
use tracing_subscriber::{Registry, layer::SubscriberExt};

static DEFAULT_LOG: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn directory() -> Option<PathBuf> {
    std::env::var_os("DUCKDB_DEV_LOG_DIR")
        .map(PathBuf::from)
        .or_else(statement::run_directory)
        .or_else(|| {
            DEFAULT_LOG
                .get()
                .and_then(|path| path.parent().map(PathBuf::from))
        })
}

/// Invoke through FnOnce so returned borrows retain the original function's
/// ownership contract, including mutable borrows and trait-object coercions.
#[doc(hidden)]
pub fn call<T>(operation: impl FnOnce() -> T) -> T {
    operation()
}

/// Complete a call expression without a new lexical temporary scope or a new
/// coercion site. A plain wrapping block changes borrowed-temporary lifetimes.
#[doc(hidden)]
pub trait FinishCall {
    type Value;
    fn __duckdb_dev_finish_call(self) -> Self::Value;
}
impl<T> FinishCall for (Operation, T) {
    type Value = T;
    fn __duckdb_dev_finish_call(self) -> T {
        let (operation, value) = self;
        drop(operation);
        value
    }
}

/// Install the dev recorder once, before constructing the first operation span.
/// A caller-provided scoped subscriber is respected (useful for embedding and tests).
pub fn init() {
    if DEFAULT_LOG.get().is_some()
        || tracing::dispatcher::get_default(|dispatch| {
            !dispatch.is::<tracing::subscriber::NoSubscriber>()
        })
    {
        return;
    }
    let Some(directory) = directory() else {
        return;
    };
    DEFAULT_LOG.get_or_init(|| {
        std::fs::create_dir_all(&directory).expect("create dev trace directory");
        let path = directory.join(format!("{}.jsonl", std::process::id()));
        let log = FileLog::create(&path).expect("create dev trace log");
        let subscriber = Registry::default().with(TraceLayer::new(log));
        tracing::subscriber::set_global_default(subscriber).expect("install dev trace subscriber");
        eprintln!("dev trace: {}", path.display());
        path
    });
}

/// A synchronous operation. Its lifetime spans execution and unwinding.
/// Async work must carry a span and enter it separately on every poll.
pub struct Operation {
    span: tracing::Span,
    _entered: tracing::span::EnteredSpan,
}

impl Operation {
    pub fn enter(span: tracing::Span) -> Self {
        span.record("outcome", "returned");
        Self {
            _entered: span.clone().entered(),
            span,
        }
    }

    pub fn result<T, E: Display>(&self, result: &Result<T, E>) {
        match result {
            Ok(_) => {
                self.span.record("outcome", "ok");
            }
            Err(error) => {
                self.span.record("outcome", "error");
                let message = error.to_string();
                self.span.record("error", message.as_str());
            }
        }
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.span.record("outcome", "panic");
        }
    }
}

/// Record a scalar or a work counter on the current operation. Display preserves
/// integer widths and non-finite floats; this never dumps arbitrary object graphs.
pub fn value(name: &str, value: &impl Display) {
    let value = value.to_string();
    tracing::event!(
        tracing::Level::TRACE,
        value_name = name,
        value = value.as_str()
    );
}

/// Make the current prefix visible before deliberate process termination.
pub fn flush() {
    tracing::event!(tracing::Level::TRACE, dev_flush = true);
}

/// Capture both identity and subscriber when handing work to a different thread.
#[derive(Clone)]
pub struct TraceContext {
    span: tracing::Span,
    dispatch: tracing::Dispatch,
    statement: Option<std::sync::Arc<statement::Identity>>,
}
impl TraceContext {
    pub fn capture() -> Self {
        Self {
            span: tracing::Span::current(),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            statement: statement::current(),
        }
    }
    pub fn in_scope<T>(&self, work: impl FnOnce() -> T) -> T {
        statement::in_scope(self.statement.clone(), || {
            tracing::dispatcher::with_default(&self.dispatch, || self.span.in_scope(work))
        })
    }
}
