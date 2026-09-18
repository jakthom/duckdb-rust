use duckdb_dev::{FileLog, Operation, TraceContext, TraceLayer, instrument};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tracing_subscriber::{Layer, Registry, layer::SubscriberExt};

fn capture(work: impl FnOnce()) -> (tempfile::TempDir, Vec<Value>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace.jsonl");
    let log = FileLog::create(&path).unwrap();
    tracing::subscriber::with_default(Registry::default().with(TraceLayer::new(log.clone())), work);
    log.flush().unwrap();
    let records = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (directory, records)
}

#[instrument]
fn child(value: i32) -> Result<i32, String> {
    if value < 0 {
        return Err("negative input".into());
    }
    Ok(value + 1)
}

#[instrument]
fn parent(value: i32) -> Result<i32, String> {
    Ok(child(value)? * 2)
}

#[instrument]
fn panic_operation() {
    panic!("test panic");
}

#[test]
fn nested_values_errors_and_panics_have_complete_correlated_records() {
    let (directory, records) = capture(|| {
        assert_eq!(parent(3).unwrap(), 8);
        assert_eq!(parent(-1).unwrap_err(), "negative input");
        assert!(std::panic::catch_unwind(panic_operation).is_err());
    });
    assert!(records.iter().any(|record| record["kind"] == "value"
        && record["fields"]["value_name"] == "return"
        && record["fields"]["value"] == "8"));
    let sites = records
        .iter()
        .filter(|record| {
            record["kind"] == "site" && !record["operation"].as_str().unwrap().starts_with("call::")
        })
        .map(|record| record["site"].as_u64().unwrap())
        .collect::<HashSet<_>>();
    let starts = records
        .iter()
        .filter(|record| {
            record["kind"] == "start" && sites.contains(&record["site"].as_u64().unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 5);
    let parents = records
        .iter()
        .filter(|record| record["kind"] == "start")
        .map(|record| (record["span"].as_u64().unwrap(), record["parent"].as_u64()))
        .collect::<HashMap<_, _>>();
    for (child, parent) in [(starts[1], starts[0]), (starts[3], starts[2])] {
        let mut ancestor = child["parent"].as_u64();
        while ancestor != parent["span"].as_u64() {
            ancestor = parents[&ancestor.expect("missing parent")];
        }
    }
    let summary = duckdb_dev::report::summarize(directory.path(), None).unwrap();
    assert!(summary.completed > 5);
    assert_eq!(summary.errors, 2);
    assert_eq!(summary.panics, 1);
    assert!(summary.incomplete.is_empty());
    assert!(
        records
            .iter()
            .filter(|record| record["kind"] == "end")
            .all(|record| record["elapsed_ns"].as_u64().is_some())
    );
}

#[instrument]
trait Borrowing {
    fn identity(&self) -> usize {
        7
    }
    fn get(&mut self) -> Result<&mut str, String>;
}
struct Borrowed(String);
#[instrument]
impl Borrowing for Borrowed {
    fn get(&mut self) -> Result<&mut str, String> {
        Ok(&mut self.0)
    }
}
#[instrument]
fn coerce(value: Borrowed) -> Result<Box<dyn Borrowing>, String> {
    Ok(Box::new(value))
}
#[instrument]
fn nested() -> i32 {
    fn helper() -> i32 {
        9
    }
    helper()
}

#[test]
fn instrumentation_preserves_borrows_coercions_and_default_interface_methods() {
    let (_, records) = capture(|| {
        let mut value = coerce(Borrowed("abc".into())).unwrap();
        assert_eq!(value.identity(), 7);
        value.get().unwrap().make_ascii_uppercase();
        assert_eq!(value.get().unwrap(), "ABC");
        assert_eq!(nested(), 9);
    });
    for name in [
        "coerce",
        "Borrowing::identity",
        "Borrowed as Borrowing::get",
        "nested::helper",
    ] {
        assert!(
            records
                .iter()
                .any(|record| record["kind"] == "site" && record["operation"] == name),
            "{name}"
        );
    }
}

#[test]
fn captured_context_preserves_parent_and_subscriber_across_threads_without_id_reuse() {
    let (directory, records) = capture(|| {
        let _root = Operation::enter(tracing::trace_span!(
            "root",
            outcome = tracing::field::Empty
        ));
        let context = TraceContext::capture();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let context = context.clone();
                scope.spawn(move || {
                    context.in_scope(|| {
                        for n in 0..100 {
                            child(n).unwrap();
                        }
                    })
                });
            }
        });
    });
    let starts = records
        .iter()
        .filter(|record| record["kind"] == "start")
        .collect::<Vec<_>>();
    assert!(starts.len() >= 401);
    let sites = records
        .iter()
        .filter(|record| record["kind"] == "site" && record["operation"] == "child")
        .map(|record| record["site"].as_u64().unwrap())
        .collect::<HashSet<_>>();
    let children = starts
        .iter()
        .filter(|record| sites.contains(&record["site"].as_u64().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 400);
    assert!(
        children
            .iter()
            .all(|record| record["parent"] == starts[0]["span"])
    );
    assert_eq!(
        starts
            .iter()
            .map(|record| record["span"].as_u64().unwrap())
            .collect::<HashSet<_>>()
            .len(),
        starts.len()
    );
    assert!(
        duckdb_dev::report::summarize(directory.path(), None)
            .unwrap()
            .incomplete
            .is_empty()
    );
}

#[test]
fn active_operation_is_flushed_before_it_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace.jsonl");
    let log = FileLog::create(&path).unwrap();
    tracing::subscriber::with_default(Registry::default().with(TraceLayer::new(log)), || {
        let _root = Operation::enter(tracing::trace_span!(
            "still_running",
            outcome = tracing::field::Empty
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if fs::read_to_string(&path)
                .unwrap()
                .contains("\"kind\":\"start\"")
            {
                break;
            }
            assert!(Instant::now() < deadline, "active trace was never flushed");
            std::thread::sleep(Duration::from_millis(10));
        }
        let summary = duckdb_dev::report::summarize(directory.path(), None).unwrap();
        assert_eq!(summary.incomplete.len(), 1);
        assert_eq!(summary.completed, 0);
    });
}

struct Counting(Arc<AtomicUsize>);
impl<S: tracing::Subscriber> Layer<S> for Counting {
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _: &tracing::Id,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if !attributes.metadata().name().starts_with("call::") {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[test]
fn alternate_subscriber_consumes_the_same_instrumentation_without_engine_changes() {
    let count = Arc::new(AtomicUsize::new(0));
    tracing::subscriber::with_default(Registry::default().with(Counting(count.clone())), || {
        assert_eq!(parent(1).unwrap(), 4);
    });
    assert_eq!(count.load(Ordering::Relaxed), 2);
}

#[test]
fn trace_files_are_never_overwritten_and_corruption_is_explicit() {
    let (directory, _) = capture(|| {
        child(1).unwrap();
    });
    let path = directory.path().join("trace.jsonl");
    let original = fs::read(&path).unwrap();
    assert!(FileLog::create(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::write(&path, "{\"kind\":\"process\",\"seq\":2}\n").unwrap();
    assert!(
        duckdb_dev::report::summarize(directory.path(), None)
            .unwrap_err()
            .to_string()
            .contains("sequence gap")
    );
    fs::write(&path, "{\"seq\":").unwrap();
    assert!(duckdb_dev::report::summarize(directory.path(), None).is_err());
}

trait External {
    fn answer(&self) -> i32;
}
struct ExternalAdapter;
impl External for ExternalAdapter {
    fn answer(&self) -> i32 {
        42
    }
}
#[instrument]
fn through_interface(adapter: &dyn External) -> i32 {
    adapter.answer()
}

#[test]
fn interface_calls_are_visible_even_without_instrumenting_the_external_adapter() {
    let (_, records) = capture(|| {
        assert_eq!(through_interface(&ExternalAdapter), 42);
    });
    assert!(
        records
            .iter()
            .any(|record| record["kind"] == "site" && record["operation"] == "call::answer")
    );
}

#[instrument]
fn borrowed_temporaries() -> Result<(), String> {
    fn accepts(_: &String) {}
    accepts(&Arc::new(String::new()));
    let mut value = String::from("abc");
    let count = value.to_ascii_lowercase().chars().count();
    assert_eq!(count, 3);
    let output = value.as_mut_str();
    output.make_ascii_uppercase();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorder.rs");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    assert!(file.metadata().map_err(|error| error.to_string())?.len() > 0);
    struct Nested;
    impl Nested {
        fn value(&self) -> i32 {
            7
        }
    }
    assert_eq!(Nested.value(), 7);
    Ok(())
}

#[test]
fn call_wrappers_preserve_temporary_lifetimes_and_nested_implementations() {
    let (_, records) = capture(|| borrowed_temporaries().unwrap());
    assert!(records.iter().any(|record| record["kind"] == "site"
        && record["operation"] == "borrowed_temporaries::Nested::value"));
}

#[instrument]
fn formatting_error() -> std::fmt::Result {
    Err(std::fmt::Error)
}

#[test]
fn result_alias_formatting_errors_are_recorded() {
    let (directory, _) = capture(|| {
        assert!(formatting_error().is_err());
    });
    let summary = duckdb_dev::report::summarize(directory.path(), None).unwrap();
    assert_eq!(summary.errors, 1);
}
