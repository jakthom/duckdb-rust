use super::*;

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Schema,
    pub rows: RowCollection,
    pub affected_rows: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg(feature = "dev")]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl QueryResult {
    pub(super) fn trace_output(&self) {
        let preview = self.rows.iter().take(20).collect::<Vec<_>>();
        duckdb_dev::statement::output(&serde_json::json!({
            "columns": trace_columns(&self.columns), "row_count": self.rows.len(),
            "affected_rows": self.affected_rows, "preview": preview,
            "omitted_rows": self.rows.len().saturating_sub(preview.len()),
            "encoding": "engine typed values; floating values preserve their bits",
        }));
    }
}

#[cfg(feature = "dev")]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl QuerySummary {
    pub(super) fn trace_output(&self) {
        duckdb_dev::statement::output(&serde_json::json!({
            "columns": trace_columns(&self.columns),
            "rows_delivered": self.execution.rows_delivered,
            "stopped_early": self.execution.stopped_early,
            "delivery": "batches consumed by caller",
        }));
    }
}

#[cfg(feature = "dev")]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn trace_columns(columns: &Schema) -> Vec<serde_json::Value> {
    columns
        .iter()
        .map(|column| {
            serde_json::json!({
                "name": column.name, "qualifier": column.qualifier, "data_type": column.data_type,
            })
        })
        .collect()
}
