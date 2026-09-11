//! Transaction change capture and replaceable, effect-free log encoding.
use super::{RowId, format::FormatId, table::Snapshot};
use crate::{
    catalog::{TableDefinition, TableName},
    common::{Result, Row},
    parallel::QueryContext,
};

/// Successful mutations in transaction order. Journals from failed or rolled
/// back transactions must never be published. Row IDs refer to the table, including
/// its own inserts; update rows contain all columns. Delete IDs are unique and
/// visible before deletion. No-op catalog operations are omitted.
#[derive(Clone, Debug)]
pub enum TransactionChange {
    CreateSchema(String),
    DropSchema(String),
    CreateTable(TableDefinition),
    DropTable(TableName),
    AlterTable {
        table: TableName,
        alteration: crate::catalog::TableAlteration,
    },
    Insert {
        table: TableName,
        rows: Vec<Row>,
    },
    Update {
        table: TableName,
        rows: Vec<(RowId, Row)>,
    },
    Delete {
        table: TableName,
        ids: Vec<RowId>,
    },
}

/// A validated candidate snapshot and its optional ordered change journal.
/// Journaling is requested by durability at transaction creation. A publication
/// must finish durably before the snapshot becomes visible. Adapters must not
/// retain these borrowed inputs or publish only a prefix of the transaction.
pub struct Commit<'a> {
    /// Current acknowledged state under the transaction publication lock.
    pub before: &'a Snapshot,
    pub snapshot: &'a Snapshot,
    pub changes: Option<&'a [TransactionChange]>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Pure, concurrently callable encoding factory for one checkpoint family.
/// The initial snapshot must have the checkpoint's physical row identities.
/// A session owns mapping/catalog state and never accesses filesystem or SQL.
/// Unsupported types fail before durable changes; resource/cancellation errors
/// preserve the old session. Encoding limits are adapter-specific.
pub trait TransactionLog: Send + Sync {
    fn name(&self) -> &'static str;
    fn format_id(&self) -> FormatId;
    fn start(&self, snapshot: &Snapshot, context: &QueryContext) -> Result<LogStart>;
}

pub struct LogStart {
    pub header: Vec<u8>,
    pub session: Box<dyn LogSession>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// A serial encoder state. Preparing a complete transaction performs no effects
/// and leaves this state unchanged. The caller installs `next` only after the
/// appended bytes are durable. Dropping a preparation discards it entirely.
pub trait LogSession: Send {
    fn prepare(&self, changes: &[TransactionChange], context: &QueryContext) -> Result<LogAppend>;
    /// Rebase onto a validated successor without changing live logical IDs.
    /// Preparation is pure; install the new header/session only after successful
    /// checkpoint publication. Invalid mappings fail before external effects.
    fn rebase(&self, checkpoint: LogCheckpoint<'_>, context: &QueryContext) -> Result<LogStart>;
}

pub struct LogCheckpoint<'a> {
    /// Actual selected checkpoint format, including its exact content rules.
    pub format: &'a dyn super::format::SnapshotFormat,
    pub logical: &'a Snapshot,
    pub physical: &'a Snapshot,
    pub bytes: &'a [u8],
    /// Replayed old physical IDs to new checkpoint IDs.
    pub layout: &'a super::layout::CheckpointLayout,
}

pub struct LogAppend {
    pub bytes: Vec<u8>,
    pub next: Box<dyn LogSession>,
}
