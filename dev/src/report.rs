//! Streaming trace summaries. Memory scales with active spans and operation sites.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{self, BufRead, BufReader},
    path::Path,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Summary {
    pub files: usize,
    pub records: u64,
    pub completed: u64,
    pub errors: u64,
    pub panics: u64,
    pub first_error: Option<Value>,
    pub incomplete: Vec<Value>,
    pub operations: BTreeMap<String, Operation>,
}

impl Summary {
    fn merge(&mut self, other: Self, filter: Option<&str>) {
        self.files += other.files;
        self.records += other.records;
        self.completed += other.completed;
        self.errors += other.errors;
        self.panics += other.panics;
        self.first_error = self.first_error.take().or(other.first_error);
        self.incomplete.extend(other.incomplete);
        for (name, operation) in other.operations {
            if filter.is_some_and(|filter| !name.contains(filter)) {
                continue;
            }
            let combined = self.operations.entry(name).or_default();
            combined.calls += operation.calls;
            combined.total_ns += operation.total_ns;
            combined.max_ns = combined.max_ns.max(operation.max_ns);
            combined.errors += operation.errors;
            combined.panics += operation.panics;
            combined.source = operation.source;
            combined.first_error = combined.first_error.take().or(operation.first_error);
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Operation {
    pub calls: u64,
    pub errors: u64,
    pub panics: u64,
    pub total_ns: u128,
    pub max_ns: u128,
    pub source: Value,
    pub first_error: Option<Value>,
}

fn identity(record: &Value, key: &str) -> io::Result<u64> {
    record[key]
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or_else(|| io::Error::other(format!("invalid trace {key} identity")))
}

pub(crate) fn modified_ns(metadata: &std::fs::Metadata) -> io::Result<u128> {
    Ok(metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos())
}

/// Incremental summaries keep agent feedback proportional to sites and active
/// operations. The raw stream remains the authoritative detailed evidence.
#[derive(Default)]
pub(crate) struct Accumulator {
    summary: Summary,
    sites: HashMap<u64, (String, Value)>,
    active: HashMap<u64, Value>,
}

impl Accumulator {
    pub fn push(&mut self, path: &Path, record: &Value) -> io::Result<()> {
        if record["seq"].as_u64() != Some(self.summary.records + 1) {
            return Err(io::Error::other(format!(
                "trace sequence gap in {}",
                path.display()
            )));
        }
        self.summary.records += 1;
        match record["kind"].as_str() {
            Some("process") => self.summary.files += 1,
            Some("site") => {
                let operation = record["operation"].as_str().unwrap_or_default();
                let mut name = format!(
                    "{}::{operation}",
                    record["module"].as_str().unwrap_or_default()
                );
                if operation.starts_with("call::") {
                    name.push_str(&format!(
                        " @ {}:{}",
                        record["file"].as_str().unwrap_or_default(),
                        record["line"]
                    ));
                }
                if self
                    .sites
                    .insert(identity(record, "site")?, (name, record.clone()))
                    .is_some()
                {
                    return Err(io::Error::other("duplicate trace site"));
                }
            }
            Some("start") => {
                if !self.sites.contains_key(&identity(record, "site")?) {
                    return Err(io::Error::other("trace start references an unknown site"));
                }
                if self
                    .active
                    .insert(identity(record, "span")?, record.clone())
                    .is_some()
                {
                    return Err(io::Error::other("duplicate active trace identity"));
                }
            }
            Some("end") => {
                let id = identity(record, "span")?;
                let start = self
                    .active
                    .remove(&id)
                    .ok_or_else(|| io::Error::other("trace end without start"))?;
                let (name, site) = &self.sites[&start["site"].as_u64().unwrap_or_default()];
                let outcome = record["fields"]["outcome"].as_str().unwrap_or_default();
                let error = u64::from(outcome == "error");
                let panic = u64::from(outcome == "panic");
                let elapsed = record["elapsed_ns"]
                    .as_u64()
                    .ok_or_else(|| io::Error::other("trace end lacks a valid elapsed_ns value"))?
                    as u128;
                self.summary.completed += 1;
                self.summary.errors += error;
                self.summary.panics += panic;
                let operation = self.summary.operations.entry(name.clone()).or_default();
                operation.calls += 1;
                operation.total_ns += elapsed;
                operation.max_ns = operation.max_ns.max(elapsed);
                operation.errors += error;
                operation.panics += panic;
                if operation.calls == 1 {
                    operation.source =
                        serde_json::json!({"file": site["file"], "line": site["line"]});
                }
                if error + panic > 0 {
                    let details = serde_json::json!({"log": path, "span": id, "operation": name,
                        "file": site["file"], "line": site["line"], "fields": record["fields"]});
                    if operation.first_error.is_none() {
                        operation.first_error = Some(details.clone());
                    }
                    if self.summary.first_error.is_none() {
                        self.summary.first_error = Some(details);
                    }
                }
            }
            Some("value" | "link" | "statement" | "statement_end") => {}
            _ => return Err(io::Error::other("unknown trace record kind")),
        }
        Ok(())
    }

    pub fn snapshot(&self, path: &Path) -> Summary {
        let mut summary = self.summary.clone();
        summary.incomplete = self
            .active
            .values()
            .map(|record| {
                let mut record = record.clone();
                record["log"] = serde_json::json!(path);
                record["site_info"] = self.sites[&record["site"].as_u64().unwrap_or_default()]
                    .1
                    .clone();
                record
            })
            .collect();
        summary
    }
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Snapshot {
    pub bytes: u64,
    pub modified_ns: u128,
    pub summary: Summary,
}

pub fn trace_files(directory: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            files.extend(trace_files(&path)?);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Read small atomic snapshots, never rescan the raw logs in the feedback path.
/// Live snapshots identify the prefix they cover, including any outstanding bytes.
pub fn cached(directory: &Path, filter: Option<&str>) -> io::Result<(Summary, u64)> {
    let mut total = Summary::default();
    let mut pending_bytes = 0;
    for path in trace_files(directory)? {
        let snapshot: Snapshot =
            serde_json::from_slice(&std::fs::read(path.with_extension("summary.json"))?)?;
        let metadata = std::fs::metadata(&path)?;
        let bytes = metadata.len();
        if bytes == snapshot.bytes && modified_ns(&metadata)? != snapshot.modified_ns {
            return Err(io::Error::other(
                "trace was modified after its summary snapshot",
            ));
        }
        if snapshot.bytes > bytes {
            return Err(io::Error::other(
                "trace is shorter than its summary snapshot",
            ));
        }
        pending_bytes += bytes - snapshot.bytes;
        total.merge(snapshot.summary, filter);
    }
    Ok((total, pending_bytes))
}

pub fn summarize(directory: &Path, filter: Option<&str>) -> io::Result<Summary> {
    let mut summary = Summary::default();
    for path in trace_files(directory)? {
        let mut accumulator = Accumulator::default();
        for (line, bytes) in BufReader::new(File::open(&path)?).split(b'\n').enumerate() {
            let bytes = bytes?;
            if bytes.is_empty() {
                continue;
            }
            let record: Value = serde_json::from_slice(&bytes).map_err(|error| {
                io::Error::other(format!("{} line {}: {error}", path.display(), line + 1))
            })?;
            accumulator.push(&path, &record)?;
        }
        summary.merge(accumulator.snapshot(&path), filter);
    }
    Ok(summary)
}

/// A bounded first view; detailed records remain available by operation and span.
pub fn overview(summary: &Summary) -> Value {
    let mut operations = summary.operations.iter().collect::<Vec<_>>();
    operations.sort_by_key(|(_, operation)| std::cmp::Reverse(operation.total_ns));
    let slow = operations
        .iter()
        .take(20)
        .map(|(name, operation)| {
            serde_json::json!({
                "operation": name, "calls": operation.calls, "total_ns": operation.total_ns,
                "max_ns": operation.max_ns, "source": operation.source,
            })
        })
        .collect::<Vec<_>>();
    operations.sort_by_key(|(_, operation)| std::cmp::Reverse(operation.calls));
    let frequent = operations
        .iter()
        .take(10)
        .map(|(name, operation)| {
            serde_json::json!({
                "operation": name, "calls": operation.calls,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({"trace_files": summary.files, "records": summary.records,
        "completed": summary.completed, "error_returns": summary.errors, "panics": summary.panics,
        "first_error": summary.first_error, "incomplete": summary.incomplete,
        "slowest_inclusive": slow, "most_frequent": frequent,
        "timing": "instrumented inclusive wall time; nested durations overlap; not performance acceptance"})
}

/// Print one operation and its descendants, including values and source metadata.
pub fn span(path: &Path, requested: u64, output: &mut impl io::Write) -> io::Result<()> {
    let mut sites = HashMap::new();
    let mut selected = std::collections::HashSet::new();
    let mut found = false;
    for line in BufReader::new(File::open(path)?).lines() {
        let mut record: Value = serde_json::from_str(&line?).map_err(io::Error::other)?;
        let id = record["span"].as_u64();
        match record["kind"].as_str() {
            Some("site") => {
                sites.insert(record["site"].as_u64(), record);
                continue;
            }
            Some("start")
                if id == Some(requested)
                    || record["parent"]
                        .as_u64()
                        .is_some_and(|parent| selected.contains(&parent)) =>
            {
                let id = id.ok_or_else(|| io::Error::other("span start lacks identity"))?;
                selected.insert(id);
                found = true;
                record["site_info"] = sites
                    .get(&record["site"].as_u64())
                    .cloned()
                    .unwrap_or_default();
            }
            _ if id.is_none_or(|id| !selected.contains(&id)) => continue,
            _ => {}
        }
        serde_json::to_writer(&mut *output, &record)?;
        output.write_all(b"\n")?;
        if record["kind"] == "end" {
            selected.remove(&id.unwrap_or_default());
            if selected.is_empty() {
                break;
            }
        }
    }
    if !found {
        return Err(io::Error::other("span not found in this process log"));
    }
    Ok(())
}
