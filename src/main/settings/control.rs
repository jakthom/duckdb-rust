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
    let mut ordered = true;
    'rows: for (row, (left, right)) in original.rows.iter().zip(&alternate.rows).enumerate() {
        if left.len() != right.len() {
            return Err(Error::Internal(format!(
                "unoptimized statement differs from original result at row {row}: widths differ"
            )));
        }
        for (left, right) in left.iter().zip(right) {
            if !same_verification_value(left, right) {
                ordered = false;
                break 'rows;
            }
        }
    }
    if ordered {
        return Ok(());
    }
    // Query results without an explicit ordering are equivalent as per-column
    // multisets. Keep the ordered comparison above as the common fast path,
    // then consume one matching default-equality value from the alternate column for
    // every original value. Consuming matches preserves duplicate counts.
    for column in 0..original.columns.len() {
        let mut matched = vec![false; alternate.rows.len()];
        for left in original.rows.iter().map(|row| &row[column]) {
            let Some((index, _)) = alternate
                .rows
                .iter()
                .map(|row| &row[column])
                .enumerate()
                .find(|(index, right)| !matched[*index] && same_verification_value(left, right))
            else {
                return Err(Error::Internal(format!(
                    "unoptimized statement differs from original result at column {column}"
                )));
            };
            matched[index] = true;
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
fn same_verification_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Float(left), Value::Float(right)) => approximate_float(*left, *right),
        (Value::Double(left), Value::Double(right)) => approximate_double(*left, *right),
        (Value::Varchar(left), Value::Varchar(right)) => {
            sanitize_verification_varchar(left) == sanitize_verification_varchar(right)
        }
        _ => left == right,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn approximate_float(left: f32, right: f32) -> bool {
    if left.is_nan() && right.is_nan() {
        return true;
    }
    if !left.is_finite() || !right.is_finite() {
        return left == right;
    }
    let epsilon = (f64::from(left).abs() * 0.01 + 0.000_000_01) as f32;
    (left - right).abs() <= epsilon
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn approximate_double(left: f64, right: f64) -> bool {
    if left.is_nan() && right.is_nan() {
        return true;
    }
    if !left.is_finite() || !right.is_finite() {
        return left == right;
    }
    (left - right).abs() <= left.abs() * 0.01 + 0.000_000_01
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sanitize_verification_varchar(value: &str) -> String {
    value
        .trim_end_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r'])
        .replace('\0', "\\0")
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn verification_accepts_reordered_rows_but_not_different_multisets() {
        let original = QueryResult {
            columns: vec![Field::new("value", DataType::BigInt)],
            rows: RowCollection::from_rows(
                1,
                vec![vec![Value::Integer(1)], vec![Value::Integer(2)]],
            )
            .unwrap(),
            affected_rows: 0,
        };
        let reordered = QueryResult {
            columns: original.columns.clone(),
            rows: RowCollection::from_rows(
                1,
                vec![vec![Value::Integer(2)], vec![Value::Integer(1)]],
            )
            .unwrap(),
            affected_rows: 0,
        };
        let duplicate = QueryResult {
            columns: original.columns.clone(),
            rows: RowCollection::from_rows(
                1,
                vec![vec![Value::Integer(2)], vec![Value::Integer(2)]],
            )
            .unwrap(),
            affected_rows: 0,
        };
        assert!(verify_query_results(&original, &reordered).is_ok());
        assert!(verify_query_results(&original, &duplicate).is_err());
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn verification_uses_release_default_value_equality() {
        fn single(data_type: DataType, value: Value) -> QueryResult {
            QueryResult {
                columns: vec![Field::new("value", data_type)],
                rows: RowCollection::from_rows(1, vec![vec![value]]).unwrap(),
                affected_rows: 0,
            }
        }

        for (left, right, accepted) in [
            (0.0_f64, -0.0_f64, true),
            (100.0, 100.5, true),
            (100.0, 102.0, false),
        ] {
            let original = single(DataType::Double, Value::Double(left));
            let alternate = single(DataType::Double, Value::Double(right));
            assert_eq!(
                verify_query_results(&original, &alternate).is_ok(),
                accepted
            );
        }
        let nan = single(DataType::Double, Value::Double(f64::NAN));
        assert!(verify_query_results(&nan, &nan).is_ok());
        let float_original = single(DataType::Float, Value::Float(100.0));
        let float_near = single(DataType::Float, Value::Float(100.5));
        let float_far = single(DataType::Float, Value::Float(102.0));
        assert!(verify_query_results(&float_original, &float_near).is_ok());
        assert!(verify_query_results(&float_original, &float_far).is_err());

        let original = single(DataType::Varchar, Value::Varchar("value\0".into()));
        let alternate = single(DataType::Varchar, Value::Varchar("value\\0 \t".into()));
        assert!(verify_query_results(&original, &alternate).is_ok());
    }
}
