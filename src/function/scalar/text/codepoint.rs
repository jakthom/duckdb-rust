//! NUL-safe VARCHAR codepoint primitives.
//!
//! These functions operate on Rust `String` values rather than C strings, so
//! embedded U+0000 is ordinary data throughout both scalar and vector paths.

use std::sync::Arc;

use crate::{
    common::{
        DataType, Error, Result, Value,
        type_registry::TypeRegistry,
        vector::{DataChunk, Vector},
    },
    function::{FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
};

#[derive(Debug)]
struct Chr;
#[derive(Debug)]
struct Ascii;
#[derive(Debug)]
struct Contains;
#[derive(Debug)]
struct StripAccents;
#[derive(Debug)]
struct NfcNormalize;

/// Borrow the common VARCHAR physical encodings so string kernels do not need
/// to clone their inputs through `Vector::value`. A nested dictionary or a
/// chunked input intentionally uses the owned compatibility path below; this
/// adapter only claims the encodings for which the borrowed representation is
/// directly available.
#[derive(Clone, Copy)]
enum VarcharBatch<'a> {
    Flat(&'a [Value]),
    Constant(&'a Value),
    Dictionary {
        values: &'a [Value],
        selection: &'a [usize],
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> VarcharBatch<'a> {
    fn new(column: &'a Vector) -> Option<Self> {
        if let Some(values) = column.flat_values() {
            return Some(Self::Flat(values));
        }
        if let Some(value) = column.constant_value() {
            return Some(Self::Constant(value));
        }
        let (parent, selection) = column.dictionary()?;
        Some(Self::Dictionary {
            values: parent.flat_values()?,
            selection,
        })
    }

    fn get(self, index: usize) -> Result<&'a Value> {
        match self {
            Self::Flat(values) => values.get(index),
            Self::Constant(value) => Some(value),
            Self::Dictionary { values, selection } => selection
                .get(index)
                .and_then(|&selected| values.get(selected)),
        }
        .ok_or_else(|| Error::Internal("VARCHAR codepoint encoding is out of bounds".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    registry
        .register_scalar(Arc::new(Chr))
        .expect("unique chr scalar function");
    registry
        .register_scalar(Arc::new(Ascii))
        .expect("unique ascii scalar function");
    // This is deliberately VARCHAR-only. Nested `contains` families retain
    // their own type-directed implementations rather than being stringified.
    registry
        .register_scalar(Arc::new(Contains))
        .expect("unique varchar contains scalar function");
    registry
        .register_scalar(Arc::new(StripAccents))
        .expect("unique strip_accents scalar function");
    registry
        .register_scalar(Arc::new(NfcNormalize))
        .expect("unique nfc_normalize scalar function");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unary_varchar_batch(
    arguments: &DataChunk,
    output_type: DataType,
    query: &QueryContext,
    apply: impl Fn(&Value) -> Result<Value> + Copy,
) -> Result<Option<Vector>> {
    let [column] = arguments.columns() else {
        return Ok(None);
    };
    if column.data_type() != &DataType::Varchar {
        return Ok(None);
    }
    if let Some(value) = column.constant_value() {
        return Vector::constant(output_type, apply(value)?, column.len()).map(Some);
    }
    if column.dictionary().is_some() {
        let mapped = column.map_dictionary_parent(output_type.clone(), |parent| {
            let mut output = Vec::new();
            output.try_reserve_exact(parent.len()).map_err(|_| {
                Error::Resource("cannot allocate VARCHAR codepoint dictionary parent".into())
            })?;
            if let Some(values) = parent.flat_values() {
                for (index, value) in values.iter().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    output.push(apply(value)?);
                }
            } else {
                for (index, value) in parent.values().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    output.push(apply(&value)?);
                }
            }
            Vector::flat(output_type.clone(), output)
        })?;
        query.check()?;
        return Ok(Some(mapped));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(arguments.len())
        .map_err(|_| Error::Resource("cannot allocate VARCHAR codepoint result column".into()))?;
    if let Some(values) = column.flat_values() {
        for (index, value) in values.iter().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            output.push(apply(value)?);
        }
    } else {
        for (index, value) in column.values().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            output.push(apply(&value)?);
        }
    }
    query.check()?;
    Vector::flat(output_type, output).map(Some)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn chr_value(value: &Value) -> Result<Value> {
    let Value::Integer(value) = value else {
        return if value.is_null() {
            Ok(Value::Null)
        } else {
            Err(Error::Internal("chr argument is not an INTEGER".into()))
        };
    };
    let codepoint = u32::try_from(*value)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(|| Error::InvalidInput(format!("Invalid UTF8 Codepoint {value}")))?;
    Ok(Value::Varchar(codepoint.to_string()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ascii_value(value: &Value) -> Result<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Varchar(value) => Ok(Value::Integer(i128::from(
            value.chars().next().map(u32::from).unwrap_or(0),
        ))),
        _ => Err(Error::Internal("ascii argument is not VARCHAR".into())),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn is_ascii(value: &str) -> bool {
    value.is_ascii()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn strip_accents_value(value: &Value) -> Result<Value> {
    let Value::Varchar(value) = value else {
        return if value.is_null() {
            Ok(Value::Null)
        } else {
            Err(Error::Internal(
                "strip_accents argument is not VARCHAR".into(),
            ))
        };
    };
    if is_ascii(value) {
        return Ok(Value::Varchar(value.clone()));
    }
    let decomposed =
        utf8proc::transform::normalize(value, utf8proc::transform::UnicodeNormalizationForm::NFD)
            .map_err(|error| Error::Resource(format!("utf8proc normalization failed: {error}")))?;
    Ok(Value::Varchar(
        decomposed
            .chars()
            .filter(|character| {
                utf8proc::properties::CharProperties::for_char(*character).major_category()
                    != utf8proc::properties::MajorCategory::Mark
            })
            .collect(),
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nfc_normalize_value(value: &Value) -> Result<Value> {
    let Value::Varchar(value) = value else {
        return if value.is_null() {
            Ok(Value::Null)
        } else {
            Err(Error::Internal(
                "nfc_normalize argument is not VARCHAR".into(),
            ))
        };
    };
    if is_ascii(value) {
        return Ok(Value::Varchar(value.clone()));
    }
    let mut options = utf8proc::transform::TransformOptions::default();
    options.composition = Some(utf8proc::transform::CompositionOptions::compose());
    options.stable = true;
    utf8proc::transform::map(value.as_str(), &options)
        .map(Value::Varchar)
        .map_err(|error| Error::Resource(format!("utf8proc normalization failed: {error}")))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn contains_value(arguments: &[Value]) -> Result<Value> {
    let [haystack, needle] = arguments else {
        return Err(Error::Internal("contains argument count".into()));
    };
    contains_values(haystack, needle)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn contains_values(haystack: &Value, needle: &Value) -> Result<Value> {
    contains_match(haystack, needle).map(|value| match value {
        Some(value) => Value::Boolean(value),
        None => Value::Null,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn contains_match(haystack: &Value, needle: &Value) -> Result<Option<bool>> {
    match (haystack, needle) {
        (Value::Null, _) | (_, Value::Null) => Ok(None),
        (Value::Varchar(haystack), Value::Varchar(needle)) => {
            Ok(Some(contains_text(haystack, needle)))
        }
        _ => Err(Error::Internal(
            "VARCHAR contains arguments are not VARCHAR".into(),
        )),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline(always)]
fn contains_text(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    match needle {
        [] => true,
        [byte] => haystack.contains(byte),
        [a, b] => haystack
            .windows(2)
            .any(|window| window[0] == *a && window[1] == *b),
        [a, b, c] => haystack
            .windows(3)
            .any(|window| window[0] == *a && window[1] == *b && window[2] == *c),
        [a, b, c, d] => haystack
            .windows(4)
            .any(|window| window[0] == *a && window[1] == *b && window[2] == *c && window[3] == *d),
        _ if needle.len() > haystack.len() => false,
        _ => haystack
            .windows(needle.len())
            .any(|window| window == needle),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_contains_matches(
    count: usize,
    query: &QueryContext,
    mut matched: impl FnMut(usize) -> Result<Option<bool>>,
) -> Result<Vector> {
    let mut common = None;
    let mut output: Option<Vec<Value>> = None;
    for index in 0..count {
        if index % 1024 == 0 {
            query.check()?;
        }
        let matched = matched(index)?;
        match (common, &mut output) {
            (None, _) => common = Some(matched),
            (Some(_), Some(output)) => output.push(match matched {
                Some(value) => Value::Boolean(value),
                None => Value::Null,
            }),
            (Some(common_value), None) if common_value != matched => {
                let mut result = Vec::new();
                result.try_reserve_exact(count).map_err(|_| {
                    Error::Resource("cannot allocate VARCHAR contains result column".into())
                })?;
                let common_value = match common_value {
                    Some(value) => Value::Boolean(value),
                    None => Value::Null,
                };
                result.extend(std::iter::repeat_n(common_value, index));
                result.push(match matched {
                    Some(value) => Value::Boolean(value),
                    None => Value::Null,
                });
                output = Some(result);
            }
            (Some(_), None) => {}
        }
    }
    query.check()?;
    if let Some(output) = output {
        return Vector::flat(DataType::Boolean, output);
    }
    match common {
        Some(Some(value)) => Vector::constant(DataType::Boolean, Value::Boolean(value), count),
        Some(None) => Vector::constant(DataType::Boolean, Value::Null, count),
        None => Vector::flat(DataType::Boolean, Vec::new()),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn varchar_arguments(name: &str, arguments: &[DataType], count: usize) -> Result<Vec<DataType>> {
    if arguments.len() != count
        || !arguments
            .iter()
            .all(|argument| matches!(argument, DataType::Varchar | DataType::Null))
    {
        return Err(Error::Bind(format!(
            "No function matches the given name and argument types '{name}({arguments:?})'"
        )));
    }
    Ok(vec![DataType::Varchar; count])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_contains(arguments: &DataChunk, query: &QueryContext) -> Result<Option<Vector>> {
    let [left, right] = arguments.columns() else {
        return Ok(None);
    };
    if left.data_type() != &DataType::Varchar || right.data_type() != &DataType::Varchar {
        return Ok(None);
    }
    if let (Some(left), Some(right)) = (VarcharBatch::new(left), VarcharBatch::new(right)) {
        if let (VarcharBatch::Constant(left), VarcharBatch::Constant(right)) = (left, right) {
            return Vector::constant(
                DataType::Boolean,
                contains_values(left, right)?,
                arguments.len(),
            )
            .map(Some);
        }
        if !matches!(left, VarcharBatch::Dictionary { .. })
            && !matches!(right, VarcharBatch::Dictionary { .. })
        {
            return batch_contains_matches(arguments.len(), query, |index| {
                contains_match(left.get(index)?, right.get(index)?)
            })
            .map(Some);
        }
        // Projection and CASE commonly produce a small set of physical string
        // pairs. Retain that repetition as a dictionary result and compare
        // borrowed string slices; high-cardinality batches switch promptly to
        // a plain Boolean column instead of growing a second cache.
        let maximum_unique = (arguments.len() / 8).clamp(1, 32);
        let mut keys = Vec::new();
        keys.try_reserve_exact(maximum_unique.saturating_add(1))
            .map_err(|_| Error::Resource("cannot allocate VARCHAR contains keys".into()))?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(maximum_unique.saturating_add(1))
            .map_err(|_| {
                Error::Resource("cannot allocate VARCHAR contains dictionary values".into())
            })?;
        let mut selection = Vec::new();
        selection.try_reserve_exact(arguments.len()).map_err(|_| {
            Error::Resource("cannot allocate VARCHAR contains dictionary selection".into())
        })?;
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let left_value = left.get(index)?;
            let right_value = right.get(index)?;
            let key = match (left_value, right_value) {
                (Value::Varchar(left), Value::Varchar(right)) => {
                    Some((left.as_str(), right.as_str()))
                }
                (Value::Null, _) | (_, Value::Null) => None,
                _ => {
                    return Err(Error::Internal(
                        "VARCHAR contains arguments are not VARCHAR".into(),
                    ));
                }
            };
            let entry = if let Some(entry) = keys.iter().position(|candidate| *candidate == key) {
                entry
            } else {
                let entry = values.len();
                keys.push(key);
                values.push(contains_values(left_value, right_value)?);
                entry
            };
            selection.push(entry);
            if values.len() > maximum_unique {
                let mut output = Vec::new();
                output.try_reserve_exact(arguments.len()).map_err(|_| {
                    Error::Resource("cannot allocate VARCHAR contains result column".into())
                })?;
                output.extend(selection.iter().map(|&entry| values[entry].clone()));
                for index in index + 1..arguments.len() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    output.push(contains_values(left.get(index)?, right.get(index)?)?);
                }
                query.check()?;
                return Vector::flat(DataType::Boolean, output).map(Some);
            }
        }
        query.check()?;
        if let Some(first) = values.first()
            && values.iter().all(|value| value == first)
        {
            return Vector::constant(DataType::Boolean, first.clone(), arguments.len()).map(Some);
        }
        return Arc::new(Vector::flat(DataType::Boolean, values)?)
            .select(selection)
            .map(Some);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(arguments.len())
        .map_err(|_| Error::Resource("cannot allocate VARCHAR contains result column".into()))?;
    for index in 0..arguments.len() {
        if index % 1024 == 0 {
            query.check()?;
        }
        let left = left.value(index).ok_or_else(|| {
            Error::Internal("VARCHAR contains left encoding is out of bounds".into())
        })?;
        let right = right.value(index).ok_or_else(|| {
            Error::Internal("VARCHAR contains right encoding is out of bounds".into())
        })?;
        output.push(contains_value(&[left, right])?);
    }
    query.check()?;
    Vector::flat(DataType::Boolean, output).map(Some)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn select_contains(arguments: &DataChunk, query: &QueryContext) -> Result<Vec<usize>> {
    let [left, right] = arguments.columns() else {
        return Err(Error::Internal(
            "VARCHAR contains selection argument count changed after binding".into(),
        ));
    };
    if left.data_type() != &DataType::Varchar || right.data_type() != &DataType::Varchar {
        return Err(Error::Internal(
            "VARCHAR contains selection types changed after binding".into(),
        ));
    }
    let mut selected = Vec::new();
    selected
        .try_reserve_exact(arguments.len())
        .map_err(|_| Error::Resource("cannot allocate VARCHAR contains selection".into()))?;
    if let (Some(left), Some(right)) = (VarcharBatch::new(left), VarcharBatch::new(right)) {
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            if contains_match(left.get(index)?, right.get(index)?)? == Some(true) {
                selected.push(index);
            }
        }
    } else {
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let left = left.value(index).ok_or_else(|| {
                Error::Internal("VARCHAR contains left selection is out of bounds".into())
            })?;
            let right = right.value(index).ok_or_else(|| {
                Error::Internal("VARCHAR contains right selection is out of bounds".into())
            })?;
            if contains_match(&left, &right)? == Some(true) {
                selected.push(index);
            }
        }
    }
    query.check()?;
    Ok(selected)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Ascii {
    fn name(&self) -> &str {
        "ascii"
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        varchar_arguments("ascii", arguments, 1)
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        varchar_arguments("ascii", arguments, 1)?;
        Ok(DataType::Integer)
    }
    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::Varchar]
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        unary_varchar_batch(arguments, DataType::Integer, query, ascii_value)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [value] = arguments else {
            return Err(Error::Internal("ascii argument count".into()));
        };
        ascii_value(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Contains {
    fn name(&self) -> &str {
        "contains"
    }
    fn batch_kind(
        &self,
        _: crate::function::ScalarBatchAccess,
    ) -> Option<crate::function::ScalarBatchKind> {
        Some(crate::function::ScalarBatchKind::builtin(
            crate::function::ScalarBatchIdentity::VarcharContains,
        ))
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if arguments == [DataType::Null, DataType::Null] {
            return Err(Error::Bind(
                "Could not choose a best candidate function for contains(NULL, NULL)".into(),
            ));
        }
        varchar_arguments("contains", arguments, 2)
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        varchar_arguments("contains", arguments, 2)?;
        Ok(DataType::Boolean)
    }
    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::Varchar, DataType::Varchar]
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        batch_contains(arguments, query)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        contains_value(arguments)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for StripAccents {
    fn name(&self) -> &str {
        "strip_accents"
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        varchar_arguments("strip_accents", arguments, 1)
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        varchar_arguments("strip_accents", arguments, 1)?;
        Ok(DataType::Varchar)
    }
    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::Varchar]
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        unary_varchar_batch(arguments, DataType::Varchar, query, strip_accents_value)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [value] = arguments else {
            return Err(Error::Internal("strip_accents argument count".into()));
        };
        strip_accents_value(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for NfcNormalize {
    fn name(&self) -> &str {
        "nfc_normalize"
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        varchar_arguments("nfc_normalize", arguments, 1)
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        varchar_arguments("nfc_normalize", arguments, 1)?;
        Ok(DataType::Varchar)
    }
    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::Varchar]
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        unary_varchar_batch(arguments, DataType::Varchar, query, nfc_normalize_value)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [value] = arguments else {
            return Err(Error::Internal("nfc_normalize argument count".into()));
        };
        nfc_normalize_value(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Chr {
    fn name(&self) -> &str {
        "chr"
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        match arguments {
            [argument] if argument.is_integer() || *argument == DataType::Null => {
                Ok(vec![DataType::Integer])
            }
            _ => Err(Error::Bind(format!(
                "No function matches the given name and argument types 'chr({arguments:?})'"
            ))),
        }
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        match arguments {
            [argument] if argument.is_integer() || *argument == DataType::Null => {}
            _ => {
                return Err(Error::Bind(format!(
                    "No function matches the given name and argument types 'chr({arguments:?})'"
                )));
            }
        }
        Ok(DataType::Varchar)
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::Integer]
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        let [column] = arguments.columns() else {
            return Ok(None);
        };
        if column.data_type() != &DataType::Integer {
            return Ok(None);
        }
        if let Some(value) = column.constant_value() {
            return Vector::constant(DataType::Varchar, chr_value(value)?, column.len()).map(Some);
        }
        if column.dictionary().is_some() {
            let mapped = column.map_dictionary_parent(DataType::Varchar, |parent| {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(parent.len())
                    .map_err(|_| Error::Resource("cannot allocate chr dictionary parent".into()))?;
                for (index, value) in parent.values().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    values.push(chr_value(&value)?);
                }
                Vector::flat(DataType::Varchar, values)
            })?;
            query.check()?;
            return Ok(Some(mapped));
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate chr result column".into()))?;
        for (index, value) in column.values().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            values.push(chr_value(&value)?);
        }
        query.check()?;
        Vector::flat(DataType::Varchar, values).map(Some)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [value] = arguments else {
            return Err(Error::Internal("chr argument count".into()));
        };
        chr_value(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn varchar(values: &[Option<&str>]) -> Result<Vector> {
        Vector::flat(
            DataType::Varchar,
            values
                .iter()
                .map(|value| match value {
                    Some(value) => Value::Varchar((*value).into()),
                    None => Value::Null,
                })
                .collect(),
        )
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn contains_selection_preserves_offsets_across_supported_and_fallback_encodings() -> Result<()>
    {
        let query = QueryContext::background();
        let haystacks = varchar(&[
            Some("abc"),
            Some("abc"),
            None,
            Some(""),
            Some("\0é"),
            Some("é"),
            Some("é"),
        ])?;
        let needles = varchar(&[
            Some("b"),
            Some("z"),
            Some("x"),
            Some(""),
            Some("\0"),
            Some("é"),
            Some("x"),
        ])?;
        let flat = DataChunk::new(vec![haystacks.clone(), needles.clone()], 7)?;
        assert_eq!(select_contains(&flat, &query)?, vec![0, 3, 4, 5]);

        let empty = Vector::constant(DataType::Varchar, Value::Varchar("".into()), 7)?;
        assert_eq!(
            select_contains(&DataChunk::new(vec![haystacks.clone(), empty], 7)?, &query)?,
            vec![0, 1, 3, 4, 5, 6]
        );

        let selection = vec![5, 2, 0, 3, 1, 4, 6];
        let direct = DataChunk::new(
            vec![
                Arc::new(haystacks.clone()).select(selection.clone())?,
                Arc::new(needles.clone()).select(selection.clone())?,
            ],
            7,
        )?;
        assert_eq!(select_contains(&direct, &query)?, vec![0, 2, 3, 5]);

        let nested_selection = vec![6, 0, 2, 1, 4, 3, 5];
        let nested = DataChunk::new(
            vec![
                Arc::new(direct.columns()[0].clone()).select(nested_selection.clone())?,
                Arc::new(direct.columns()[1].clone()).select(nested_selection)?,
            ],
            7,
        )?;
        assert_eq!(select_contains(&nested, &query)?, vec![1, 2, 5, 6]);

        let chunked_haystacks = Vector::chunked(
            DataType::Varchar,
            vec![haystacks.slice(0, 3)?, haystacks.slice(3, 4)?],
        )?;
        let chunked_needles = Vector::chunked(
            DataType::Varchar,
            vec![needles.slice(0, 2)?, needles.slice(2, 5)?],
        )?;
        let chunked = DataChunk::new(vec![chunked_haystacks, chunked_needles], 7)?;
        assert_eq!(select_contains(&chunked, &query)?, vec![0, 3, 4, 5]);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn codepoint_batches_cover_flat_constant_dictionary_selected_and_chunked_vectors() -> Result<()>
    {
        let query = QueryContext::background();

        let chr = Chr;
        let flat = DataChunk::new(
            vec![Vector::flat(
                DataType::Integer,
                vec![Value::Integer(0), Value::Integer(233), Value::Null],
            )?],
            3,
        )?;
        assert_eq!(
            chr.evaluate_batch(&flat, &query)?
                .expect("chr flat batch")
                .values()
                .collect::<Vec<_>>(),
            vec![
                Value::Varchar("\0".into()),
                Value::Varchar("é".into()),
                Value::Null,
            ]
        );
        let constant = DataChunk::new(
            vec![Vector::constant(DataType::Integer, Value::Integer(0), 3)?],
            3,
        )?;
        let result = chr
            .evaluate_batch(&constant, &query)?
            .expect("chr constant batch");
        assert!(result.constant_value().is_some());
        assert_eq!(
            result.values().collect::<Vec<_>>(),
            vec![Value::Varchar("\0".into()); 3]
        );
        let parent = Arc::new(Vector::flat(
            DataType::Integer,
            vec![Value::Integer(120), Value::Integer(0), Value::Null],
        )?);
        let dictionary = DataChunk::new(vec![parent.select(vec![1, 0, 2, 1])?], 4)?;
        let result = chr
            .evaluate_batch(&dictionary, &query)?
            .expect("chr dictionary batch");
        assert!(result.dictionary().is_some());
        assert_eq!(
            result.values().collect::<Vec<_>>(),
            vec![
                Value::Varchar("\0".into()),
                Value::Varchar("x".into()),
                Value::Null,
                Value::Varchar("\0".into()),
            ]
        );

        let ascii = Ascii;
        let strings = Arc::new(Vector::flat(
            DataType::Varchar,
            vec![
                Value::Varchar("é".into()),
                Value::Varchar("\0x".into()),
                Value::Varchar("".into()),
                Value::Null,
            ],
        )?);
        let selected = DataChunk::new(vec![strings.select(vec![1, 0, 2, 3, 1])?], 5)?;
        let result = ascii
            .evaluate_batch(&selected, &query)?
            .expect("ascii selected batch");
        assert!(result.dictionary().is_some());
        assert_eq!(
            result.values().collect::<Vec<_>>(),
            vec![
                Value::Integer(0),
                Value::Integer(233),
                Value::Integer(0),
                Value::Null,
                Value::Integer(0),
            ]
        );

        let contains = Contains;
        let constant_contains = DataChunk::new(
            vec![
                Vector::constant(DataType::Varchar, Value::Varchar("a\0b".into()), 4)?,
                Vector::constant(DataType::Varchar, Value::Varchar("\0".into()), 4)?,
            ],
            4,
        )?;
        let result = contains
            .evaluate_batch(&constant_contains, &query)?
            .expect("contains constant batch");
        assert!(result.constant_value().is_some());
        assert_eq!(
            result.values().collect::<Vec<_>>(),
            vec![Value::Boolean(true); 4]
        );
        let dictionary_parent = Arc::new(Vector::flat(
            DataType::Varchar,
            vec![Value::Varchar("a\0b".into()), Value::Varchar("xyz".into())],
        )?);
        let dictionary =
            dictionary_parent.select(vec![0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1])?;
        let needles = Vector::constant(DataType::Varchar, Value::Varchar("\0".into()), 16)?;
        let result = contains
            .evaluate_batch(&DataChunk::new(vec![dictionary, needles], 16)?, &query)?
            .expect("contains dictionary batch");
        assert!(result.dictionary().is_some());
        assert_eq!(
            result.values().collect::<Vec<_>>(),
            vec![
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
            ]
        );
        let chunked = Vector::concatenate(
            DataType::Varchar,
            &[
                Vector::flat(
                    DataType::Varchar,
                    vec![Value::Varchar("a\0b".into()), Value::Null],
                )?,
                Vector::flat(
                    DataType::Varchar,
                    vec![Value::Varchar("éx".into()), Value::Varchar("".into())],
                )?,
            ],
        )?;
        let needles = Vector::flat(
            DataType::Varchar,
            vec![
                Value::Varchar("\0".into()),
                Value::Varchar("x".into()),
                Value::Varchar("é".into()),
                Value::Varchar("".into()),
            ],
        )?;
        let chunked = DataChunk::new(vec![chunked, needles], 4)?;
        assert_eq!(
            contains
                .evaluate_batch(&chunked, &query)?
                .expect("contains chunked batch")
                .values()
                .collect::<Vec<_>>(),
            vec![
                Value::Boolean(true),
                Value::Null,
                Value::Boolean(true),
                Value::Boolean(true),
            ]
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn unicode_normalization_batches_preserve_varchar_encodings_and_nuls() -> Result<()> {
        let query = QueryContext::background();
        let values = Arc::new(Vector::flat(
            DataType::Varchar,
            vec![
                Value::Varchar("a\0é".into()),
                Value::Varchar("é".into()),
                Value::Null,
            ],
        )?);
        let strip = StripAccents;
        let nfc = NfcNormalize;
        let flat = DataChunk::new(vec![values.as_ref().clone()], 3)?;
        assert_eq!(
            strip
                .evaluate_batch(&flat, &query)?
                .expect("strip flat batch")
                .values()
                .collect::<Vec<_>>(),
            vec![
                Value::Varchar("a\0e".into()),
                Value::Varchar("e".into()),
                Value::Null,
            ]
        );
        let constant = DataChunk::new(
            vec![Vector::constant(
                DataType::Varchar,
                Value::Varchar("é".into()),
                3,
            )?],
            3,
        )?;
        let result = nfc
            .evaluate_batch(&constant, &query)?
            .expect("nfc constant batch");
        assert!(result.constant_value().is_some());
        assert_eq!(
            result.values().collect::<Vec<_>>(),
            vec![Value::Varchar("é".into()); 3]
        );
        let dictionary = DataChunk::new(vec![values.select(vec![1, 0, 2, 1])?], 4)?;
        let result = nfc
            .evaluate_batch(&dictionary, &query)?
            .expect("nfc dictionary batch");
        assert!(result.dictionary().is_some());
        assert_eq!(
            result.values().collect::<Vec<_>>(),
            vec![
                Value::Varchar("é".into()),
                Value::Varchar("a\0é".into()),
                Value::Null,
                Value::Varchar("é".into()),
            ]
        );
        let chunked = Vector::chunked(
            DataType::Varchar,
            vec![values.slice(0, 2)?, values.slice(2, 1)?],
        )?;
        assert_eq!(
            strip
                .evaluate_batch(&DataChunk::new(vec![chunked], 3)?, &query)?
                .expect("strip chunked batch")
                .values()
                .collect::<Vec<_>>(),
            vec![
                Value::Varchar("a\0e".into()),
                Value::Varchar("e".into()),
                Value::Null,
            ]
        );
        Ok(())
    }
}
