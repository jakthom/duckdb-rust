use super::SettingsSnapshot;
use crate::{Error, Result, Value, main::QueryResult, parallel::QueryContext};
use serde::Serialize;
use std::time::Duration;

/// Bounded statement profile emitted by the current Rust execution surface.
/// Fields describe work this engine actually measured; absent native metrics
/// are not synthesized from estimates.
#[derive(Debug, Serialize)]
pub(in crate::main) struct QueryProfile {
    latency_seconds: f64,
    rows_returned: usize,
    verification_enabled: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl QueryProfile {
    pub(in crate::main) fn new(
        elapsed: Duration,
        rows_returned: usize,
        verification_enabled: bool,
    ) -> Self {
        Self {
            latency_seconds: elapsed.as_secs_f64(),
            rows_returned,
            verification_enabled,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::main) fn verify_query_results(
    original: &QueryResult,
    alternate: &QueryResult,
) -> Result<()> {
    if original.columns.len() != alternate.columns.len()
        || original
            .columns
            .iter()
            .zip(&alternate.columns)
            .any(|(left, right)| left.data_type != right.data_type)
    {
        return Err(Error::Internal(
            "unoptimized statement differs from original result: column types differ".into(),
        ));
    }
    if original.rows.len() != alternate.rows.len() {
        return Err(Error::Internal(format!(
            "unoptimized statement differs from original result: {} rows versus {}",
            original.rows.len(),
            alternate.rows.len()
        )));
    }
    for (row, (left, right)) in original.rows.iter().zip(&alternate.rows).enumerate() {
        if left.len() != right.len() {
            return Err(Error::Internal(format!(
                "unoptimized statement differs from original result at row {row}: widths differ"
            )));
        }
        for (column, (left, right)) in left.iter().zip(right).enumerate() {
            if !same_representation(left, right) {
                return Err(Error::Internal(format!(
                    "unoptimized statement differs from original result at row {row}, column {column}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn emit_profile(
    settings: &SettingsSnapshot,
    profile: &QueryProfile,
    query: &QueryContext,
) -> Result<()> {
    let Some(format) = settings.profiling_format(query)? else {
        return Ok(());
    };
    if format == "no_output" {
        return Ok(());
    }
    let rendered = match format {
        "json" => serde_json::to_string(profile)
            .map_err(|error| Error::Internal(format!("cannot render query profile: {error}")))?,
        "default" | "text" | "query_tree" | "query_tree_optimizer" => format!(
            "Query Profile\nLatency: {:.9}s\nRows returned: {}\nVerification enabled: {}\n",
            profile.latency_seconds, profile.rows_returned, profile.verification_enabled
        ),
        other => {
            return Err(Error::Unsupported(format!(
                "profiling renderer {other} is not implemented"
            )));
        }
    };
    let output = match settings.get("profiling_output", query)? {
        Value::Varchar(output) => output,
        _ => {
            return Err(Error::Internal(
                "invalid profiling_output setting type".into(),
            ));
        }
    };
    if output.is_empty() {
        eprintln!("{rendered}");
        return Ok(());
    }
    validate_extension(format, output)?;
    std::fs::write(output, rendered)?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_extension(format: &str, output: &str) -> Result<()> {
    let extension = std::path::Path::new(output)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let expected = match format {
        "json" => "json",
        _ => "txt",
    };
    if extension == expected || extension == "txt" {
        Ok(())
    } else {
        Err(Error::Parse(format!(
            "Profiler file type ({extension}) must either have the same file extension as the profiling output type ({expected}), or be a '.txt' file."
        )))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn same_representation(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Float(left), Value::Float(right)) => left.to_bits() == right.to_bits(),
        (Value::Double(left), Value::Double(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DataType, common::RowCollection, planner::Field};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn result(value: Value) -> QueryResult {
        QueryResult {
            columns: vec![Field::new("value", DataType::BigInt)],
            rows: RowCollection::from_rows(1, vec![vec![value]]).unwrap(),
            affected_rows: 0,
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn verification_rejects_a_perturbed_alternate_execution() {
        let original = result(Value::Integer(1));
        let perturbed = result(Value::Integer(2));
        assert!(matches!(
            verify_query_results(&original, &perturbed),
            Err(Error::Internal(message)) if message.contains("differs from original result")
        ));
    }
}
