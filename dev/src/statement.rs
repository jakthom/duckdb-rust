//! SQL identity and scoped evidence, independent of the engine's syntax and result types.
use crate::{FileLog, Operation, TraceLayer};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fmt::Display,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tracing_subscriber::{Registry, layer::SubscriberExt};

static NEXT_EXECUTION: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static CURRENT: RefCell<Option<Arc<Identity>>> = const { RefCell::new(None) };
}

pub(crate) struct Identity {
    pub id: String,
    directory: PathBuf,
}

pub(crate) fn current() -> Option<Arc<Identity>> {
    CURRENT.with(|current| current.borrow().clone())
}

pub(crate) fn run_directory() -> Option<PathBuf> {
    current()?.directory.parent()?.parent().map(Path::to_owned)
}

pub(crate) fn in_scope<T>(identity: Option<Arc<Identity>>, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<Identity>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|current| *current.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CURRENT.with(|current| current.replace(identity)));
    work()
}

/// Content identity; execution IDs remain distinct even for identical SQL and parameters.
pub fn sql_hash(sql: &str) -> String {
    format!("{:x}", Sha256::digest(sql.as_bytes()))
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut output = io::BufWriter::new(crate::budget::Writer(fs::File::create(&temporary)?));
    serde_json::to_writer(&mut output, value)?;
    output.flush()?;
    drop(output);
    fs::rename(temporary, path)
}

fn required<T>(result: io::Result<T>) -> T {
    result.unwrap_or_else(|error| crate::recorder::fail(error))
}

/// Store typed result metadata/preview separately from operation records.
/// The engine chooses the preview bound and reports any omitted rows explicitly.
pub fn output(value: &impl Serialize) {
    if let Some(identity) = current() {
        required(atomic_json(&identity.directory.join("result.json"), value));
    }
}

pub fn parameters(value: &impl Serialize) {
    if let Some(identity) = current() {
        required(atomic_json(
            &identity.directory.join("parameters.json"),
            value,
        ));
    }
}

struct Evidence {
    identity: Arc<Identity>,
    metadata: Value,
    log: FileLog,
    started: Instant,
}

impl Drop for Evidence {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.metadata["status"] = "panic".into();
        }
        self.metadata["elapsed_ns"] = json!(self.started.elapsed().as_nanos());
        self.log
            .record(json!({"kind": "statement_end", "statement": self.metadata}));
        required(self.log.flush());
        required(atomic_json(
            &self.identity.directory.join("statement.json"),
            &self.metadata,
        ));
    }
}

/// Run one parse/prepare request or one execution in a separate subscriber and file.
/// Early returns, errors, panic unwinding, nested SQL and concurrent connections
/// retain their own identity. A request links the individual statements it parsed.
pub fn run<T, E: Display>(
    sql: &str,
    phase: &str,
    work: impl FnOnce() -> Result<T, E>,
    describe: impl FnOnce(&T),
) -> Result<T, E> {
    let Some(directory) = crate::directory() else {
        return work();
    };
    run_at(&directory, sql, phase, work, describe)
}

fn run_at<T, E: Display>(
    directory: &Path,
    sql: &str,
    phase: &str,
    work: impl FnOnce() -> Result<T, E>,
    describe: impl FnOnce(&T),
) -> Result<T, E> {
    crate::init();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let id = format!(
        "{stamp}-{}-{}",
        std::process::id(),
        NEXT_EXECUTION.fetch_add(1, Ordering::Relaxed)
    );
    let statements = directory.join("statements");
    required(fs::create_dir_all(&statements));
    let directory = statements.join(&id);
    required(fs::create_dir(&directory));
    let identity = Arc::new(Identity {
        id: id.clone(),
        directory,
    });
    let path = identity.directory.join("trace.jsonl");
    let metadata = json!({"execution_id": id, "sql_hash": sql_hash(sql), "sql": sql,
        "phase": phase, "parent_execution_id": current().map(|parent| parent.id.clone()),
        "run": std::env::var("DUCKDB_DEV_RUN").ok(), "source": std::env::var("DUCKDB_DEV_SOURCE").ok(),
        "profile": std::env::var("DUCKDB_DEV_PROFILE").ok(), "unix_ns": stamp,
        "trace": path, "status": "running", "timing": "instrumented inclusive wall time"});
    required(atomic_json(
        &identity.directory.join("statement.json"),
        &metadata,
    ));
    let log = required(FileLog::create(&path));
    log.record(json!({"kind": "statement", "statement": metadata}));
    eprintln!("dev SQL {phase}: {id}\ntrace: {}", path.display());
    let mut evidence = Evidence {
        identity: identity.clone(),
        metadata,
        log: log.clone(),
        started: Instant::now(),
    };
    in_scope(Some(identity), || {
        tracing::subscriber::with_default(Registry::default().with(TraceLayer::new(log)), || {
            let operation = Operation::enter(tracing::trace_span!(
                "sql.statement",
                execution_id = id.as_str(),
                phase,
                outcome = tracing::field::Empty,
                error = tracing::field::Empty
            ));
            let result = work();
            operation.result(&result);
            match &result {
                Ok(value) => {
                    describe(value);
                    evidence.metadata["status"] = "ok".into();
                }
                Err(error) => {
                    evidence.metadata["status"] = "error".into();
                    evidence.metadata["error"] = error.to_string().into();
                }
            }
            result
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_worker_statements_and_panics_keep_identity_and_terminal_evidence() {
        let temporary = tempfile::tempdir().unwrap();
        let mut root = None;
        let mut children = Vec::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_at(
                temporary.path(),
                "SELECT scope_parent",
                "execute",
                || -> Result<(), String> {
                    root = current();
                    let context = crate::TraceContext::capture();
                    children = std::thread::scope(|scope| {
                        (0..2)
                            .map(|_| {
                                let context = context.clone();
                                scope.spawn(move || {
                                    context
                                        .in_scope(|| {
                                            run(
                                                "SELECT scope_child",
                                                "execute",
                                                || -> Result<_, String> {
                                                    crate::value("worker_value", &42);
                                                    Ok(current().unwrap())
                                                },
                                                |_| {},
                                            )
                                        })
                                        .unwrap()
                                })
                            })
                            .collect::<Vec<_>>()
                            .into_iter()
                            .map(|child| child.join().unwrap())
                            .collect::<Vec<_>>()
                    });
                    panic!("intentional SQL scope panic");
                },
                |_| {},
            )
            .unwrap();
        }));
        assert!(result.is_err());
        let root = root.unwrap();
        assert_ne!(children[0].id, children[1].id);
        for child in children {
            let metadata: Value =
                serde_json::from_slice(&fs::read(child.directory.join("statement.json")).unwrap())
                    .unwrap();
            assert_eq!(metadata["parent_execution_id"], root.id);
            assert_eq!(metadata["status"], "ok");
            assert!(
                crate::report::summarize(&child.directory, None)
                    .unwrap()
                    .incomplete
                    .is_empty()
            );
        }
        let metadata: Value =
            serde_json::from_slice(&fs::read(root.directory.join("statement.json")).unwrap())
                .unwrap();
        assert_eq!(metadata["status"], "panic");
        let summary = crate::report::summarize(&root.directory, None).unwrap();
        assert_eq!(summary.panics, 1);
        assert!(summary.incomplete.is_empty());
        assert!(current().is_none());
    }
}
