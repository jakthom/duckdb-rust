//! Checksummed DuckDB WAL v2 recovery. No filesystem or SQL dependencies.
mod alter;
mod chunk;
mod publication;
mod records;
pub mod writer;

use super::binary::{Reader, checksum, corrupt, u64_at};
use crate::{
    common::{Error, Result},
    parallel::QueryContext,
    storage::{
        format::{DUCKDB_FORMAT, FormatId, SnapshotFormat},
        recovery::{PreparedRecovery, Recovery, RecoveryInput, RecoveryTarget},
        table::Snapshot,
    },
};

const MAX_BYTES: usize = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 1_000_000;

fn append_frame(payload: &[u8], bytes: &mut Vec<u8>) -> Result<()> {
    if bytes.len().saturating_add(payload.len()).saturating_add(16) > MAX_BYTES {
        return Err(Error::Resource("encoded WAL exceeds 512 MiB".into()));
    }
    bytes.extend((payload.len() as u64).to_le_bytes());
    bytes.extend(checksum(payload)?.to_le_bytes());
    bytes.extend(payload);
    Ok(())
}

/// Recovers committed catalog/tuple records in an unencrypted v2 log. Legacy
/// unframed logs, concurrent checkpoint logs, bulk block
/// appends and unimplemented catalog records are rejected. Publication is a
/// separate durability concern; this adapter performs no external effects.
pub struct DuckDbWalRecovery;

impl Recovery for DuckDbWalRecovery {
    fn name(&self) -> &'static str {
        "duckdb-wal-v2"
    }
    fn format_id(&self) -> FormatId {
        DUCKDB_FORMAT
    }
    fn supports_preparation(&self) -> bool {
        true
    }
    fn recover(
        &self,
        input: RecoveryInput,
        format: &dyn SnapshotFormat,
        context: &QueryContext,
    ) -> Result<Snapshot> {
        let Inspection {
            identity,
            header: _,
            scan,
        } = inspect(&input, format, context)?;
        let snapshot = format.decode(input.checkpoint, context.type_registry())?;
        replay(snapshot, &input.log, &scan, identity, context)
    }
    fn prepare(
        &self,
        input: RecoveryInput,
        format: &dyn SnapshotFormat,
        context: &QueryContext,
    ) -> Result<PreparedRecovery> {
        publication::prepare(input, format, context)
    }
}

struct Inspection {
    identity: super::CheckpointIdentity,
    header: Option<LogHeader>,
    scan: LogScan,
}

fn inspect(
    input: &RecoveryInput,
    format: &dyn SnapshotFormat,
    context: &QueryContext,
) -> Result<Inspection> {
    context.check()?;
    if format.format_id() != DUCKDB_FORMAT {
        return Err(Error::Unsupported("WAL/checkpoint format mismatch".into()));
    }
    if input.log.len() > MAX_BYTES || input.checkpoint.len() > MAX_BYTES {
        return Err(Error::Resource(
            "recovery input limits each file to 512 MiB".into(),
        ));
    }
    let identity = super::CheckpointIdentity::read(&input.checkpoint)?;
    let header = if input.log.is_empty() {
        None
    } else {
        Some(read_header(&input.log, &identity.identifier)?)
    };
    let scan = match header {
        Some(header) => scan_log(&input.log, header.end, context)?,
        None => LogScan::default(),
    };
    validate_generation(header, &scan, identity)?;
    Ok(Inspection {
        identity,
        header,
        scan,
    })
}

fn replay(
    mut snapshot: Snapshot,
    log: &[u8],
    scan: &LogScan,
    identity: super::CheckpointIdentity,
    context: &QueryContext,
) -> Result<Snapshot> {
    snapshot.validate_with_context(context)?;
    if scan.checkpoint == Some(identity.root) {
        return Ok(snapshot);
    }
    let mut changes = Vec::new();
    let mut state = records::RecordState::new(&snapshot)?;
    for frame in &scan.frames {
        context.check()?;
        let mut reader = Reader::new(log[frame.clone()].to_vec());
        reader.field(100)?;
        let kind = reader.unsigned()?;
        if kind == 100 {
            reader.end()?;
            snapshot
                .apply_committed(&changes, context)
                .map_err(recovery_error)?;
            changes.clear();
        } else if let Some(change) = state.read(kind, &mut reader, context)? {
            changes.push(change);
        }
        if !reader.finished() {
            return Err(corrupt("trailing bytes in WAL entry"));
        }
    }
    context.check()?;
    Ok(snapshot)
}

fn recovery_error(error: Error) -> Error {
    match error {
        Error::Unsupported(_) | Error::Resource(_) | Error::Interrupted => error,
        other => corrupt(format!("invalid committed WAL transaction: {other}")),
    }
}

#[derive(Clone, Copy)]
struct LogHeader {
    end: usize,
    iteration: Option<u64>,
}

fn read_header(log: &[u8], identifier: &[u8; 16]) -> Result<LogHeader> {
    // The version header is unframed and has a small, fixed schema. Limit its
    // copied prefix independently of the size of the remaining log.
    let mut reader = Reader::new(log[..log.len().min(256)].to_vec());
    reader.field(100)?;
    if reader.unsigned()? != 98 {
        return Err(Error::Unsupported("unframed DuckDB WAL v1".into()));
    }
    reader.field(101)?;
    let version = reader.unsigned()?;
    if version != 2 {
        return Err(Error::Unsupported(format!("DuckDB WAL version {version}")));
    }
    let has_identifier = reader.optional(102)?;
    if has_identifier {
        if reader.length()? != 16 {
            return Err(corrupt("WAL database identifier length"));
        }
        for expected in identifier {
            if reader.unsigned()? != u64::from(*expected) {
                return Err(corrupt("WAL does not match database identifier"));
            }
        }
    }
    let has_iteration = reader.optional(103)?;
    let iteration = if has_iteration {
        Some(reader.unsigned()?)
    } else {
        None
    };
    if has_identifier != has_iteration {
        return Err(corrupt("incomplete WAL database identity"));
    }
    reader.end()?;
    Ok(LogHeader {
        end: reader.position,
        iteration,
    })
}

fn validate_generation(
    header: Option<LogHeader>,
    scan: &LogScan,
    identity: super::CheckpointIdentity,
) -> Result<()> {
    if let Some(iteration) = header.and_then(|h| h.iteration)
        && iteration != identity.iteration
        && !(iteration.checked_add(1) == Some(identity.iteration)
            && scan.checkpoint == Some(identity.root))
    {
        return Err(Error::Unsupported(
            "WAL/checkpoint generation reconciliation".into(),
        ));
    }
    Ok(())
}

#[derive(Default)]
struct LogScan {
    frames: Vec<std::ops::Range<usize>>,
    checkpoint: Option<u64>,
}

/// Check every complete frame before replay. A checkpoint marker may be followed
/// only by its flush or a torn final frame. Checksum failures remain corruption.
fn scan_log(log: &[u8], mut position: usize, context: &QueryContext) -> Result<LogScan> {
    let mut frames = Vec::new();
    let mut committed = 0;
    let mut checkpoint = None;
    let mut checkpoint_flushed = false;
    let mut entries = 0;
    while position < log.len() {
        context.check()?;
        if checkpoint_flushed {
            return Err(corrupt("data after WAL checkpoint flush"));
        }
        if log.len() - position < 16 {
            break;
        }
        let size = u64_at(log, position)?;
        let stored = u64_at(log, position + 8)?;
        position += 16;
        if size > (log.len() - position) as u64 {
            break;
        }
        let end = position + size as usize;
        let payload = &log[position..end];
        let mut computed = 5381;
        for part in payload.chunks(64 * 1024) {
            context.check()?;
            computed ^= checksum(part)? ^ 5381;
        }
        if computed != stored {
            return Err(corrupt("WAL entry checksum mismatch"));
        }
        if payload.len() < 5
            || payload[..2] != 100u16.to_le_bytes()
            || payload[payload.len() - 2..] != [255, 255]
        {
            return Err(corrupt("invalid WAL entry envelope"));
        }
        let mut envelope = Reader::new(payload[..payload.len().min(16)].to_vec());
        envelope.field(100)?;
        let kind = envelope.unsigned()?;
        if kind > u64::from(u8::MAX) {
            return Err(corrupt("WAL record type out of range"));
        }
        if checkpoint.is_some() && kind != 100 {
            return Err(corrupt("data or repeated marker after WAL checkpoint"));
        }
        if entries >= MAX_ENTRIES {
            return Err(Error::Resource("WAL exceeds one million entries".into()));
        }
        entries += 1;
        if kind == 99 {
            let mut marker = Reader::new(payload.to_vec());
            marker.field(100)?;
            marker.unsigned()?;
            marker.field(101)?;
            let (root, offset) = marker.pointer()?;
            // Zero is the implicit root offset; the native metadata writer
            // records eight explicitly, after the chain-link word.
            if offset != 0 && offset != 8 {
                return Err(Error::Unsupported("WAL checkpoint root offset".into()));
            }
            if root == u64::MAX {
                return Err(corrupt("invalid WAL checkpoint root"));
            }
            marker.end()?;
            if !marker.finished() {
                return Err(corrupt("trailing WAL checkpoint fields"));
            }
            checkpoint = Some(root);
        } else {
            frames.push(position..end);
        }
        if kind == 100 {
            if envelope.end().is_err() || envelope.position != payload.len() {
                return Err(corrupt("invalid WAL commit marker"));
            }
            committed = frames.len();
            checkpoint_flushed = checkpoint.is_some();
        }
        position = end;
    }
    frames.truncate(committed);
    Ok(LogScan { frames, checkpoint })
}
