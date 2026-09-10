use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tracing::{
    Id, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Record},
};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

/// A single process owns each file. Writers block rather than dropping records.
/// A short periodic flush makes starts visible even if the operation hangs.
#[derive(Clone)]
pub struct FileLog(Arc<Owner>);

struct Owner {
    output: Arc<Mutex<Output>>,
    _lease: Option<File>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        let mut output = self.output.lock().expect("dev trace output mutex");
        output.active = false;
        output.snapshot().unwrap_or_else(|error| fail(error));
    }
}

type Outputs = Mutex<Vec<Weak<Mutex<Output>>>>;
static OUTPUTS: OnceLock<Outputs> = OnceLock::new();

fn register(output: &Arc<Mutex<Output>>) {
    let outputs = OUTPUTS.get_or_init(|| {
        std::thread::Builder::new()
            .name("dev-trace-flush".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(Duration::from_millis(25));
                    let Some(outputs) = OUTPUTS.get() else {
                        continue;
                    };
                    let pending = {
                        let mut outputs = outputs.lock().expect("dev trace outputs");
                        let pending = outputs.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
                        outputs.retain(|output| output.strong_count() != 0);
                        pending
                    };
                    for output in pending {
                        let mut output = output.lock().expect("dev trace output mutex");
                        if !output.active {
                            continue;
                        }
                        let result = output.writer.flush().and_then(|()| {
                            if output.snapshot_at.elapsed() >= Duration::from_millis(500) {
                                output.snapshot()?;
                            }
                            Ok(())
                        });
                        if let Err(error) = result {
                            fail(error);
                        }
                    }
                }
            })
            .unwrap_or_else(|error| fail(error));
        Mutex::new(Vec::new())
    });
    outputs
        .lock()
        .expect("dev trace outputs")
        .push(Arc::downgrade(output));
}

static NEXT_SPAN: AtomicU64 = AtomicU64::new(1);

struct Output {
    active: bool,
    path: PathBuf,
    writer: BufWriter<crate::budget::Writer<File>>,
    epoch: Instant,
    sequence: u64,
    sites: HashMap<tracing::callsite::Identifier, u64>,
    summary: crate::report::Accumulator,
    snapshot_at: Instant,
}

impl FileLog {
    pub fn create(path: &Path) -> io::Result<Self> {
        let lease = crate::artifacts::recording_lease()?;
        let file = OpenOptions::new().create_new(true).write(true).open(path)?;
        let output = Arc::new(Mutex::new(Output {
            active: true,
            path: path.to_owned(),
            writer: BufWriter::with_capacity(64 * 1024, crate::budget::Writer(file)),
            epoch: Instant::now(),
            sequence: 0,
            sites: HashMap::new(),
            summary: crate::report::Accumulator::default(),
            snapshot_at: Instant::now(),
        }));
        let log = Self(Arc::new(Owner {
            output: output.clone(),
            _lease: lease,
        }));
        log.write(json!({
            "kind": "process", "schema": 1, "pid": std::process::id(),
            "unix_ns": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos(),
            "command": std::env::args_os().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            "run": std::env::var("DUCKDB_DEV_RUN").ok(),
            "profile": std::env::var("DUCKDB_DEV_PROFILE").ok(),
            "source": std::env::var("DUCKDB_DEV_SOURCE").ok(),
            "timing": "instrumented wall time; includes tracing and scheduling overhead",
        }));
        log.flush()?;
        register(&output);
        Ok(log)
    }

    fn write(&self, record: Value) {
        let mut output = self.0.output.lock().expect("dev trace output mutex");
        output.write(record).unwrap_or_else(|error| fail(error));
    }

    pub(crate) fn record(&self, record: Value) {
        self.write(record);
    }

    pub fn flush(&self) -> io::Result<()> {
        self.0
            .output
            .lock()
            .expect("dev trace output mutex")
            .snapshot()
    }

    fn site(&self, metadata: &'static tracing::Metadata<'static>) -> u64 {
        let mut output = self.0.output.lock().expect("dev trace output mutex");
        if let Some(id) = output.sites.get(&metadata.callsite()) {
            return *id;
        }
        let site = output.sites.len() as u64 + 1;
        output.sites.insert(metadata.callsite(), site);
        output
            .write(
                json!({"kind": "site", "site": site, "operation": metadata.name(),
            "module": metadata.module_path(), "file": metadata.file(), "line": metadata.line()}),
            )
            .unwrap_or_else(|error| fail(error));
        site
    }
}

impl Output {
    fn write(&mut self, mut record: Value) -> io::Result<()> {
        self.sequence += 1;
        record["seq"] = self.sequence.into();
        record["at_ns"] = json!(self.epoch.elapsed().as_nanos());
        serde_json::to_writer(&mut self.writer, &record)?;
        self.writer.write_all(b"\n")?;
        self.summary.push(&self.path, &record)
    }

    fn snapshot(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        let metadata = self.writer.get_ref().0.metadata()?;
        let snapshot = crate::report::Snapshot {
            bytes: metadata.len(),
            modified_ns: crate::report::modified_ns(&metadata)?,
            summary: self.summary.snapshot(&self.path),
        };
        crate::statement::atomic_json(&self.path.with_extension("summary.json"), &snapshot)?;
        self.snapshot_at = Instant::now();
        Ok(())
    }
}

pub(crate) fn fail(error: io::Error) -> ! {
    // A dev run with silently missing evidence must never look successful.
    // Exit also works during unwinding, without a double-panic in span cleanup.
    eprintln!("dev trace recording failed: {error}");
    std::process::exit(74)
}

#[derive(Default)]
struct Fields(Map<String, Value>);
impl Visit for Fields {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), format!("{value:?}").into());
    }
}

struct Active {
    id: u64,
    started: Instant,
    fields: Fields,
    parent: Option<u64>,
}

/// Standard tracing layer: alternate subscribers can consume the same sites.
/// Records start, end, values, errors and causal parents without sampling.
pub struct TraceLayer {
    log: FileLog,
}
impl TraceLayer {
    pub fn new(log: FileLog) -> Self {
        Self { log }
    }
}

impl<S> Layer<S> for TraceLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let span = context.span(id).expect("registered span");
        let parent = span
            .parent()
            .and_then(|parent| parent.extensions().get::<Active>().map(|active| active.id));
        let record_id = NEXT_SPAN.fetch_add(1, Ordering::Relaxed);
        let site = self.log.site(attributes.metadata());
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        self.log
            .write(json!({"kind": "start", "span": record_id, "parent": parent,
            "site": site, "thread": format!("{:?}", std::thread::current().id()),
            "thread_name": std::thread::current().name(), "fields": fields.0}));
        span.extensions_mut().insert(Active {
            id: record_id,
            started: Instant::now(),
            fields,
            parent,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        if let Some(span) = context.span(id)
            && let Some(active) = span.extensions_mut().get_mut::<Active>()
        {
            values.record(&mut active.fields);
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let parent = context
            .event_span(event)
            .and_then(|span| span.extensions().get::<Active>().map(|active| active.id));
        self.log
            .write(json!({"kind": "value", "span": parent, "fields": fields.0}));
        if fields.0.get("dev_flush").and_then(Value::as_bool) == Some(true) {
            self.log.flush().unwrap_or_else(|error| fail(error));
        }
    }

    fn on_follows_from(&self, id: &Id, follows: &Id, context: Context<'_, S>) {
        let record_id = |id| {
            context
                .span(id)
                .and_then(|span| span.extensions().get::<Active>().map(|active| active.id))
        };
        self.log
            .write(json!({"kind": "link", "span": record_id(id), "follows": record_id(follows)}));
    }

    fn on_close(&self, id: Id, context: Context<'_, S>) {
        let span = context.span(&id).expect("closing registered span");
        let extensions = span.extensions();
        let active = extensions.get::<Active>().expect("operation start");
        self.log.write(json!({"kind": "end", "span": active.id,
            "elapsed_ns": active.started.elapsed().as_nanos(), "fields": active.fields.0}));
        if active.parent.is_none() {
            self.log.flush().unwrap_or_else(|error| fail(error));
        }
    }
}
