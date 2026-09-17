//! UTF-8 character-indexed VARCHAR slicing.
//!
//! DuckDB's `substring` operates on decoded characters, while the returned
//! value must retain the source's original UTF-8 bytes (including NULs).

use std::sync::Arc;

use super::super::{FunctionRegistry, ScalarFunction};
use crate::{
    common::{
        DataType, Error, Result, Value,
        type_registry::TypeRegistry,
        vector::{DataChunk, Vector},
    },
    parallel::QueryContext,
};

const MIN_BOUND: i128 = -4_294_967_296;
const MAX_BOUND: i128 = 4_294_967_295;

#[derive(Debug)]
struct Substring(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["substring", "substr"] {
        registry
            .register_scalar(Arc::new(Substring(name)))
            .expect("unique substring function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Substring {
    fn name(&self) -> &str {
        self.0
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if !matches!(arguments.len(), 2 | 3) {
            return Err(Error::Bind(format!(
                "{} requires a string, start and optional length",
                self.0
            )));
        }
        if !matches!(arguments[0], DataType::Varchar | DataType::Null) {
            return Err(Error::Bind(format!("{} requires a VARCHAR", self.0)));
        }
        let mut required = Vec::with_capacity(arguments.len());
        required.push(DataType::Varchar);
        required.extend(std::iter::repeat_n(DataType::BigInt, arguments.len() - 1));
        Ok(required)
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if matches!(
            arguments,
            [DataType::Varchar, DataType::BigInt]
                | [DataType::Varchar, DataType::BigInt, DataType::BigInt]
        ) {
            Ok(DataType::Varchar)
        } else {
            Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.0
            )))
        }
    }

    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        matches!(
            arguments,
            [DataType::Varchar, DataType::BigInt]
                | [DataType::Varchar, DataType::BigInt, DataType::BigInt]
        )
    }

    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        if !self.supports_batch_evaluation(
            &arguments
                .columns()
                .iter()
                .map(|column| column.data_type().clone())
                .collect::<Vec<_>>(),
        ) {
            return Ok(None);
        }
        let columns = arguments.columns();
        let length = match columns.len() {
            2 => Some(None),
            3 => columns[2].flat_bigints().map(Some),
            _ => None,
        };
        if let (Some(input), Some(starts), Some(length)) =
            (columns[0].flat_values(), columns[1].flat_bigints(), length)
        {
            return batch_flat_substring(input, starts, length, query).map(Some);
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate substring result column".into()))?;
        let mut row = Vec::with_capacity(arguments.columns().len());
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            arguments.read_row(index, &mut row)?;
            output.push(substring_value(&row)?);
        }
        query.check()?;
        Vector::flat(DataType::Varchar, output).map(Some)
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        substring_value(arguments)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_flat_substring(
    input: &[Value],
    starts: &[i64],
    lengths: Option<&[i64]>,
    query: &QueryContext,
) -> Result<Vector> {
    if input.len() != starts.len() || lengths.is_some_and(|lengths| lengths.len() != input.len()) {
        return Err(Error::Internal(
            "flat substring columns differ in cardinality".into(),
        ));
    }
    if input.is_empty() {
        return Vector::flat(DataType::Varchar, Vec::new());
    }
    // Keep the compact dictionary path only while repeated physical triples
    // dominate. A high-cardinality batch switches to a plain flat result
    // before the map can become a second per-row payload store.
    let maximum_unique = (input.len() / 8).clamp(1, 32);
    let mut keys = Vec::new();
    keys.try_reserve_exact(maximum_unique.saturating_add(1))
        .map_err(|_| Error::Resource("cannot allocate substring dictionary keys".into()))?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(maximum_unique.saturating_add(1))
        .map_err(|_| Error::Resource("cannot allocate substring dictionary values".into()))?;
    let mut selection = Vec::new();
    selection
        .try_reserve_exact(input.len())
        .map_err(|_| Error::Resource("cannot allocate substring dictionary selection".into()))?;
    let mut index = 0;
    while index < input.len() {
        if index % 1024 == 0 {
            query.check()?;
        }
        let length = lengths.map(|lengths| lengths[index]);
        let text = match &input[index] {
            Value::Null => None,
            Value::Varchar(text) => Some(text.as_str()),
            _ => {
                return Err(Error::Internal(
                    "flat substring input is not VARCHAR".into(),
                ));
            }
        };
        let key = (text, starts[index], length);
        // Typical SQL batches repeat a small number of physical triples. A
        // tiny linear cache avoids hashing short VARCHARs with a randomized
        // hasher, while the hard cap bounds high-cardinality prefix work.
        let entry = if let Some(entry) = keys.iter().position(|candidate| *candidate == key) {
            entry
        } else {
            let value = flat_substring_value(&input[index], starts[index], length)?;
            let entry = values.len();
            keys.push(key);
            values.push(value);
            entry
        };
        selection.push(entry);
        index += 1;
        if values.len() > maximum_unique {
            let mut output = Vec::new();
            output
                .try_reserve_exact(input.len())
                .map_err(|_| Error::Resource("cannot allocate substring result column".into()))?;
            output.extend(selection.iter().map(|&entry| values[entry].clone()));
            while index < input.len() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                output.push(flat_substring_value(
                    &input[index],
                    starts[index],
                    lengths.map(|lengths| lengths[index]),
                )?);
                index += 1;
            }
            query.check()?;
            return Vector::flat(DataType::Varchar, output);
        }
    }
    query.check()?;
    Arc::new(Vector::flat(DataType::Varchar, values)?).select(selection)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn substring_value(arguments: &[Value]) -> Result<Value> {
    if !matches!(arguments.len(), 2 | 3) {
        return Err(Error::Internal(
            "substring argument count changed after binding".into(),
        ));
    }
    // SQL NULL propagation precedes range validation. This preserves the
    // scalar result and avoids reporting an irrelevant bound error in a NULL
    // row while physical batches are evaluated speculatively.
    if arguments.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let Value::Varchar(input) = &arguments[0] else {
        return Err(Error::Internal("substring input is not VARCHAR".into()));
    };
    let start = bound(&arguments[1], "offset")?;
    let length = arguments
        .get(2)
        .map(|value| bound(value, "length"))
        .transpose()?;
    Ok(Value::Varchar(slice(input, start, length)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn flat_substring_value(input: &Value, start: i64, length: Option<i64>) -> Result<Value> {
    let Value::Varchar(input) = input else {
        return if input.is_null() {
            Ok(Value::Null)
        } else {
            Err(Error::Internal(
                "flat substring input is not VARCHAR".into(),
            ))
        };
    };
    let start = bound_i128(i128::from(start), "offset")?;
    let length = length
        .map(|length| bound_i128(i128::from(length), "length"))
        .transpose()?;
    Ok(Value::Varchar(slice(input, start, length)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bound(value: &Value, name: &str) -> Result<i128> {
    bound_i128(value.as_i128()?, name)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bound_i128(value: i128, name: &str) -> Result<i128> {
    if !(MIN_BOUND..=MAX_BOUND).contains(&value) {
        let direction = if value > MAX_BOUND { ">" } else { "<" };
        let boundary = if value > MAX_BOUND {
            MAX_BOUND
        } else {
            MIN_BOUND
        };
        return Err(Error::OutOfRange(format!(
            "Substring {name} outside of supported range ({direction} {boundary})"
        )));
    }
    Ok(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn slice(input: &str, start: i128, length: Option<i128>) -> String {
    // The source operates in character offsets. Count characters first, then
    // find the two requested byte offsets without materializing every UTF-8
    // boundary. This keeps column scans allocation-free until the resulting
    // VARCHAR itself is copied, while retaining embedded NUL bytes verbatim.
    let count = input.chars().count() as i128;

    // Positive starts are one-based. Zero and negative starts intentionally
    // retain their pre-clamp offset: `substring('abc', 0, 2)` is `a`.
    let raw_start = if start > 0 {
        start - 1
    } else if start == 0 {
        -1
    } else {
        count + start
    };
    let (raw_begin, raw_end) = match length {
        Some(length) if length < 0 => (raw_start + length, raw_start),
        Some(length) => (raw_start, raw_start + length),
        None => (raw_start, count),
    };
    let begin = raw_begin.clamp(0, count) as usize;
    let end = raw_end.clamp(0, count) as usize;
    if begin >= end {
        String::new()
    } else {
        let mut begin_byte = 0;
        let mut end_byte = input.len();
        for (index, (offset, _)) in input.char_indices().enumerate() {
            if index == begin {
                begin_byte = offset;
            }
            if index == end {
                end_byte = offset;
                break;
            }
        }
        input[begin_byte..end_byte].to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::vector::{DataChunk, Vector};

    #[test]
    fn character_offsets_retain_utf8_and_nuls() {
        assert_eq!(slice("é🦆x", 2, Some(1)), "🦆");
        assert_eq!(slice("a\0é", 2, Some(2)), "\0é");
        assert_eq!(slice("abcdef", 0, Some(3)), "ab");
        assert_eq!(slice("abcdef", 3, Some(-2)), "ab");
        let long = "é🦆".repeat(50_000);
        assert_eq!(slice(&long, 99_999, Some(2)), "é🦆");
    }

    #[test]
    fn substring_batch_handles_flat_constant_dictionary_and_first_range_error() -> Result<()> {
        let query = QueryContext::background();
        let function = Substring("substring");
        let flat = DataChunk::new(
            vec![
                Vector::flat(
                    DataType::Varchar,
                    vec![Value::Varchar("é🦆x".into()), Value::Null],
                )?,
                Vector::flat(DataType::BigInt, vec![Value::Integer(2), Value::Integer(1)])?,
            ],
            2,
        )?;
        let output = function
            .evaluate_batch(&flat, &query)?
            .expect("substring batch callback");
        assert_eq!(
            output.values().collect::<Vec<_>>(),
            vec![Value::Varchar("🦆x".into()), Value::Null]
        );

        let constants = DataChunk::new(
            vec![
                Vector::constant(DataType::Varchar, Value::Varchar("abcdef".into()), 2)?,
                Vector::constant(DataType::BigInt, Value::Integer(2), 2)?,
                Vector::constant(DataType::BigInt, Value::Integer(3), 2)?,
            ],
            2,
        )?;
        assert_eq!(
            function
                .evaluate_batch(&constants, &query)?
                .expect("constant substring batch callback")
                .values()
                .collect::<Vec<_>>(),
            vec![Value::Varchar("bcd".into()); 2]
        );

        let input = Arc::new(Vector::flat(
            DataType::Varchar,
            vec![
                Value::Varchar("abcdef".into()),
                Value::Varchar("é🦆x".into()),
            ],
        )?);
        let starts = Arc::new(Vector::flat(
            DataType::BigInt,
            vec![Value::Integer(2), Value::Integer(-2)],
        )?);
        let dictionary = DataChunk::new(
            vec![input.select(vec![1, 0, 1])?, starts.select(vec![1, 0, 1])?],
            3,
        )?;
        assert_eq!(
            function
                .evaluate_batch(&dictionary, &query)?
                .expect("dictionary substring batch callback")
                .values()
                .collect::<Vec<_>>(),
            vec![
                Value::Varchar("🦆x".into()),
                Value::Varchar("bcdef".into()),
                Value::Varchar("🦆x".into()),
            ]
        );

        let repeated = DataChunk::new(
            vec![
                Vector::flat(DataType::Varchar, vec![Value::Varchar("abcdef".into()); 16])?,
                Vector::flat(DataType::BigInt, vec![Value::Integer(2); 16])?,
                Vector::flat(DataType::BigInt, vec![Value::Integer(3); 16])?,
            ],
            16,
        )?;
        assert!(
            function
                .evaluate_batch(&repeated, &query)?
                .expect("low-cardinality substring batch callback")
                .dictionary()
                .is_some()
        );

        let distinct = DataChunk::new(
            vec![
                Vector::flat(
                    DataType::Varchar,
                    (0..16)
                        .map(|index| Value::Varchar(format!("value-{index}")))
                        .collect(),
                )?,
                Vector::flat(DataType::BigInt, vec![Value::Integer(1); 16])?,
            ],
            16,
        )?;
        assert!(
            function
                .evaluate_batch(&distinct, &query)?
                .expect("high-cardinality substring batch callback")
                .dictionary()
                .is_none()
        );

        let flat_source = Vector::flat(
            DataType::Varchar,
            vec![
                Value::Varchar("abcdef".into()),
                Value::Varchar("é🦆x".into()),
            ],
        )?;
        let flat_start =
            Vector::flat(DataType::BigInt, vec![Value::Integer(2), Value::Integer(2)])?;
        for (length, expected) in [
            (
                Vector::constant(DataType::BigInt, Value::Integer(1), 2)?,
                vec![Value::Varchar("b".into()), Value::Varchar("🦆".into())],
            ),
            (
                Vector::flat(DataType::BigInt, vec![Value::Integer(1), Value::Null])?,
                vec![Value::Varchar("b".into()), Value::Null],
            ),
            (
                Arc::new(Vector::flat(
                    DataType::BigInt,
                    vec![Value::Integer(1), Value::Integer(2)],
                )?)
                .select(vec![1, 0])?,
                vec![Value::Varchar("bc".into()), Value::Varchar("🦆".into())],
            ),
        ] {
            let output = function
                .evaluate_batch(
                    &DataChunk::new(vec![flat_source.clone(), flat_start.clone(), length], 2)?,
                    &query,
                )?
                .expect("non-flat length fallback");
            assert_eq!(output.values().collect::<Vec<_>>(), expected);
        }

        let error = DataChunk::new(
            vec![
                Vector::flat(
                    DataType::Varchar,
                    vec![Value::Varchar("abc".into()), Value::Varchar("abc".into())],
                )?,
                Vector::flat(
                    DataType::BigInt,
                    vec![Value::Integer(2), Value::Integer(4_294_967_296)],
                )?,
            ],
            2,
        )?;
        assert!(matches!(
            function.evaluate_batch(&error, &query),
            Err(Error::OutOfRange(message)) if message == "Substring offset outside of supported range (> 4294967295)"
        ));
        Ok(())
    }
}
