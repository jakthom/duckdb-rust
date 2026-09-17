//! utf8proc-backed extended grapheme VARCHAR functions.
//!
//! The byte ranges are found with utf8proc's stateful UAX#29 implementation,
//! but are sliced from the original string.  In particular this does not
//! normalize input and therefore preserves embedded NUL bytes exactly.

use std::sync::Arc;

use super::{BigintBatch, VarcharBatch, batch_dictionary_substring};
use crate::function::FunctionRegistry;
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
        let columns = arguments.columns();
        if self.0 == "length_grapheme"
            && let Some(input) = VarcharBatch::new(&columns[0])
        {
            return batch_length(input, arguments.len(), query).map(Some);
        }
        if self.0 == "substring_grapheme" {
            let length = match columns.len() {
                2 => Some(None),
                3 => BigintBatch::new(&columns[2], query)?.map(Some),
                _ => None,
            };
            let starts = BigintBatch::new(&columns[1], query)?;
            if let (Some(input), Some(starts), Some(length)) =
                (VarcharBatch::new(&columns[0]), starts, length)
            {
                return batch_substring(input, starts, length, arguments.len(), query).map(Some);
            }
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
            "length_grapheme" if arguments.len() == 1 => Ok(Value::Integer(grapheme_count(input))),
            "substring_grapheme" if matches!(arguments.len(), 2 | 3) => {
                let start = bound(&arguments[1], "offset")?;
                let length = arguments
                    .get(2)
                    .map(|value| bound(value, "length"))
                    .transpose()?;
                slice(input, start, length).map(Value::Varchar)
            }
            _ => Err(Error::Internal(
                "grapheme argument count changed after binding".into(),
            )),
        }
    }
}

fn batch_length(input: VarcharBatch<'_>, count: usize, query: &QueryContext) -> Result<Vector> {
    match input {
        VarcharBatch::Constant(value) => {
            query.check()?;
            Vector::constant(DataType::BigInt, grapheme_length_value(value)?, count)
        }
        VarcharBatch::Flat(values) => {
            let mut output = Vec::new();
            output.try_reserve_exact(count).map_err(|_| {
                Error::Resource("cannot allocate grapheme length result column".into())
            })?;
            for (index, value) in values.iter().enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                output.push(grapheme_length_value(value)?);
            }
            query.check()?;
            Vector::flat(DataType::BigInt, output)
        }
        VarcharBatch::Dictionary { values, selection } => {
            let mut output = Vec::new();
            output.try_reserve_exact(values.len()).map_err(|_| {
                Error::Resource("cannot allocate grapheme length dictionary values".into())
            })?;
            for (index, value) in values.iter().enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                output.push(grapheme_length_value(value)?);
            }
            let mut owned_selection = Vec::new();
            owned_selection
                .try_reserve_exact(selection.len())
                .map_err(|_| {
                    Error::Resource("cannot allocate grapheme length dictionary selection".into())
                })?;
            owned_selection.extend_from_slice(selection);
            query.check()?;
            Arc::new(Vector::flat(DataType::BigInt, output)?).select(owned_selection)
        }
    }
}

fn batch_substring(
    input: VarcharBatch<'_>,
    starts: BigintBatch<'_>,
    lengths: Option<BigintBatch<'_>>,
    count: usize,
    query: &QueryContext,
) -> Result<Vector> {
    if count == 0 {
        return Vector::flat(DataType::Varchar, Vec::new());
    }
    if let Some(output) = batch_dictionary_substring(
        input,
        starts,
        lengths,
        count,
        DataType::Varchar,
        flat_grapheme_substring_value,
        query,
    )? {
        return Ok(output);
    }

    let maximum_unique = (count / 8).clamp(1, 32);
    let mut keys = Vec::new();
    keys.try_reserve_exact(maximum_unique.saturating_add(1))
        .map_err(|_| Error::Resource("cannot allocate grapheme substring keys".into()))?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(maximum_unique.saturating_add(1))
        .map_err(|_| Error::Resource("cannot allocate grapheme substring values".into()))?;
    let mut selection = Vec::new();
    selection
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate grapheme substring selection".into()))?;
    let mut index = 0;
    while index < count {
        if index % 1024 == 0 {
            query.check()?;
        }
        let input_value = input.get(index)?;
        let start = starts.get(index)?;
        let length = lengths.map(|lengths| lengths.get(index)).transpose()?;
        let key = match input_value {
            Value::Varchar(text) => Some((text.as_str(), start, length)),
            Value::Null => None,
            _ => {
                return Err(Error::Internal(
                    "grapheme substring input encoding is not VARCHAR".into(),
                ));
            }
        };
        let entry = if let Some(entry) = keys.iter().position(|candidate| *candidate == key) {
            entry
        } else {
            let entry = values.len();
            keys.push(key);
            values.push(flat_grapheme_substring_value(input_value, start, length)?);
            entry
        };
        selection.push(entry);
        index += 1;
        if values.len() > maximum_unique {
            let mut output = Vec::new();
            output.try_reserve_exact(count).map_err(|_| {
                Error::Resource("cannot allocate grapheme substring result column".into())
            })?;
            output.extend(selection.iter().map(|&entry| values[entry].clone()));
            while index < count {
                if index % 1024 == 0 {
                    query.check()?;
                }
                output.push(flat_grapheme_substring_value(
                    input.get(index)?,
                    starts.get(index)?,
                    lengths.map(|lengths| lengths.get(index)).transpose()?,
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

fn grapheme_length_value(value: &Value) -> Result<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Varchar(input) => Ok(Value::Integer(grapheme_count(input))),
        _ => Err(Error::Internal(
            "grapheme length input encoding is not VARCHAR".into(),
        )),
    }
}

fn flat_grapheme_substring_value(input: &Value, start: i64, length: Option<i64>) -> Result<Value> {
    let Value::Varchar(input) = input else {
        return if input.is_null() {
            Ok(Value::Null)
        } else {
            Err(Error::Internal(
                "grapheme substring input encoding is not VARCHAR".into(),
            ))
        };
    };
    let start = bound_i128(i128::from(start), "offset")?;
    let length = length
        .map(|length| bound_i128(i128::from(length), "length"))
        .transpose()?;
    slice(input, start, length).map(Value::Varchar)
}

fn bound(value: &Value, name: &str) -> Result<i128> {
    bound_i128(value.as_i128()?, name)
}

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

fn grapheme_count(input: &str) -> i128 {
    if input.is_empty() {
        0
    } else {
        utf8proc::grapheme::grapheme_breaks(input).count() as i128 + 1
    }
}

fn slice(input: &str, start: i128, length: Option<i128>) -> Result<String> {
    let count = grapheme_count(input);
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
        return Ok(String::new());
    }
    let mut begin_offset = (begin == 0).then_some(0);
    let mut end_offset = (end == count as usize).then_some(input.len());
    for (ordinal, offset) in utf8proc::grapheme::grapheme_breaks(input).enumerate() {
        let ordinal = ordinal + 1;
        if ordinal == begin {
            begin_offset = Some(offset);
        }
        if ordinal == end {
            end_offset = Some(offset);
            break;
        }
    }
    let begin_offset =
        begin_offset.ok_or_else(|| Error::Internal("grapheme start boundary is missing".into()))?;
    let end_offset =
        end_offset.ok_or_else(|| Error::Internal("grapheme end boundary is missing".into()))?;
    let source = &input[begin_offset..end_offset];
    let mut output = String::new();
    output
        .try_reserve_exact(source.len())
        .map_err(|_| Error::Resource("cannot allocate grapheme substring result".into()))?;
    output.push_str(source);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::ScalarFunction;

    #[test]
    fn stateful_utf8proc_boundaries_cover_emoji_and_combining_marks() {
        let input = "e\u{301}👍🏽\u{200d}❤️\u{fe0f}x\0";
        assert_eq!(grapheme_count(input), 4);
        assert_eq!(slice(input, 1, Some(1)).unwrap(), "e\u{301}");
        assert_eq!(slice(input, 2, Some(1)).unwrap(), "👍🏽\u{200d}❤️\u{fe0f}");
        assert_eq!(slice(input, -1, None).unwrap(), "\0");
        assert_eq!(slice(input, 0, Some(2)).unwrap(), "e\u{301}");
        assert_eq!(
            slice(input, 3, Some(-2)).unwrap(),
            "e\u{301}👍🏽\u{200d}❤️\u{fe0f}"
        );
    }

    #[test]
    fn batch_evaluation_handles_flat_constant_dictionary_and_selected_inputs() -> Result<()> {
        let query = QueryContext::background();
        let substring = GraphemeFunction("substring_grapheme");
        let length = GraphemeFunction("length_grapheme");

        let flat = DataChunk::new(
            vec![
                Vector::flat(
                    DataType::Varchar,
                    vec![
                        Value::Varchar("e\u{301}👍🏽\u{200d}❤️\u{fe0f}x".into()),
                        Value::Null,
                    ],
                )?,
                Vector::flat(DataType::BigInt, vec![Value::Integer(2), Value::Integer(1)])?,
                Vector::flat(DataType::BigInt, vec![Value::Integer(1), Value::Integer(1)])?,
            ],
            2,
        )?;
        assert_eq!(
            substring
                .evaluate_batch(&flat, &query)?
                .expect("flat grapheme substring callback")
                .values()
                .collect::<Vec<_>>(),
            vec![Value::Varchar("👍🏽\u{200d}❤️\u{fe0f}".into()), Value::Null]
        );

        let constants = DataChunk::new(
            vec![Vector::constant(
                DataType::Varchar,
                Value::Varchar("e\u{301}x\0".into()),
                3,
            )?],
            3,
        )?;
        assert_eq!(
            length
                .evaluate_batch(&constants, &query)?
                .expect("constant grapheme length callback")
                .values()
                .collect::<Vec<_>>(),
            vec![Value::Integer(3); 3]
        );

        let strings = Arc::new(Vector::flat(
            DataType::Varchar,
            vec![
                Value::Varchar("e\u{301}x".into()),
                Value::Varchar("👍🏽\u{200d}❤️\u{fe0f}x".into()),
            ],
        )?);
        let starts = Arc::new(Vector::flat(
            DataType::BigInt,
            vec![Value::Integer(1), Value::Integer(-1)],
        )?);
        let dictionary = DataChunk::new(
            vec![
                strings.select(vec![1, 0, 1])?,
                starts.select(vec![1, 0, 1])?,
            ],
            3,
        )?;
        assert_eq!(
            substring
                .evaluate_batch(&dictionary, &query)?
                .expect("dictionary grapheme substring callback")
                .values()
                .collect::<Vec<_>>(),
            vec![
                Value::Varchar("x".into()),
                Value::Varchar("e\u{301}x".into()),
                Value::Varchar("x".into()),
            ]
        );

        let selected_source = DataChunk::new(
            vec![Vector::flat(
                DataType::Varchar,
                vec![
                    Value::Varchar("a\0".into()),
                    Value::Varchar("e\u{301}".into()),
                    Value::Varchar("👍🏽".into()),
                ],
            )?],
            3,
        )?;
        let selected = selected_source.select(&[2, 0, 2, 1])?;
        assert_eq!(
            length
                .evaluate_batch(&selected, &query)?
                .expect("selected grapheme length callback")
                .values()
                .collect::<Vec<_>>(),
            vec![
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(1),
                Value::Integer(1),
            ]
        );
        Ok(())
    }
}
