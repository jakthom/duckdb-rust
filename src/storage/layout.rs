//! Explicit physical row identity changes produced by checkpoint encoding.
use super::{RowId, table::Snapshot};
use crate::{
    catalog::{Catalog, TableName},
    common::Result,
};
use std::collections::BTreeMap;

/// An encoded successor and its complete source-to-destination row mapping.
/// Every live source row appears exactly once, each destination is distinct,
/// and values/catalog semantics are preserved. Maps belong to this image only.
pub struct CheckpointImage {
    pub bytes: Vec<u8>,
    pub layout: CheckpointLayout,
}

#[derive(Clone, Debug, Default)]
pub struct CheckpointLayout {
    pub tables: BTreeMap<TableName, TableLayout>,
}

#[derive(Clone, Debug)]
pub struct TableLayout {
    pub rows: BTreeMap<RowId, RowId>,
    pub next_row_id: RowId,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CheckpointLayout {
    pub fn identity(snapshot: &Snapshot) -> Result<Self> {
        Self::from_snapshot(snapshot, false)
    }
    /// Layout of the native writer's ascending source-ID, compacted row stream.
    pub(crate) fn compacted(snapshot: &Snapshot) -> Result<Self> {
        Self::from_snapshot(snapshot, true)
    }
    fn from_snapshot(snapshot: &Snapshot, compact: bool) -> Result<Self> {
        let mut tables = BTreeMap::new();
        for table in snapshot.tables()? {
            let ids = snapshot.row_ids(&table.name)?;
            let next_row_id = if compact {
                ids.len() as u64
            } else {
                snapshot.next_row_id(&table.name)?
            };
            let rows = ids
                .into_iter()
                .enumerate()
                .map(|(ordinal, id)| (id, if compact { ordinal as u64 } else { id }))
                .collect();
            tables.insert(table.name, TableLayout { rows, next_row_id });
        }
        Ok(Self { tables })
    }
}
