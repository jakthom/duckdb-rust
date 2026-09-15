//! SQLLogicTest result and error oracle.
//!
//! This is a dependency-free port of DuckDB's pinned
//! `test/sqlite/result_helper.cpp`.  The runner adapter deliberately supplies a
//! small, rendered result view: the SQLLogicTest signature remains a column
//! count, while the native logical type is used only for DuckDB's fallback
//! numeric/boolean comparison.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

const REGEX: &[u8] = b"<REGEX>:";
const NOT_REGEX: &[u8] = b"<!REGEX>:";
const FILE: &[u8] = b"<FILE>:";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SortMode {
    None,
    Rows,
    Values,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActualColumn<'a> {
    pub name: &'a str,
    pub logical_type: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ActualCell<'a> {
    Null,
    Text(&'a str),
    Bytes(&'a [u8]),
}

/// Cells are row-major. `row_count * columns.len()` must equal `cells.len()`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ActualResult<'a> {
    pub columns: &'a [ActualColumn<'a>],
    pub row_count: usize,
    pub cells: &'a [ActualCell<'a>],
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ExpectedValues<'a> {
    Lines(&'a [&'a [u8]]),
    File(&'a [u8]),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct QueryExpectation<'a> {
    /// The length of the query's `I`/`R`/`T` signature. The letters themselves
    /// do not constrain native logical types in the pinned development runner.
    pub expected_column_count: usize,
    pub values: ExpectedValues<'a>,
    pub sort: SortMode,
    pub fallback_sort: SortMode,
    pub label: Option<&'a str>,
    /// Zero disables the threshold. The comparison switches when value count
    /// is strictly greater than this number, matching the C++ helper.
    pub hash_threshold: usize,
    pub original_sqlite_test: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> QueryExpectation<'a> {
    pub fn lines(expected_column_count: usize, values: &'a [&'a [u8]]) -> Self {
        Self {
            expected_column_count,
            values: ExpectedValues::Lines(values),
            sort: SortMode::None,
            fallback_sort: SortMode::None,
            label: None,
            hash_threshold: 0,
            original_sqlite_test: false,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) trait Re2Matcher {
    /// Compile with RE2 UTF-8 syntax, `dot_nl=true`, then perform `FullMatch`.
    /// Invalid patterns must return `Err`, never a non-match.
    fn full_match(&self, pattern: &[u8], value: &[u8]) -> Result<bool, String>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) trait ExpectedSubstitutions {
    fn replace(&self, input: &[u8]) -> Vec<u8>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) trait ExpectedFileResolver {
    fn load(&self, path: &[u8], column_names: &[&str]) -> Result<ResolvedExpected, String>;
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ResolvedExpected {
    pub column_count: usize,
    pub values: Vec<Vec<u8>>,
}

/// A conservative `<FILE>:` resolver for source-owned fixtures. It rejects
/// absolute paths, `..`, symlink escapes, missing files, malformed pipe CSV,
/// and row cardinality mismatches.
#[derive(Debug)]
pub(crate) struct SourceRootFileResolver {
    root: PathBuf,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SourceRootFileResolver {
    pub fn new(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("cannot resolve source root {}: {error}", root.display()))?;
        if !root.is_dir() {
            return Err(format!("source root {} is not a directory", root.display()));
        }
        Ok(Self { root })
    }

    fn resolve(&self, raw: &[u8]) -> Result<PathBuf, String> {
        let text = std::str::from_utf8(raw)
            .map_err(|_| "expected-result file path is not valid UTF-8".to_string())?;
        let relative = Path::new(text);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(format!(
                "expected-result path escapes source root: {text:?}"
            ));
        }
        let path = self.root.join(relative);
        let canonical = path.canonicalize().map_err(|error| {
            format!(
                "cannot read expected-result file {}: {error}",
                path.display()
            )
        })?;
        if !canonical.starts_with(&self.root) {
            return Err(format!(
                "expected-result path escapes source root: {text:?}"
            ));
        }
        if !canonical.is_file() {
            return Err(format!(
                "expected-result path is not a file: {}",
                canonical.display()
            ));
        }
        Ok(canonical)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpectedFileResolver for SourceRootFileResolver {
    fn load(&self, path: &[u8], column_names: &[&str]) -> Result<ResolvedExpected, String> {
        let path = self.resolve(path)?;
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "cannot read expected-result file {}: {error}",
                path.display()
            )
        })?;
        let rows = parse_pipe_csv(&bytes)?;
        let Some(header) = rows.first() else {
            return Err(format!("expected-result file {} is empty", path.display()));
        };
        let column_count = header.len();
        if column_count != column_names.len() {
            return Err(format!(
                "expected-result file {} has {} columns, query has {}",
                path.display(),
                column_count,
                column_names.len()
            ));
        }
        let mut values = Vec::new();
        for (index, row) in rows.into_iter().skip(1).enumerate() {
            if row.len() != column_count {
                return Err(format!(
                    "expected-result file {} row {} has {} columns, expected {}",
                    path.display(),
                    index + 2,
                    row.len(),
                    column_count
                ));
            }
            values.extend(row);
        }
        Ok(ResolvedExpected {
            column_count,
            values,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorKind {
    Regular,
    Unsupported,
    Internal,
    Verification,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActualError<'a> {
    /// `MaterializedQueryResult::GetError()` in the C++ runner.
    pub message: &'a [u8],
    /// `MaterializedQueryResult::ToString()`, used for regex matching.
    pub rendered: &'a [u8],
    pub kind: ErrorKind,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum StatementResult<'a> {
    Success,
    Error(ActualError<'a>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExpectedStatement<'a> {
    Success,
    Error(Option<&'a [u8]>),
    Unknown(Option<&'a [u8]>),
    DontCare,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum OracleError {
    InvalidActual(String),
    Conversion(String),
    Cardinality {
        expected_columns: usize,
        actual_columns: usize,
        expected_rows: Option<usize>,
        actual_rows: usize,
    },
    ExpectedShape(String),
    ValueMismatch {
        row: usize,
        column: usize,
        column_name: String,
        expected: Vec<u8>,
        actual: Vec<u8>,
    },
    HashMismatch {
        expected: String,
        actual: String,
    },
    LabelMismatch {
        label: String,
        expected: String,
        actual: String,
    },
    MissingLabel(String),
    RegexCapabilityUnavailable,
    RegexInvalid(String),
    FileCapabilityUnavailable,
    File(String),
    UnexpectedStatementSuccess,
    UnexpectedStatementError {
        kind: ErrorKind,
        message: Vec<u8>,
    },
    ErrorMessageMismatch {
        expected: Vec<u8>,
        actual: Vec<u8>,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for OracleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::error::Error for OracleError {}

pub(crate) struct Oracle<'a> {
    regex: Option<&'a dyn Re2Matcher>,
    substitutions: Option<&'a dyn ExpectedSubstitutions>,
    files: Option<&'a dyn ExpectedFileResolver>,
    labels: BTreeMap<String, String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> Default for Oracle<'a> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> Oracle<'a> {
    pub fn new() -> Self {
        Self {
            regex: None,
            substitutions: None,
            files: None,
            labels: BTreeMap::new(),
        }
    }

    pub fn with_regex(mut self, regex: &'a dyn Re2Matcher) -> Self {
        self.regex = Some(regex);
        self
    }

    pub fn with_substitutions(mut self, substitutions: &'a dyn ExpectedSubstitutions) -> Self {
        self.substitutions = Some(substitutions);
        self
    }

    pub fn with_files(mut self, files: &'a dyn ExpectedFileResolver) -> Self {
        self.files = Some(files);
        self
    }

    pub fn reset_label(&mut self, label: &str) -> Result<(), OracleError> {
        self.labels
            .remove(label)
            .map(|_| ())
            .ok_or_else(|| OracleError::MissingLabel(label.to_string()))
    }

    pub fn check_query(
        &mut self,
        actual: ActualResult<'_>,
        expectation: QueryExpectation<'_>,
    ) -> Result<(), OracleError> {
        validate_actual(actual)?;
        let mut actual_values = convert_result(actual, expectation.original_sqlite_test)?;
        sort_values(expectation.sort, &mut actual_values, actual.columns.len())?;

        let total_values = actual_values.len();
        let (mut expected_columns, mut expected_values) =
            self.load_expected(actual, expectation.values)?;
        if expected_columns == 0 {
            expected_columns = expectation.expected_column_count;
        }
        let explicit_hash = expected_values.len() == 1 && result_is_hash(&expected_values[0]);
        let compare_hash = expectation.label.is_some()
            || (expectation.hash_threshold > 0 && total_values > expectation.hash_threshold)
            || explicit_hash;
        let actual_hash = if compare_hash {
            Some(result_hash(&actual_values))
        } else {
            None
        };

        if compare_hash {
            let actual_hash = actual_hash.expect("hash was computed");
            let mut label_mismatch = None;
            if let Some(label) = expectation.label {
                if let Some(prior) = self.labels.get(label) {
                    if prior != &actual_hash {
                        label_mismatch = Some((label.to_string(), prior.clone()));
                    }
                } else {
                    self.labels.insert(label.to_string(), actual_hash.clone());
                }
            }
            if explicit_hash {
                let expected = String::from_utf8_lossy(&expected_values[0]).into_owned();
                if expected != actual_hash {
                    return Err(OracleError::HashMismatch {
                        expected,
                        actual: actual_hash,
                    });
                }
                // The pinned helper assigns (rather than ORs) the explicit-hash
                // comparison after checking a label. Preserve that ordering: a
                // matching explicit hash supersedes a prior-label mismatch.
            } else if let Some((label, expected)) = label_mismatch {
                return Err(OracleError::LabelMismatch {
                    label,
                    expected,
                    actual: actual_hash,
                });
            }
            return Ok(());
        }

        let actual_columns = actual.columns.len();
        if expected_columns != actual_columns || expected_columns == 0 {
            return Err(OracleError::Cardinality {
                expected_columns,
                actual_columns,
                expected_rows: None,
                actual_rows: actual.row_count,
            });
        }

        let mut row_wise = expected_columns > 1 && expected_values.len() == actual.row_count;
        if !row_wise {
            row_wise = expected_values.iter().all(|value| value.contains(&b'\t'));
        }
        let expected_rows = if row_wise {
            expected_values.len()
        } else {
            if expected_values.len() % expected_columns != 0 {
                return Err(OracleError::ExpectedShape(format!(
                    "{} expected values are not divisible by {expected_columns} columns",
                    expected_values.len()
                )));
            }
            expected_values.len() / expected_columns
        };
        if expected_rows != actual.row_count {
            return Err(OracleError::Cardinality {
                expected_columns,
                actual_columns,
                expected_rows: Some(expected_rows),
                actual_rows: actual.row_count,
            });
        }
        if row_wise {
            let mut split = Vec::with_capacity(total_values);
            for (index, row) in expected_values.iter().enumerate() {
                let fields: Vec<_> = row.split(|byte| *byte == b'\t').collect();
                if fields.len() != expected_columns {
                    return Err(OracleError::ExpectedShape(format!(
                        "expected row {} has {} tab-separated values, expected {expected_columns}",
                        index + 1,
                        fields.len()
                    )));
                }
                split.extend(fields.into_iter().map(<[u8]>::to_vec));
            }
            expected_values = split;
        }

        match self.compare_all(actual, &actual_values, &expected_values) {
            Ok(()) => Ok(()),
            Err(_first_error) if expectation.fallback_sort != SortMode::None => {
                sort_values(
                    expectation.fallback_sort,
                    &mut actual_values,
                    actual_columns,
                )?;
                sort_values(
                    expectation.fallback_sort,
                    &mut expected_values,
                    expected_columns,
                )?;
                self.compare_all(actual, &actual_values, &expected_values)
            }
            Err(error) => Err(error),
        }
    }

    pub fn check_statement(
        &self,
        actual: StatementResult<'_>,
        expected: ExpectedStatement<'_>,
    ) -> Result<(), OracleError> {
        if let StatementResult::Error(error) = actual
            && matches!(
                error.kind,
                ErrorKind::Unsupported | ErrorKind::Internal | ErrorKind::Verification
            )
        {
            return Err(OracleError::UnexpectedStatementError {
                kind: error.kind,
                message: error.message.to_vec(),
            });
        }
        match (expected, actual) {
            (ExpectedStatement::Success, StatementResult::Success)
            | (ExpectedStatement::DontCare, _)
            | (ExpectedStatement::Unknown(None), _)
            | (ExpectedStatement::Error(None), StatementResult::Error(_)) => Ok(()),
            (ExpectedStatement::Success, StatementResult::Error(error)) => {
                Err(OracleError::UnexpectedStatementError {
                    kind: error.kind,
                    message: error.message.to_vec(),
                })
            }
            (ExpectedStatement::Error(_), StatementResult::Success) => {
                Err(OracleError::UnexpectedStatementSuccess)
            }
            (ExpectedStatement::Unknown(Some(expected)), StatementResult::Success) => {
                // RESULT_UNKNOWN accepts success; an expected diagnostic only
                // constrains an error when one exists.
                let _ = expected;
                Ok(())
            }
            (
                ExpectedStatement::Error(Some(expected))
                | ExpectedStatement::Unknown(Some(expected)),
                StatementResult::Error(error),
            ) => self.compare_error(error, expected),
        }
    }

    fn load_expected(
        &self,
        actual: ActualResult<'_>,
        expected: ExpectedValues<'_>,
    ) -> Result<(usize, Vec<Vec<u8>>), OracleError> {
        match expected {
            ExpectedValues::Lines(lines) if lines.len() == 1 && lines[0].starts_with(FILE) => {
                let resolver = self.files.ok_or(OracleError::FileCapabilityUnavailable)?;
                let mut path = lines[0][FILE.len()..].to_vec();
                if let Some(substitutions) = self.substitutions {
                    path = substitutions.replace(&path);
                }
                let names: Vec<_> = actual.columns.iter().map(|column| column.name).collect();
                let resolved = resolver.load(&path, &names).map_err(OracleError::File)?;
                Ok((resolved.column_count, resolved.values))
            }
            ExpectedValues::Lines(lines) => {
                Ok((0, lines.iter().map(|line| line.to_vec()).collect()))
            }
            ExpectedValues::File(path) => {
                let resolver = self.files.ok_or(OracleError::FileCapabilityUnavailable)?;
                let mut path = path.to_vec();
                if let Some(substitutions) = self.substitutions {
                    path = substitutions.replace(&path);
                }
                let names: Vec<_> = actual.columns.iter().map(|column| column.name).collect();
                let resolved = resolver.load(&path, &names).map_err(OracleError::File)?;
                Ok((resolved.column_count, resolved.values))
            }
        }
    }

    fn compare_all(
        &self,
        actual: ActualResult<'_>,
        actual_values: &[Vec<u8>],
        expected_values: &[Vec<u8>],
    ) -> Result<(), OracleError> {
        for (index, (actual_value, expected_value)) in
            actual_values.iter().zip(expected_values).enumerate()
        {
            let column = index % actual.columns.len();
            if !self.compare_value(
                actual_value,
                expected_value,
                actual.columns[column].logical_type,
            )? {
                return Err(OracleError::ValueMismatch {
                    row: index / actual.columns.len() + 1,
                    column: column + 1,
                    column_name: actual.columns[column].name.to_string(),
                    expected: expected_value.clone(),
                    actual: actual_value.clone(),
                });
            }
        }
        Ok(())
    }

    fn compare_value(
        &self,
        actual: &[u8],
        expected: &[u8],
        logical_type: &str,
    ) -> Result<bool, OracleError> {
        if actual == expected {
            return Ok(true);
        }
        if let Some(substitutions) = self.substitutions
            && actual == substitutions.replace(expected)
        {
            return Ok(true);
        }
        if expected.starts_with(REGEX) || expected.starts_with(NOT_REGEX) {
            return self.matches_regex(actual, expected);
        }
        if let Some(kind) = NumericKind::from_logical_type(logical_type) {
            return Ok(compare_numeric(kind, actual, expected));
        }
        if base_type(logical_type) == "BOOLEAN" {
            return Ok(parse_bool(actual)
                .zip(parse_bool(expected))
                .is_some_and(|(a, b)| a == b));
        }
        Ok(false)
    }

    fn compare_error(&self, actual: ActualError<'_>, expected: &[u8]) -> Result<(), OracleError> {
        if contains(actual.message, expected) {
            return Ok(());
        }
        if let Some(substitutions) = self.substitutions
            && contains(actual.message, &substitutions.replace(expected))
        {
            return Ok(());
        }
        if (expected.starts_with(REGEX) || expected.starts_with(NOT_REGEX))
            && self.matches_regex(actual.rendered, expected)?
        {
            return Ok(());
        }
        Err(OracleError::ErrorMessageMismatch {
            expected: expected.to_vec(),
            actual: actual.message.to_vec(),
        })
    }

    fn matches_regex(&self, value: &[u8], expectation: &[u8]) -> Result<bool, OracleError> {
        let want_match = expectation.starts_with(REGEX);
        let mut pattern = expectation.to_vec();
        remove_all(&mut pattern, REGEX);
        remove_all(&mut pattern, NOT_REGEX);
        let matcher = self.regex.ok_or(OracleError::RegexCapabilityUnavailable)?;
        let matched = matcher
            .full_match(&pattern, value)
            .map_err(OracleError::RegexInvalid)?;
        Ok(if want_match { matched } else { !matched })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Deterministic timing payload for Gate P. Both implementations can convert,
/// sort, and MD5 the same result `iterations` times; the returned checksum keeps
/// the work observable without adding I/O to the timed region.
pub(crate) fn oracle_benchmark_checksum(
    actual: ActualResult<'_>,
    sort: SortMode,
    original_sqlite_test: bool,
    iterations: usize,
) -> Result<u64, OracleError> {
    validate_actual(actual)?;
    let mut checksum = 0xcbf2_9ce4_8422_2325_u64;
    for iteration in 0..iterations {
        let mut values = convert_result(actual, original_sqlite_test)?;
        sort_values(sort, &mut values, actual.columns.len())?;
        let hash = result_hash(&values);
        for byte in hash.bytes().chain((iteration as u64).to_le_bytes()) {
            checksum ^= u64::from(byte);
            checksum = checksum.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Ok(checksum)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_actual(actual: ActualResult<'_>) -> Result<(), OracleError> {
    let expected = actual
        .row_count
        .checked_mul(actual.columns.len())
        .ok_or_else(|| OracleError::InvalidActual("result dimensions overflow".to_string()))?;
    if expected != actual.cells.len() {
        return Err(OracleError::InvalidActual(format!(
            "{} rows * {} columns requires {expected} cells, got {}",
            actual.row_count,
            actual.columns.len(),
            actual.cells.len()
        )));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn convert_result(actual: ActualResult<'_>, sqlite: bool) -> Result<Vec<Vec<u8>>, OracleError> {
    actual
        .cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            convert_cell(
                *cell,
                actual.columns[index % actual.columns.len()].logical_type,
                sqlite,
            )
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn convert_cell(
    cell: ActualCell<'_>,
    logical_type: &str,
    sqlite: bool,
) -> Result<Vec<u8>, OracleError> {
    let bytes = match cell {
        ActualCell::Null => return Ok(b"NULL".to_vec()),
        ActualCell::Text(text) => text.as_bytes(),
        ActualCell::Bytes(bytes) => bytes,
    };
    let base = base_type(logical_type);
    if sqlite && matches!(base.as_str(), "DECIMAL" | "FLOAT" | "DOUBLE") {
        return sqlite_integer_render(bytes, logical_type);
    }
    if base == "BOOLEAN" {
        return parse_bool(bytes)
            .map(|value| if value { b"1".to_vec() } else { b"0".to_vec() })
            .ok_or_else(|| {
                OracleError::Conversion(format!("invalid BOOLEAN rendering {:?}", bytes))
            });
    }
    if bytes.is_empty() {
        return Ok(b"(empty)".to_vec());
    }
    let mut result = Vec::with_capacity(bytes.len());
    for byte in bytes {
        if *byte == 0 {
            result.extend_from_slice(b"\\0");
        } else {
            result.push(*byte);
        }
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sqlite_integer_render(bytes: &[u8], logical_type: &str) -> Result<Vec<u8>, OracleError> {
    let kind = NumericKind::from_logical_type(logical_type)
        .ok_or_else(|| OracleError::Conversion(format!("not a numeric type: {logical_type}")))?;
    let integer = match kind {
        NumericKind::Float32 => {
            let value = parse_float(bytes, true)
                .ok_or_else(|| OracleError::Conversion("invalid FLOAT".into()))?;
            if !value.is_finite()
                || value.round() < i64::MIN as f64
                || value.round() > i64::MAX as f64
            {
                return Err(OracleError::Conversion(
                    "FLOAT cannot be cast to BIGINT".into(),
                ));
            }
            format!("{}", value.round() as i64)
        }
        NumericKind::Float64 => {
            let value = parse_float(bytes, false)
                .ok_or_else(|| OracleError::Conversion("invalid DOUBLE".into()))?;
            if !value.is_finite()
                || value.round() < i64::MIN as f64
                || value.round() > i64::MAX as f64
            {
                return Err(OracleError::Conversion(
                    "DOUBLE cannot be cast to BIGINT".into(),
                ));
            }
            format!("{}", value.round() as i64)
        }
        NumericKind::Decimal { .. } => {
            let normalized = normalize_decimal(bytes, 0)
                .ok_or_else(|| OracleError::Conversion("invalid DECIMAL".into()))?;
            normalized
                .parse::<i64>()
                .map_err(|_| OracleError::Conversion("DECIMAL cannot be cast to BIGINT".into()))?
                .to_string()
        }
        _ => unreachable!("SQLite conversion only selects float/decimal"),
    };
    Ok(integer.into_bytes())
}

#[derive(Clone, Copy, Debug)]
enum NumericKind {
    Signed(u32),
    Unsigned(u32),
    Decimal { precision: u32, scale: u32 },
    Float32,
    Float64,
    Bignum,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NumericKind {
    fn from_logical_type(logical_type: &str) -> Option<Self> {
        let base = base_type(logical_type);
        Some(match base.as_str() {
            "TINYINT" => Self::Signed(8),
            "SMALLINT" => Self::Signed(16),
            "INTEGER" | "INT" => Self::Signed(32),
            "BIGINT" => Self::Signed(64),
            "HUGEINT" => Self::Signed(128),
            "UTINYINT" => Self::Unsigned(8),
            "USMALLINT" => Self::Unsigned(16),
            "UINTEGER" | "UINT" => Self::Unsigned(32),
            "UBIGINT" => Self::Unsigned(64),
            "UHUGEINT" => Self::Unsigned(128),
            "FLOAT" | "REAL" => Self::Float32,
            "DOUBLE" => Self::Float64,
            "BIGNUM" => Self::Bignum,
            "DECIMAL" | "NUMERIC" => {
                let (precision, scale) = decimal_parameters(logical_type).unwrap_or((18, 3));
                Self::Decimal { precision, scale }
            }
            _ => return None,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare_numeric(kind: NumericKind, left: &[u8], right: &[u8]) -> bool {
    if left == b"NULL" || right == b"NULL" {
        return left == b"NULL" && right == b"NULL";
    }
    match kind {
        NumericKind::Signed(bits) => parse_signed(left, bits)
            .zip(parse_signed(right, bits))
            .is_some_and(|(a, b)| a == b),
        NumericKind::Unsigned(bits) => parse_unsigned(left, bits)
            .zip(parse_unsigned(right, bits))
            .is_some_and(|(a, b)| a == b),
        NumericKind::Decimal { precision, scale } => parse_decimal(left, precision, scale)
            .zip(parse_decimal(right, precision, scale))
            .is_some_and(|(a, b)| a == b),
        NumericKind::Float32 => parse_float_value(left, true)
            .zip(parse_float_value(right, true))
            .is_some_and(|(a, b)| float_equal(a, b)),
        NumericKind::Float64 => parse_float_value(left, false)
            .zip(parse_float_value(right, false))
            .is_some_and(|(a, b)| float_equal(a, b)),
        NumericKind::Bignum => normalize_free_decimal(left)
            .zip(normalize_free_decimal(right))
            .is_some_and(|(a, b)| a == b),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_signed(bytes: &[u8], bits: u32) -> Option<i128> {
    let value = normalize_decimal(bytes, 0)?.parse::<i128>().ok()?;
    let (minimum, maximum) = if bits == 128 {
        (i128::MIN, i128::MAX)
    } else {
        (-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1)
    };
    (minimum..=maximum).contains(&value).then_some(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_unsigned(bytes: &[u8], bits: u32) -> Option<u128> {
    let normalized = normalize_decimal(bytes, 0)?;
    if normalized.starts_with('-') {
        return None;
    }
    let value = normalized.parse::<u128>().ok()?;
    let maximum = if bits == 128 {
        u128::MAX
    } else {
        (1_u128 << bits) - 1
    };
    (value <= maximum).then_some(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_decimal(bytes: &[u8], precision: u32, scale: u32) -> Option<String> {
    let value = normalize_decimal(bytes, scale)?;
    let digits = value
        .bytes()
        .filter(u8::is_ascii_digit)
        .skip_while(|byte| *byte == b'0')
        .count();
    (digits.max(1) <= precision as usize).then_some(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_float_value(bytes: &[u8], single: bool) -> Option<f64> {
    let value = parse_float(bytes, single)?;
    if single {
        Some(f64::from(value as f32))
    } else {
        Some(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn float_equal(left: f64, right: f64) -> bool {
    left == right || (left.is_nan() && right.is_nan())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_float(bytes: &[u8], single: bool) -> Option<f64> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if single {
        text.parse::<f32>().ok().map(f64::from)
    } else {
        text.parse::<f64>().ok()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Normalize and round a decimal string to `scale` places. DuckDB casts from
/// VARCHAR before comparing numeric values, so representational differences
/// such as `1`, `1.0`, and `1e0` compare through the declared native type.
fn normalize_decimal(bytes: &[u8], scale: u32) -> Option<String> {
    const MAX_NORMALIZED_DIGITS: usize = 1 << 20;
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    let (negative, unsigned) = match text.as_bytes()[0] {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => (
            &unsigned[..index],
            unsigned[index + 1..].parse::<i32>().ok()?,
        ),
        None => (unsigned, 0),
    };
    let mut digits = Vec::new();
    let mut dot = None;
    for byte in mantissa.bytes() {
        match byte {
            b'0'..=b'9' => digits.push(byte),
            b'.' if dot.is_none() => dot = Some(digits.len()),
            _ => return None,
        }
    }
    if digits.is_empty() {
        return None;
    }
    if scale as usize > digits.len().saturating_add(MAX_NORMALIZED_DIGITS) {
        return None;
    }
    let dot = dot.unwrap_or(digits.len()) as i64;
    let keep = dot + i64::from(exponent) + i64::from(scale);
    let mut scaled = if keep <= 0 {
        vec![b'0']
    } else {
        let keep = usize::try_from(keep).ok()?;
        if keep > digits.len().saturating_add(MAX_NORMALIZED_DIGITS) {
            return None;
        }
        if keep >= digits.len() {
            let mut result = digits.clone();
            result.resize(keep, b'0');
            result
        } else {
            digits[..keep].to_vec()
        }
    };
    let round_up = if keep < 0 {
        false
    } else {
        usize::try_from(keep)
            .ok()
            .and_then(|index| digits.get(index))
            .is_some_and(|digit| *digit >= b'5')
    };
    if round_up {
        increment_decimal_digits(&mut scaled);
    }
    let first_nonzero = scaled
        .iter()
        .position(|digit| *digit != b'0')
        .unwrap_or(scaled.len() - 1);
    let scaled = &scaled[first_nonzero..];
    let is_zero = scaled.iter().all(|digit| *digit == b'0');
    let mut result = String::new();
    if negative && !is_zero {
        result.push('-');
    }
    if scale == 0 {
        result.push_str(std::str::from_utf8(scaled).ok()?);
        return Some(result);
    }
    let scale = scale as usize;
    if scaled.len() <= scale {
        result.push('0');
        result.push('.');
        result.extend(std::iter::repeat_n('0', scale - scaled.len()));
        result.push_str(std::str::from_utf8(scaled).ok()?);
    } else {
        let split = scaled.len() - scale;
        result.push_str(std::str::from_utf8(&scaled[..split]).ok()?);
        result.push('.');
        result.push_str(std::str::from_utf8(&scaled[split..]).ok()?);
    }
    Some(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn normalize_free_decimal(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (&text[..index], text[index + 1..].parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (_, fractional) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let scale = i64::from(fractional.len() as u32)
        .checked_sub(i64::from(exponent))?
        .max(0) as u32;
    let mut normalized = normalize_decimal(bytes, scale)?;
    while normalized.ends_with('0') && normalized.contains('.') {
        normalized.pop();
    }
    if normalized.ends_with('.') {
        normalized.pop();
    }
    Some(normalized)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn increment_decimal_digits(digits: &mut Vec<u8>) {
    for digit in digits.iter_mut().rev() {
        if *digit < b'9' {
            *digit += 1;
            return;
        }
        *digit = b'0';
    }
    digits.insert(0, b'1');
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decimal_parameters(logical_type: &str) -> Option<(u32, u32)> {
    let start = logical_type.find('(')? + 1;
    let end = logical_type[start..].find(')')? + start;
    let (precision, scale) = logical_type[start..end].split_once(',')?;
    Some((precision.trim().parse().ok()?, scale.trim().parse().ok()?))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn base_type(logical_type: &str) -> String {
    logical_type
        .split(['(', '<', '['])
        .next()
        .unwrap_or(logical_type)
        .trim()
        .to_ascii_uppercase()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_bool(bytes: &[u8]) -> Option<bool> {
    if bytes == b"1" || bytes.eq_ignore_ascii_case(b"true") {
        Some(true)
    } else if bytes == b"0" || bytes.eq_ignore_ascii_case(b"false") {
        Some(false)
    } else {
        None
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sort_values(mode: SortMode, values: &mut [Vec<u8>], columns: usize) -> Result<(), OracleError> {
    match mode {
        SortMode::None => Ok(()),
        SortMode::Values => {
            values.sort();
            Ok(())
        }
        SortMode::Rows => {
            if columns == 0 || !values.len().is_multiple_of(columns) {
                return Err(OracleError::ExpectedShape(format!(
                    "cannot row-sort {} values into {columns} columns",
                    values.len()
                )));
            }
            values.chunks_exact_mut(columns).for_each(|_| {});
            let mut rows: Vec<Vec<Vec<u8>>> =
                values.chunks(columns).map(<[Vec<u8>]>::to_vec).collect();
            rows.sort();
            for (target, value) in values.iter_mut().zip(rows.into_iter().flatten()) {
                *target = value;
            }
            Ok(())
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn result_hash(values: &[Vec<u8>]) -> String {
    let mut md5 = Md5::new();
    for value in values {
        md5.update(value);
        md5.update(b"\n");
    }
    format!("{} values hashing to {}", values.len(), md5.finish_hex())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn result_is_hash(value: &[u8]) -> bool {
    let digits = value
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 {
        return false;
    }
    let suffix = &value[digits..];
    let prefix = b" values hashing to ";
    if !suffix.starts_with(prefix) || suffix.len() != prefix.len() + 32 {
        return false;
    }
    suffix[prefix.len()..]
        .iter()
        .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn remove_all(value: &mut Vec<u8>, needle: &[u8]) {
    while let Some(index) = value
        .windows(needle.len())
        .position(|window| window == needle)
    {
        value.drain(index..index + needle.len());
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_pipe_csv(input: &[u8]) -> Result<Vec<Vec<Vec<u8>>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = Vec::new();
    let mut quoted = false;
    let mut index = 0;
    while index < input.len() {
        let byte = input[index];
        if quoted {
            if byte == b'"' {
                if input.get(index + 1) == Some(&b'"') {
                    field.push(b'"');
                    index += 1;
                } else {
                    quoted = false;
                }
            } else {
                field.push(byte);
            }
        } else {
            match byte {
                b'"' if field.is_empty() => quoted = true,
                b'|' => row.push(std::mem::take(&mut field)),
                b'\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                b'\r' if input.get(index + 1) == Some(&b'\n') => {}
                _ => field.push(byte),
            }
        }
        index += 1;
    }
    if quoted {
        return Err("unterminated quoted field in expected-result file".to_string());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

// Minimal RFC 1321 implementation kept local so the oracle's wire format does
// not depend on a different digest crate. This is the same lowercase MD5 used
// by DuckDB's `MD5Context::FinishHex()`.
struct Md5 {
    state: [u32; 4],
    length: u64,
    pending: Vec<u8>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Md5 {
    fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            length: 0,
            pending: Vec::new(),
        }
    }

    fn update(&mut self, input: &[u8]) {
        self.length = self.length.wrapping_add(input.len() as u64);
        self.pending.extend_from_slice(input);
        while self.pending.len() >= 64 {
            let block: [u8; 64] = self.pending[..64].try_into().expect("block length");
            self.compress(&block);
            self.pending.drain(..64);
        }
    }

    fn finish_hex(mut self) -> String {
        let bit_length = self.length.wrapping_mul(8);
        self.pending.push(0x80);
        while self.pending.len() % 64 != 56 {
            self.pending.push(0);
        }
        self.pending.extend_from_slice(&bit_length.to_le_bytes());
        while !self.pending.is_empty() {
            let block: [u8; 64] = self.pending[..64].try_into().expect("block length");
            self.compress(&block);
            self.pending.drain(..64);
        }
        let mut result = String::with_capacity(32);
        for word in self.state {
            for byte in word.to_le_bytes() {
                use std::fmt::Write;
                write!(result, "{byte:02x}").expect("writing to String cannot fail");
            }
        }
        result
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const SHIFT: [u32; 64] = [
            7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
            5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
            6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
        ];
        const K: [u32; 64] = [
            0xd76a_a478,
            0xe8c7_b756,
            0x2420_70db,
            0xc1bd_ceee,
            0xf57c_0faf,
            0x4787_c62a,
            0xa830_4613,
            0xfd46_9501,
            0x6980_98d8,
            0x8b44_f7af,
            0xffff_5bb1,
            0x895c_d7be,
            0x6b90_1122,
            0xfd98_7193,
            0xa679_438e,
            0x49b4_0821,
            0xf61e_2562,
            0xc040_b340,
            0x265e_5a51,
            0xe9b6_c7aa,
            0xd62f_105d,
            0x0244_1453,
            0xd8a1_e681,
            0xe7d3_fbc8,
            0x21e1_cde6,
            0xc337_07d6,
            0xf4d5_0d87,
            0x455a_14ed,
            0xa9e3_e905,
            0xfcef_a3f8,
            0x676f_02d9,
            0x8d2a_4c8a,
            0xfffa_3942,
            0x8771_f681,
            0x6d9d_6122,
            0xfde5_380c,
            0xa4be_ea44,
            0x4bde_cfa9,
            0xf6bb_4b60,
            0xbebf_bc70,
            0x289b_7ec6,
            0xeaa1_27fa,
            0xd4ef_3085,
            0x0488_1d05,
            0xd9d4_d039,
            0xe6db_99e5,
            0x1fa2_7cf8,
            0xc4ac_5665,
            0xf429_2244,
            0x432a_ff97,
            0xab94_23a7,
            0xfc93_a039,
            0x655b_59c3,
            0x8f0c_cc92,
            0xffef_f47d,
            0x8584_5dd1,
            0x6fa8_7e4f,
            0xfe2c_e6e0,
            0xa301_4314,
            0x4e08_11a1,
            0xf753_7e82,
            0xbd3a_f235,
            0x2ad7_d2bb,
            0xeb86_d391,
        ];
        let mut words = [0_u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            *word = u32::from_le_bytes(block[index * 4..index * 4 + 4].try_into().expect("word"));
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for index in 0..64 {
            let (function, word) = match index {
                0..=15 => ((b & c) | (!b & d), index),
                16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let next = b.wrapping_add(
                a.wrapping_add(function)
                    .wrapping_add(K[index])
                    .wrapping_add(words[word])
                    .rotate_left(SHIFT[index]),
            );
            a = d;
            d = c;
            c = b;
            b = next;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn query<'a>(
        columns: &'a [ActualColumn<'a>],
        cells: &'a [ActualCell<'a>],
        rows: usize,
    ) -> ActualResult<'a> {
        ActualResult {
            columns,
            row_count: rows,
            cells,
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn expected<'a>(columns: usize, lines: &'a [&'a [u8]]) -> QueryExpectation<'a> {
        QueryExpectation::lines(columns, lines)
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn pinned_conversion_null_boolean_empty_and_nul() {
        let columns = [
            ActualColumn {
                name: "n",
                logical_type: "INTEGER",
            },
            ActualColumn {
                name: "b",
                logical_type: "BOOLEAN",
            },
            ActualColumn {
                name: "e",
                logical_type: "VARCHAR",
            },
            ActualColumn {
                name: "z",
                logical_type: "VARCHAR",
            },
        ];
        let cells = [
            ActualCell::Null,
            ActualCell::Text("true"),
            ActualCell::Text(""),
            ActualCell::Bytes(b"a\0b"),
        ];
        let lines: &[&[u8]] = &[b"NULL", b"1", b"(empty)", b"a\\0b"];
        Oracle::new()
            .check_query(query(&columns, &cells, 1), expected(4, lines))
            .unwrap();
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn signature_is_column_count_not_type_assertion() {
        let columns = [ActualColumn {
            name: "v",
            logical_type: "VARCHAR",
        }];
        let cells = [ActualCell::Text("hello")];
        let lines: &[&[u8]] = &[b"hello"];
        // An `I` signature has length one. The pinned development runner does
        // not reject VARCHAR metadata merely because the marker was `I`.
        Oracle::new()
            .check_query(query(&columns, &cells, 1), expected(1, lines))
            .unwrap();
        let wrong_columns = expected(2, lines);
        assert!(matches!(
            Oracle::new().check_query(query(&columns, &cells, 1), wrong_columns),
            Err(OracleError::Cardinality { .. })
        ));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn wrong_value_and_logical_type_mutations_fail() {
        let numeric_columns = [ActualColumn {
            name: "v",
            logical_type: "DOUBLE",
        }];
        let text_columns = [ActualColumn {
            name: "v",
            logical_type: "VARCHAR",
        }];
        let cells = [ActualCell::Text("1.0")];
        let equivalent: &[&[u8]] = &[b"1"];
        Oracle::new()
            .check_query(query(&numeric_columns, &cells, 1), expected(1, equivalent))
            .unwrap();
        assert!(matches!(
            Oracle::new().check_query(query(&text_columns, &cells, 1), expected(1, equivalent)),
            Err(OracleError::ValueMismatch { .. })
        ));
        let changed: &[&[u8]] = &[b"2"];
        assert!(matches!(
            Oracle::new().check_query(query(&numeric_columns, &cells, 1), expected(1, changed)),
            Err(OracleError::ValueMismatch { .. })
        ));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn numeric_cast_equality_and_boundaries() {
        let cases: &[(&str, &[u8], &[u8], bool)] = &[
            ("FLOAT", b"16777216", b"16777217", true),
            ("DOUBLE", b"9007199254740992", b"9007199254740993", true),
            ("DOUBLE", b"NaN", b"nan", true),
            ("DOUBLE", b"inf", b"+inf", true),
            ("DOUBLE", b"-0", b"0.0", true),
            ("DOUBLE", b"inf", b"-inf", false),
            ("TINYINT", b"127", b"127.0", true),
            ("TINYINT", b"127", b"128", false),
            ("UTINYINT", b"0", b"-0.6", false),
            ("DECIMAL(38,2)", b"1.20", b"1.2", true),
            ("DECIMAL(3,2)", b"9.99", b"10.00", false),
            ("BIGNUM", b"0.012", b"1.2e-2", true),
        ];
        for (logical_type, actual_value, expected_value, succeeds) in cases {
            let columns = [ActualColumn {
                name: "v",
                logical_type,
            }];
            let cells = [ActualCell::Bytes(actual_value)];
            let lines: &[&[u8]] = &[*expected_value];
            assert_eq!(
                Oracle::new()
                    .check_query(query(&columns, &cells, 1), expected(1, lines))
                    .is_ok(),
                *succeeds,
                "{logical_type}: {:?} vs {:?}",
                actual_value,
                expected_value
            );
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn original_sqlite_numeric_conversion_uses_bigint_rendering() {
        let columns = [
            ActualColumn {
                name: "d",
                logical_type: "DOUBLE",
            },
            ActualColumn {
                name: "f",
                logical_type: "FLOAT",
            },
            ActualColumn {
                name: "n",
                logical_type: "DECIMAL(4,2)",
            },
        ];
        let cells = [
            ActualCell::Text("1.5"),
            ActualCell::Text("-1.5"),
            ActualCell::Text("2.49"),
        ];
        let lines: &[&[u8]] = &[b"2", b"-2", b"2"];
        let mut expectation = expected(3, lines);
        expectation.original_sqlite_test = true;
        Oracle::new()
            .check_query(query(&columns, &cells, 1), expectation)
            .unwrap();
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn huge_signed_unsigned_and_decimal_values_are_exact() {
        let columns = [
            ActualColumn {
                name: "h",
                logical_type: "HUGEINT",
            },
            ActualColumn {
                name: "u",
                logical_type: "UHUGEINT",
            },
            ActualColumn {
                name: "d",
                logical_type: "DECIMAL(38,0)",
            },
        ];
        let cells = [
            ActualCell::Text("-170141183460469231731687303715884105728"),
            ActualCell::Text("340282366920938463463374607431768211455"),
            ActualCell::Text("99999999999999999999999999999999999999"),
        ];
        let lines: &[&[u8]] = &[
            b"-170141183460469231731687303715884105728",
            b"340282366920938463463374607431768211455",
            b"99999999999999999999999999999999999999",
        ];
        Oracle::new()
            .check_query(query(&columns, &cells, 1), expected(3, lines))
            .unwrap();

        let overflow: &[&[u8]] = &[
            b"-170141183460469231731687303715884105729",
            b"340282366920938463463374607431768211456",
            b"100000000000000000000000000000000000000",
        ];
        assert!(matches!(
            Oracle::new().check_query(query(&columns, &cells, 1), expected(3, overflow)),
            Err(OracleError::ValueMismatch { .. })
        ));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn cardinality_and_order_mutations_fail() {
        let columns = [
            ActualColumn {
                name: "a",
                logical_type: "INTEGER",
            },
            ActualColumn {
                name: "b",
                logical_type: "VARCHAR",
            },
        ];
        let cells = [
            ActualCell::Text("2"),
            ActualCell::Text("two"),
            ActualCell::Text("1"),
            ActualCell::Text("one"),
        ];
        let ordered: &[&[u8]] = &[b"1\tone", b"2\ttwo"];
        assert!(matches!(
            Oracle::new().check_query(query(&columns, &cells, 2), expected(2, ordered)),
            Err(OracleError::ValueMismatch { .. })
        ));
        let mut rowsorted = expected(2, ordered);
        rowsorted.sort = SortMode::Rows;
        Oracle::new()
            .check_query(query(&columns, &cells, 2), rowsorted)
            .unwrap();

        let missing_row: &[&[u8]] = &[b"1\tone"];
        assert!(matches!(
            Oracle::new().check_query(query(&columns, &cells, 2), expected(2, missing_row)),
            Err(OracleError::Cardinality { .. })
        ));
        let malformed: &[&[u8]] = &[b"1\tone\textra", b"2\ttwo"];
        assert!(matches!(
            Oracle::new().check_query(query(&columns, &cells, 2), expected(2, malformed)),
            Err(OracleError::ExpectedShape(_))
        ));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn multicolumn_valuesort_intentionally_drops_pairing_but_rowsort_does_not() {
        let columns = [
            ActualColumn {
                name: "a",
                logical_type: "VARCHAR",
            },
            ActualColumn {
                name: "b",
                logical_type: "VARCHAR",
            },
        ];
        let cells = [
            ActualCell::Text("a"),
            ActualCell::Text("2"),
            ActualCell::Text("b"),
            ActualCell::Text("1"),
        ];
        let flattened_sorted: &[&[u8]] = &[b"1", b"2", b"a", b"b"];
        let mut value_sorted = expected(2, flattened_sorted);
        value_sorted.sort = SortMode::Values;
        Oracle::new()
            .check_query(query(&columns, &cells, 2), value_sorted)
            .unwrap();

        let paired_rows: &[&[u8]] = &[b"a\t1", b"b\t2"];
        let mut row_sorted = expected(2, paired_rows);
        row_sorted.sort = SortMode::Rows;
        assert!(matches!(
            Oracle::new().check_query(query(&columns, &cells, 2), row_sorted),
            Err(OracleError::ValueMismatch { .. })
        ));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn pinned_md5_wire_format_and_hash_mutation() {
        assert_eq!(
            result_hash(&[b"1".to_vec()]),
            "1 values hashing to b026324c6904b2a9cb4b88d6d61c81d1"
        );

        // Unchanged upstream case: test/sql/order/test_order_large.test.
        let values: Vec<Vec<u8>> = (1..=10_000)
            .map(|value| value.to_string().into_bytes())
            .collect();
        assert_eq!(
            result_hash(&values),
            "10000 values hashing to 72d4ff27a28afbc066d5804999d5a504"
        );

        let columns = [ActualColumn {
            name: "a",
            logical_type: "INTEGER",
        }];
        let cells = [ActualCell::Text("1")];
        let good: &[&[u8]] = &[b"1 values hashing to b026324c6904b2a9cb4b88d6d61c81d1"];
        Oracle::new()
            .check_query(query(&columns, &cells, 1), expected(1, good))
            .unwrap();
        let wrong: &[&[u8]] = &[b"1 values hashing to a026324c6904b2a9cb4b88d6d61c81d1"];
        assert!(matches!(
            Oracle::new().check_query(query(&columns, &cells, 1), expected(1, wrong)),
            Err(OracleError::HashMismatch { .. })
        ));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn threshold_labels_and_reset_match_pinned_state_machine() {
        let columns = [ActualColumn {
            name: "a",
            logical_type: "INTEGER",
        }];
        let first_cells = [ActualCell::Text("1"), ActualCell::Text("2")];
        let second_cells = [ActualCell::Text("1"), ActualCell::Text("3")];
        let ignored_values: &[&[u8]] = &[b"deliberately ignored in hash mode"];
        let mut oracle = Oracle::new();
        let mut first = expected(1, ignored_values);
        first.label = Some("same");
        oracle
            .check_query(query(&columns, &first_cells, 2), first)
            .unwrap();
        assert!(matches!(
            oracle.check_query(query(&columns, &second_cells, 2), first),
            Err(OracleError::LabelMismatch { .. })
        ));
        let explicit_hash = result_hash(&[b"1".to_vec(), b"3".to_vec()]);
        let explicit_lines: &[&[u8]] = &[explicit_hash.as_bytes()];
        let mut explicit = expected(1, explicit_lines);
        explicit.label = Some("same");
        oracle
            .check_query(query(&columns, &second_cells, 2), explicit)
            .unwrap();
        oracle.reset_label("same").unwrap();
        oracle
            .check_query(query(&columns, &second_cells, 2), first)
            .unwrap();
        assert!(matches!(
            oracle.reset_label("missing"),
            Err(OracleError::MissingLabel(_))
        ));

        let mut threshold = expected(1, ignored_values);
        threshold.hash_threshold = 1;
        Oracle::new()
            .check_query(query(&columns, &first_cells, 2), threshold)
            .unwrap();
    }

    struct StubRe2;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Re2Matcher for StubRe2 {
        fn full_match(&self, pattern: &[u8], value: &[u8]) -> Result<bool, String> {
            match pattern {
                b".*needle.*" => Ok(value.windows(6).any(|window| window == b"needle")),
                b"line.*two" => Ok(value == b"line\none two"), // dot_nl=true
                b"[" => Err("missing ]".to_string()),
                _ => Ok(false),
            }
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn regex_requires_re2_and_supports_positive_negative_fullmatch_and_dot_nl() {
        let columns = [ActualColumn {
            name: "v",
            logical_type: "VARCHAR",
        }];
        let cells = [ActualCell::Text("a needle here")];
        let positive: &[&[u8]] = &[b"<REGEX>:.*needle.*"];
        assert_eq!(
            Oracle::new().check_query(query(&columns, &cells, 1), expected(1, positive)),
            Err(OracleError::RegexCapabilityUnavailable)
        );
        Oracle::new()
            .with_regex(&StubRe2)
            .check_query(query(&columns, &cells, 1), expected(1, positive))
            .unwrap();
        let negative: &[&[u8]] = &[b"<!REGEX>:.*absent.*"];
        Oracle::new()
            .with_regex(&StubRe2)
            .check_query(query(&columns, &cells, 1), expected(1, negative))
            .unwrap();
        let wrong_negative: &[&[u8]] = &[b"<!REGEX>:.*needle.*"];
        assert!(matches!(
            Oracle::new()
                .with_regex(&StubRe2)
                .check_query(query(&columns, &cells, 1), expected(1, wrong_negative)),
            Err(OracleError::ValueMismatch { .. })
        ));
        let invalid: &[&[u8]] = &[b"<REGEX>:["];
        assert!(matches!(
            Oracle::new()
                .with_regex(&StubRe2)
                .check_query(query(&columns, &cells, 1), expected(1, invalid)),
            Err(OracleError::RegexInvalid(_))
        ));

        let multiline_cells = [ActualCell::Text("line\none two")];
        let multiline: &[&[u8]] = &[b"<REGEX>:line.*two"];
        Oracle::new()
            .with_regex(&StubRe2)
            .check_query(query(&columns, &multiline_cells, 1), expected(1, multiline))
            .unwrap();
    }

    struct ReplaceRoot;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl ExpectedSubstitutions for ReplaceRoot {
        fn replace(&self, input: &[u8]) -> Vec<u8> {
            if input == b"{EXPECTED}" {
                b"expected.csv".to_vec()
            } else if input == b"{VALUE}" {
                b"42".to_vec()
            } else {
                input.to_vec()
            }
        }
    }

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn temp_directory() -> PathBuf {
        let suffix = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "duckdb-rust-oracle-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn external_expected_file_is_source_rooted_and_pipe_csv_aware() {
        let root = temp_directory();
        std::fs::write(
            root.join("expected.csv"),
            b"a|b\n1|\"hello|world\"\n2|\"say \"\"hi\"\"\"\n",
        )
        .unwrap();
        let resolver = SourceRootFileResolver::new(&root).unwrap();
        let columns = [
            ActualColumn {
                name: "a",
                logical_type: "INTEGER",
            },
            ActualColumn {
                name: "b",
                logical_type: "VARCHAR",
            },
        ];
        let cells = [
            ActualCell::Text("1"),
            ActualCell::Text("hello|world"),
            ActualCell::Text("2"),
            ActualCell::Text("say \"hi\""),
        ];
        let marker: &[&[u8]] = &[b"<FILE>:{EXPECTED}"];
        Oracle::new()
            .with_substitutions(&ReplaceRoot)
            .with_files(&resolver)
            .check_query(query(&columns, &cells, 2), expected(999, marker))
            .unwrap();
        let mut direct = expected(2, &[]);
        direct.values = ExpectedValues::File(b"expected.csv");
        Oracle::new()
            .with_files(&resolver)
            .check_query(query(&columns, &cells, 2), direct)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn external_expected_file_rejects_traversal_and_missing_file() {
        let root = temp_directory();
        let resolver = SourceRootFileResolver::new(&root).unwrap();
        assert!(
            resolver
                .load(b"../secret.csv", &["a"])
                .unwrap_err()
                .contains("escapes")
        );
        assert!(
            resolver
                .load(b"missing.csv", &["a"])
                .unwrap_err()
                .contains("cannot read")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn statement_error_matching_order_and_mutations() {
        Oracle::new()
            .check_statement(StatementResult::Success, ExpectedStatement::Success)
            .unwrap();
        let regular = StatementResult::Error(ActualError {
            message: b"Binder Error: missing column x",
            rendered: b"Error: Binder Error: missing column x\n",
            kind: ErrorKind::Regular,
        });
        Oracle::new()
            .check_statement(regular, ExpectedStatement::Error(Some(b"missing column")))
            .unwrap();
        assert!(matches!(
            Oracle::new()
                .check_statement(regular, ExpectedStatement::Error(Some(b"missing table"))),
            Err(OracleError::ErrorMessageMismatch { .. })
        ));
        assert_eq!(
            Oracle::new().check_statement(StatementResult::Success, ExpectedStatement::Error(None)),
            Err(OracleError::UnexpectedStatementSuccess)
        );
        Oracle::new()
            .check_statement(
                StatementResult::Success,
                ExpectedStatement::Unknown(Some(b"ignored")),
            )
            .unwrap();

        // Literal containment wins before regex compilation, as in C++.
        let literal_marker = StatementResult::Error(ActualError {
            message: b"literal <REGEX>:[ appears here",
            rendered: b"unused",
            kind: ErrorKind::Regular,
        });
        Oracle::new()
            .check_statement(literal_marker, ExpectedStatement::Error(Some(b"<REGEX>:[")))
            .unwrap();

        let regex_error = StatementResult::Error(ActualError {
            message: b"short diagnostic",
            rendered: b"Error: a needle here",
            kind: ErrorKind::Regular,
        });
        assert_eq!(
            Oracle::new().check_statement(
                regex_error,
                ExpectedStatement::Error(Some(b"<REGEX>:.*needle.*"))
            ),
            Err(OracleError::RegexCapabilityUnavailable)
        );
        Oracle::new()
            .with_regex(&StubRe2)
            .check_statement(
                regex_error,
                ExpectedStatement::Error(Some(b"<REGEX>:.*needle.*")),
            )
            .unwrap();
        assert!(matches!(
            Oracle::new().with_regex(&StubRe2).check_statement(
                regex_error,
                ExpectedStatement::Error(Some(b"<!REGEX>:.*needle.*")),
            ),
            Err(OracleError::ErrorMessageMismatch { .. })
        ));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn unsupported_internal_and_verification_errors_are_never_expected() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Internal,
            ErrorKind::Verification,
        ] {
            let actual = StatementResult::Error(ActualError {
                message: b"boom",
                rendered: b"boom",
                kind,
            });
            for expected in [
                ExpectedStatement::Error(None),
                ExpectedStatement::Unknown(None),
                ExpectedStatement::DontCare,
            ] {
                assert!(matches!(
                    Oracle::new().check_statement(actual, expected),
                    Err(OracleError::UnexpectedStatementError { .. })
                ));
            }
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn benchmark_checksum_is_deterministic_and_sensitive() {
        let columns = [ActualColumn {
            name: "a",
            logical_type: "INTEGER",
        }];
        let cells = [ActualCell::Text("2"), ActualCell::Text("1")];
        let actual = query(&columns, &cells, 2);
        let first = oracle_benchmark_checksum(actual, SortMode::Rows, false, 8).unwrap();
        let second = oracle_benchmark_checksum(actual, SortMode::Rows, false, 8).unwrap();
        assert_eq!(first, second);
        let changed_cells = [ActualCell::Text("3"), ActualCell::Text("1")];
        let changed =
            oracle_benchmark_checksum(query(&columns, &changed_cells, 2), SortMode::Rows, false, 8)
                .unwrap();
        assert_ne!(first, changed);
    }
}
