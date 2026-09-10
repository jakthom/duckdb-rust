//! Recovery reconstructs committed state without publishing files or invoking SQL.
use crate::{
    catalog::{Catalog, TableDefinition, TableName},
    common::{Error, Result, Row, Value},
    parallel::QueryContext,
};

use super::{
    RowId, TableStorage,
    format::{FormatId, SnapshotFormat},
    table::Snapshot,
};

pub struct RecoveryInput {
    pub checkpoint: Vec<u8>,
    pub log: Vec<u8>,
}

/// A fully validated logical result and a publication plan. Preparation is pure
/// and must finish before the storage adapter changes any durable object.
pub struct PreparedRecovery {
    pub snapshot: Snapshot,
    /// Complete mapping from replayed physical IDs to the published checkpoint.
    pub layout: super::layout::CheckpointLayout,
    /// Exact leased input this plan applies to. The publisher compares it under
    /// its publication lock and rejects stale plans before changing either file.
    pub basis: RecoveryInput,
    pub publication: RecoveryPublication,
}

/// Protocol shared with the file publisher. Replace first publishes and syncs
/// the bridge log, then the checkpoint, then retires the log. The bridge MUST
/// recover the same committed state with either old or new checkpoint, including
/// after a restart. It must not mark the old checkpoint as already published.
/// RetireLog asserts that the current checkpoint already contains every commit;
/// the publisher syncs that checkpoint and its directory before log retirement.
/// Byte buffers are owned and immutable during publication. No SQL or new user
/// transaction is committed by either operation.
pub enum RecoveryPublication {
    Replace {
        checkpoint: Vec<u8>,
        bridge_log: Vec<u8>,
    },
    RetireLog,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Stateless, concurrently callable recovery adapter for one checkpoint family.
/// Inputs are owned; success returns independent, validated committed state.
/// No file writes, external effects or partially recovered state may escape.
/// Unsupported versions/records return Unsupported; invalid committed bytes or
/// state return Corrupt. A supported incomplete tail excludes its transaction.
/// Implementations check cancellation and row limits, bound serialized input,
/// and document additional limits. Filesystem calls and checkpoint decoding
/// currently have no interruptible/global byte-budget contract.
pub trait Recovery: Send + Sync {
    fn name(&self) -> &'static str;
    fn format_id(&self) -> FormatId;
    /// Whether this adapter prepares writable recovery publications.
    fn supports_preparation(&self) -> bool {
        false
    }
    fn recover(
        &self,
        input: RecoveryInput,
        format: &dyn SnapshotFormat,
        context: &QueryContext,
    ) -> Result<Snapshot>;
    fn prepare(
        &self,
        _input: RecoveryInput,
        _format: &dyn SnapshotFormat,
        _context: &QueryContext,
    ) -> Result<PreparedRecovery> {
        Err(Error::Unsupported("writable recovery preparation".into()))
    }
}

/// Logical mutations in durable order within a verified committed transaction.
/// Row IDs belong to the restored table. Inserts allocate after its high-water
/// mark, including deleted slots. Column updates preserve other columns.
#[derive(Clone, Debug)]
pub enum RecoveredChange {
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
    Delete {
        table: TableName,
        ids: Vec<RowId>,
    },
    Update {
        table: TableName,
        column: usize,
        values: Vec<(RowId, Value)>,
    },
    /// Native logs can serialize values and validity separately. Validity is
    /// applied after value updates; making a NULL valid requires a value.
    Validity {
        table: TableName,
        column: usize,
        values: Vec<(RowId, bool)>,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Recovery visibility boundary. Applying a transaction is atomic on error or
/// cancellation. Constraint/index validation occurs at this boundary, allowing
/// temporary duplicate keys between records. No durability publication occurs.
pub trait RecoveryTarget: Catalog + TableStorage {
    fn apply_committed(
        &mut self,
        changes: &[RecoveredChange],
        context: &QueryContext,
    ) -> Result<()>;
}
