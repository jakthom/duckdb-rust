//! Incremental access retains the statement's row identities and visibility.
use super::RowId;
use crate::{
    common::{Error, Result, Row},
    parallel::QueryContext,
};

/// A cursor is local to its driver. Each call returns at most `max_rows` owned
/// rows, with no replay. None is permanent exhaustion; Some must be nonempty.
/// Dropping a cursor releases its retained snapshot and performs no mutation.
pub trait TableScan {
    fn next(
        &mut self,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<Vec<(RowId, Row)>>>;
}

/// Engine consumers validate foreign cursor output before interpreting it.
pub fn next_batch(
    scan: &mut dyn TableScan,
    max_rows: usize,
    context: &QueryContext,
) -> Result<Option<Vec<(RowId, Row)>>> {
    let max_rows = context.batch_demand(max_rows)?;
    let batch = scan.next(max_rows, context)?;
    if let Some(batch) = &batch
        && (batch.is_empty() || batch.len() > max_rows)
    {
        return Err(Error::Internal(
            "table scan violated batch cardinality".into(),
        ));
    }
    context.check()?;
    Ok(batch)
}

pub(crate) struct SnapshotScan<'a> {
    pub rows: std::collections::btree_map::Iter<'a, RowId, Row>,
    pub finished: bool,
}

impl TableScan for SnapshotScan<'_> {
    fn next(
        &mut self,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<Vec<(RowId, Row)>>> {
        if self.finished {
            return Ok(None);
        }
        let result = (|| {
            let max_rows = context.batch_demand(max_rows)?;
            let mut rows = Vec::new();
            for _ in 0..max_rows {
                context.check()?;
                let Some((&id, row)) = self.rows.next() else {
                    break;
                };
                rows.push((id, row.clone()));
            }
            Ok((!rows.is_empty()).then_some(rows))
        })();
        if !matches!(result, Ok(Some(_))) {
            self.finished = true;
        }
        result
    }
}
