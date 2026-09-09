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
    catalog::TableName,
    common::{Result, Row},
    parallel::QueryContext,
};

pub type RowId = u64;

#[derive(Debug, Clone, Copy)]
pub struct StorageCapabilities {
    pub mutable: bool,
    pub positional_fetch: bool,
    pub key_lookup: bool,
}

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
        rows: Vec<(RowId, Row)>,
        context: &QueryContext,
    ) -> Result<usize>;
    fn delete(&mut self, table: &TableName, ids: &[RowId], context: &QueryContext)
    -> Result<usize>;
}
