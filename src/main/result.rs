use super::*;

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Schema,
    pub rows: Vec<Row>,
    pub affected_rows: usize,
}

impl QueryResult {
    pub(super) fn command(count: usize) -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            affected_rows: count,
        }
    }
}

impl From<DataSet> for QueryResult {
    fn from(data: DataSet) -> Self {
        Self {
            columns: data.schema,
            rows: data.rows,
            affected_rows: 0,
        }
    }
}

/// Metadata and completion for a query consumed through the batch API.
#[derive(Debug)]
pub struct QuerySummary {
    pub columns: crate::planner::Schema,
    pub execution: crate::execution::ExecutionOutcome,
}
