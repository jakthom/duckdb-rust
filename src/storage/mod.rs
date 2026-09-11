pub mod checkpoint;
pub mod compression;
pub mod duckdb;
pub mod filesystem;
pub mod format;
pub mod layout;
pub mod log;
pub mod logged;
pub mod recovery;
pub mod scan;
pub mod table;

use crate::{
    catalog::{TableDefinition, TableName},
    common::{Error, Result, Row},
    parallel::QueryContext,
};
use std::collections::HashSet;

pub type RowId = u64;

/// Physical behavior selected from the assigned columns at bind time. Regular
/// updates retain their physical slot; indexed or unsupported nested-column
/// updates relocate the row through DELETE + INSERT while retaining its logical
/// identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMode {
    Regular,
    DeleteInsert,
}

/// The columns named by the UPDATE statement and their required physical mode.
/// This travels with the mutation so storage and durability adapters never try
/// to infer intent from whether the resulting values happen to differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateMetadata {
    pub columns: Vec<usize>,
    pub mode: UpdateMode,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl UpdateMetadata {
    pub fn for_table(definition: &TableDefinition, columns: Vec<usize>) -> Result<Self> {
        if columns.is_empty() {
            return Err(Error::Bind("UPDATE requires an assigned column".into()));
        }
        let mut seen = HashSet::new();
        for &column in &columns {
            if column >= definition.columns.len() || !seen.insert(column) {
                return Err(Error::Bind("invalid UPDATE column identity".into()));
            }
        }
        let indexed = definition
            .unique_keys
            .iter()
            .any(|key| key.columns.iter().any(|column| seen.contains(column)));
        let unsupported = columns.iter().any(|&column| {
            !definition.columns[column]
                .data_type
                .supports_regular_update()
        });
        Ok(Self {
            columns,
            mode: if indexed || unsupported {
                UpdateMode::DeleteInsert
            } else {
                UpdateMode::Regular
            },
        })
    }

    pub fn validate_for(&self, definition: &TableDefinition) -> Result<()> {
        let expected = Self::for_table(definition, self.columns.clone())?;
        if *self != expected {
            return Err(Error::Bind(
                "UPDATE physical mode differs from assigned columns".into(),
            ));
        }
        Ok(())
    }
}

/// Keep the first physical encounter position for each row while applying its
/// final replacement. Plain UPDATE supplies unique IDs; this also keeps direct
/// storage adapters from re-sorting defensive duplicate input by logical ID.
pub(crate) fn normalize_update_rows(rows: Vec<(RowId, Row)>) -> Vec<(RowId, Row)> {
    let mut positions = std::collections::HashMap::new();
    let mut normalized = Vec::with_capacity(rows.len());
    for (id, row) in rows {
        if let Some(&position) = positions.get(&id) {
            normalized[position] = (id, row);
        } else {
            positions.insert(id, normalized.len());
            normalized.push((id, row));
        }
    }
    normalized
}

#[derive(Debug, Clone, Copy)]
pub struct StorageCapabilities {
    pub mutable: bool,
    pub positional_fetch: bool,
    pub key_lookup: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Stable row identities belong to a table in a transaction; deleted IDs are not
/// reused. Fetch preserves request order and duplicates, including missing IDs.
/// Key lookup uses only an advertised index, returning ascending distinct IDs;
/// an unavailable index returns Unsupported and never hides a sequential scan.
/// All access observes transaction visibility and cooperative cancellation.
pub trait TableStorage: Send {
    fn capabilities(&self) -> StorageCapabilities;
    /// Exact visible cardinality from this statement's snapshot, including its
    /// own writes. None means unavailable. This optional metadata operation must
    /// not scan rows or retain state across snapshots; it has no external effects.
    fn row_count(&self, _table: &TableName) -> Result<Option<usize>> {
        Ok(None)
    }
    fn key_columns(&self, table: &TableName) -> Result<Vec<Vec<usize>>>;
    fn open_scan(&self, table: &TableName) -> Result<Box<dyn scan::TableScan + '_>>;
    /// Explicit collection is for callers that need all rows simultaneously.
    fn scan(&self, table: &TableName, context: &QueryContext) -> Result<Vec<(RowId, Row)>> {
        let mut scan = self.open_scan(table)?;
        let mut rows = Vec::new();
        while let Some(batch) = scan::next_batch(scan.as_mut(), context.batch_size(), context)? {
            context.check_rows(rows.len().saturating_add(batch.len()))?;
            rows.extend(batch.rows());
        }
        Ok(rows)
    }
    fn fetch(
        &self,
        table: &TableName,
        ids: &[RowId],
        context: &QueryContext,
    ) -> Result<Vec<Option<Row>>>;
    fn lookup(
        &self,
        table: &TableName,
        columns: &[usize],
        key: &Row,
        context: &QueryContext,
    ) -> Result<Vec<(RowId, Row)>>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait TableStorageMut: TableStorage {
    fn insert(
        &mut self,
        table: &TableName,
        rows: Vec<Row>,
        context: &QueryContext,
    ) -> Result<usize>;
    fn update(
        &mut self,
        table: &TableName,
        metadata: &UpdateMetadata,
        rows: Vec<(RowId, Row)>,
        context: &QueryContext,
    ) -> Result<usize>;
    fn delete(&mut self, table: &TableName, ids: &[RowId], context: &QueryContext)
    -> Result<usize>;
}
