use super::*;

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Schema,
    pub rows: RowCollection,
    pub affected_rows: usize,
}

impl QueryResult {
    pub(super) fn command(count: usize) -> Self {
        Self {
            columns: Vec::new(),
            rows: RowCollection::new(0),
            affected_rows: count,
        }
    }
}

/// Metadata and completion for a query consumed through the batch API.
#[derive(Debug)]
pub struct QuerySummary {
    pub columns: crate::planner::Schema,
    pub execution: crate::execution::ExecutionOutcome,
}
