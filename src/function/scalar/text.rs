//! UTF-8 character-indexed VARCHAR slicing.
//!
//! DuckDB's `substring` operates on decoded characters, while the returned
//! value must retain the source's original UTF-8 bytes (including NULs).

use std::sync::Arc;

use super::super::{FunctionRegistry, ScalarFunction};
use crate::{
    common::{DataType, Error, Result, Value, type_registry::TypeRegistry},
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

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if !matches!(arguments.len(), 2 | 3) {
            return Err(Error::Internal(
                "substring argument count changed after binding".into(),
            ));
        }
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
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bound(value: &Value, name: &str) -> Result<i128> {
    let value = value.as_i128()?;
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
    // The source operates in character offsets. Keep byte boundaries beside
    // the decoded character positions so slicing retains arbitrary valid UTF-8
    // contents verbatim, including embedded NULs.
    let mut boundaries = Vec::with_capacity(input.chars().count() + 1);
    boundaries.extend(input.char_indices().map(|(offset, _)| offset));
    boundaries.push(input.len());
    let count = (boundaries.len() - 1) as i128;

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
        input[boundaries[begin]..boundaries[end]].to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::slice;

    #[test]
    fn character_offsets_retain_utf8_and_nuls() {
        assert_eq!(slice("é🦆x", 2, Some(1)), "🦆");
        assert_eq!(slice("a\0é", 2, Some(2)), "\0é");
        assert_eq!(slice("abcdef", 0, Some(3)), "ab");
        assert_eq!(slice("abcdef", 3, Some(-2)), "ab");
    }
}
