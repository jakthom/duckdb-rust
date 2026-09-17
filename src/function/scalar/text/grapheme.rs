//! utf8proc-backed extended grapheme VARCHAR functions.
//!
//! The byte ranges are found with utf8proc's stateful UAX#29 implementation,
//! but are sliced from the original string.  In particular this does not
//! normalize input and therefore preserves embedded NUL bytes exactly.

use std::sync::Arc;

use super::super::super::FunctionRegistry;
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
struct GraphemeFunction(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["substring_grapheme", "length_grapheme"] {
        registry
            .register_scalar(Arc::new(GraphemeFunction(name)))
            .expect("unique grapheme function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl crate::function::ScalarFunction for GraphemeFunction {
    fn name(&self) -> &str {
        self.0
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        match self.0 {
            "length_grapheme"
                if arguments.len() == 1
                    && matches!(arguments[0], DataType::Varchar | DataType::Null) =>
            {
                Ok(vec![DataType::Varchar])
            }
            "substring_grapheme" if matches!(arguments.len(), 2 | 3) => {
                if !matches!(arguments[0], DataType::Varchar | DataType::Null) {
                    return Err(Error::Bind("substring_grapheme requires a VARCHAR".into()));
                }
                let mut required = Vec::with_capacity(arguments.len());
                required.push(DataType::Varchar);
                required.extend(std::iter::repeat_n(DataType::BigInt, arguments.len() - 1));
                Ok(required)
            }
            "length_grapheme" => Err(Error::Bind("length_grapheme requires a VARCHAR".into())),
            _ => Err(Error::Bind(
                "substring_grapheme requires a string, start and optional length".into(),
            )),
        }
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        match (self.0, arguments) {
            ("length_grapheme", [DataType::Varchar]) => Ok(DataType::BigInt),
            ("substring_grapheme", [DataType::Varchar, DataType::BigInt])
            | ("substring_grapheme", [DataType::Varchar, DataType::BigInt, DataType::BigInt]) => {
                Ok(DataType::Varchar)
            }
            _ => Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.0
            ))),
        }
    }

    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }

    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        matches!(
            (self.0, arguments),
            ("length_grapheme", [DataType::Varchar])
                | ("substring_grapheme", [DataType::Varchar, DataType::BigInt])
                | (
                    "substring_grapheme",
                    [DataType::Varchar, DataType::BigInt, DataType::BigInt]
                )
        )
    }

    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        let types = arguments
            .columns()
            .iter()
            .map(|column| column.data_type().clone())
            .collect::<Vec<_>>();
        if !self.supports_batch_evaluation(&types) {
            return Ok(None);
        }
        let mut output = Vec::new();
        output.try_reserve_exact(arguments.len()).map_err(|_| {
            Error::Resource("cannot allocate grapheme function result column".into())
        })?;
        let mut row = Vec::with_capacity(arguments.columns().len());
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            arguments.read_row(index, &mut row)?;
            output.push(self.evaluate_value(&row)?);
        }
        query.check()?;
        Vector::flat(self.result_type(), output).map(Some)
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.evaluate_value(arguments)
    }
}

impl GraphemeFunction {
    fn result_type(&self) -> DataType {
        match self.0 {
            "length_grapheme" => DataType::BigInt,
            _ => DataType::Varchar,
        }
    }

    fn evaluate_value(&self, arguments: &[Value]) -> Result<Value> {
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let Value::Varchar(input) = arguments.first().ok_or_else(|| {
            Error::Internal("grapheme argument count changed after binding".into())
        })?
        else {
            return Err(Error::Internal("grapheme input is not VARCHAR".into()));
        };
        match self.0 {
            "length_grapheme" if arguments.len() == 1 => {
                Ok(Value::Integer(grapheme_boundaries(input).len() as i128 - 1))
            }
            "substring_grapheme" if matches!(arguments.len(), 2 | 3) => {
                let start = bound(&arguments[1], "offset")?;
                let length = arguments
                    .get(2)
                    .map(|value| bound(value, "length"))
                    .transpose()?;
                Ok(Value::Varchar(slice(input, start, length)))
            }
            _ => Err(Error::Internal(
                "grapheme argument count changed after binding".into(),
            )),
        }
    }
}

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

/// Every boundary, including zero and the terminal byte length.
fn grapheme_boundaries(input: &str) -> Vec<usize> {
    let mut boundaries = Vec::with_capacity(input.chars().count().saturating_add(1));
    boundaries.push(0);
    let mut chars = input.char_indices();
    let Some((_, mut previous)) = chars.next() else {
        return boundaries;
    };
    let mut state = 0;
    for (offset, current) in chars {
        // SAFETY: utf8proc receives valid Unicode scalar values and a valid
        // mutable state pointer.  Calls are strictly source order.
        let breaks = unsafe {
            utf8proc_sys::utf8proc_grapheme_break_stateful(
                previous as i32,
                current as i32,
                &mut state,
            )
        } != 0;
        if breaks {
            boundaries.push(offset);
            state = 0;
        }
        previous = current;
    }
    boundaries.push(input.len());
    boundaries
}

fn slice(input: &str, start: i128, length: Option<i128>) -> String {
    let boundaries = grapheme_boundaries(input);
    let count = (boundaries.len() - 1) as i128;
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
    use super::*;

    #[test]
    fn stateful_utf8proc_boundaries_cover_emoji_and_combining_marks() {
        let input = "e\u{301}👍🏽\u{200d}❤️\u{fe0f}x\0";
        assert_eq!(grapheme_boundaries(input).len() - 1, 4);
        assert_eq!(slice(input, 1, Some(1)), "e\u{301}");
        assert_eq!(slice(input, 2, Some(1)), "👍🏽\u{200d}❤️\u{fe0f}");
        assert_eq!(slice(input, -1, None), "\0");
        assert_eq!(slice(input, 0, Some(2)), "e\u{301}");
        assert_eq!(slice(input, 3, Some(-2)), "e\u{301}");
    }
}
