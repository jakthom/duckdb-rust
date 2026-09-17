//! UTF-8 character-indexed VARCHAR slicing.
//!
//! DuckDB's `substring` operates on decoded characters, while the returned
//! value must retain the source's original UTF-8 bytes (including NULs).

use std::sync::Arc;

use super::super::{FunctionRegistry, ScalarBatchKind, ScalarFunction};
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
        .ok_or_else(|| Error::Internal("substring VARCHAR encoding is out of bounds".into()))
    }

    fn physical_len(self) -> Option<usize> {
        match self {
            Self::Flat(_) => None,
            Self::Constant(_) => Some(1),
            Self::Dictionary { values, .. } => Some(values.len()),
        }
    }

    fn physical_index(self, index: usize) -> Result<usize> {
        match self {
            Self::Flat(_) => None,
            Self::Constant(_) => Some(0),
            Self::Dictionary { selection, .. } => selection.get(index).copied(),
        }
        .ok_or_else(|| Error::Internal("substring VARCHAR selection is out of bounds".into()))
    }

    fn physical_value(self, index: usize) -> Result<&'a Value> {
        match self {
            Self::Flat(_) => None,
            Self::Constant(value) => (index == 0).then_some(value),
            Self::Dictionary { values, .. } => values.get(index),
        }
        .ok_or_else(|| Error::Internal("substring VARCHAR parent is out of bounds".into()))
    }
}

#[derive(Clone, Copy)]
enum BigintBatch<'a> {
    Flat(&'a [i64]),
    SignedFlat(&'a Vector),
    Constant(i64),
    Dictionary {
        values: &'a [i64],
        selection: &'a [usize],
    },
    SignedDictionary {
        parent: &'a Vector,
        selection: &'a [usize],
    },
    ValueDictionary {
        values: &'a [Value],
        selection: &'a [usize],
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> BigintBatch<'a> {
    fn new(column: &'a Vector, query: &QueryContext) -> Result<Option<Self>> {
        if let Some(values) = column.flat_bigints() {
            return Ok(Some(Self::Flat(values)));
        }
        if let Some(Value::Integer(value)) = column.constant_value() {
            return Ok(i64::try_from(*value).ok().map(Self::Constant));
        }
        if let Some((parent, selection)) = column.dictionary() {
            if let Some(values) = parent.flat_bigints() {
                return Ok(Some(Self::Dictionary { values, selection }));
            }
            if parent.all_valid()
                && parent
                    .data_type()
                    .integer_bits()
                    .is_some_and(|bits| bits <= 64)
                && (parent.is_empty() || parent.flat_signed_i64_at(0).is_some())
            {
                return Ok(Some(Self::SignedDictionary { parent, selection }));
            }
            if let Some(values) = parent.flat_values() {
                for (offset, &selected) in selection.iter().enumerate() {
                    if offset % 1024 == 0 {
                        query.check()?;
                    }
                    match values.get(selected) {
                        Some(Value::Integer(value)) if i64::try_from(*value).is_ok() => {}
                        Some(Value::Null) => return Ok(None),
                        Some(_) => {
                            return Err(Error::Internal(
                                "substring BIGINT dictionary differs from its type".into(),
                            ));
                        }
                        None => {
                            return Err(Error::Internal(
                                "substring BIGINT selection is out of bounds".into(),
                            ));
                        }
                    }
                }
                query.check()?;
                return Ok(Some(Self::ValueDictionary { values, selection }));
            }
            return Ok(None);
        }
        if column.all_valid()
            && column
                .data_type()
                .integer_bits()
                .is_some_and(|bits| bits <= 64)
            && (column.is_empty() || column.flat_signed_i64_at(0).is_some())
        {
            return Ok(Some(Self::SignedFlat(column)));
        }
        Ok(None)
    }

    fn get(self, index: usize) -> Result<i64> {
        match self {
            Self::Flat(values) => values.get(index).copied(),
            Self::SignedFlat(values) => values.flat_signed_i64_at(index),
            Self::Constant(value) => Some(value),
            Self::Dictionary { values, selection } => selection
                .get(index)
                .and_then(|&selected| values.get(selected))
                .copied(),
            Self::SignedDictionary { parent, selection } => selection
                .get(index)
                .and_then(|&selected| parent.flat_signed_i64_at(selected)),
            Self::ValueDictionary { values, selection } => selection
                .get(index)
                .and_then(|&selected| values.get(selected))
                .and_then(|value| match value {
                    Value::Integer(value) => i64::try_from(*value).ok(),
                    _ => None,
                }),
        }
        .ok_or_else(|| Error::Internal("substring BIGINT encoding is out of bounds".into()))
    }

    fn physical_len(self) -> Option<usize> {
        match self {
            Self::Flat(_) => None,
            Self::SignedFlat(_) => None,
            Self::Constant(_) => Some(1),
            Self::Dictionary { values, .. } => Some(values.len()),
            Self::SignedDictionary { parent, .. } => Some(parent.len()),
            Self::ValueDictionary { values, .. } => Some(values.len()),
        }
    }

    fn physical_index(self, index: usize) -> Result<usize> {
        match self {
            Self::Flat(_) => None,
            Self::SignedFlat(_) => None,
            Self::Constant(_) => Some(0),
            Self::Dictionary { selection, .. }
            | Self::SignedDictionary { selection, .. }
            | Self::ValueDictionary { selection, .. } => selection.get(index).copied(),
        }
        .ok_or_else(|| Error::Internal("substring BIGINT selection is out of bounds".into()))
    }

    fn physical_value(self, index: usize) -> Result<i64> {
        match self {
            Self::Flat(_) => None,
            Self::SignedFlat(_) => None,
            Self::Constant(value) => (index == 0).then_some(value),
            Self::Dictionary { values, .. } => values.get(index).copied(),
            Self::SignedDictionary { parent, .. } => parent.flat_signed_i64_at(index),
            Self::ValueDictionary { values, .. } => {
                values.get(index).and_then(|value| match value {
                    Value::Integer(value) => i64::try_from(*value).ok(),
                    _ => None,
                })
            }
        }
        .ok_or_else(|| Error::Internal("substring BIGINT parent is out of bounds".into()))
    }

    fn dictionary_selection(self) -> Option<&'a [usize]> {
        match self {
            Self::Dictionary { selection, .. }
            | Self::SignedDictionary { selection, .. }
            | Self::ValueDictionary { selection, .. } => Some(selection),
            _ => None,
        }
    }
}

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

    fn batch_kind(&self) -> Option<ScalarBatchKind> {
        Some(ScalarBatchKind::Substring)
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
            3 => BigintBatch::new(&columns[2], query)?.map(Some),
            _ => None,
        };
        let starts = BigintBatch::new(&columns[1], query)?;
        if let (Some(input), Some(starts), Some(length)) =
            (VarcharBatch::new(&columns[0]), starts, length)
        {
            return batch_encoded_substring(input, starts, length, arguments.len(), query)
                .map(Some);
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
fn batch_encoded_substring(
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
        flat_substring_value,
        query,
    )? {
        return Ok(output);
    }
    // Keep the compact dictionary path only while repeated physical triples
    // dominate. A high-cardinality batch switches to a plain flat result
    // before the map can become a second per-row payload store.
    let maximum_unique = (count / 8).clamp(1, 32);
    let mut keys = Vec::new();
    keys.try_reserve_exact(maximum_unique.saturating_add(1))
        .map_err(|_| Error::Resource("cannot allocate substring dictionary keys".into()))?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(maximum_unique.saturating_add(1))
        .map_err(|_| Error::Resource("cannot allocate substring dictionary values".into()))?;
    let mut selection = Vec::new();
    selection
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate substring dictionary selection".into()))?;
    let mut index = 0;
    while index < count {
        if index % 1024 == 0 {
            query.check()?;
        }
        let length = lengths.map(|lengths| lengths.get(index)).transpose()?;
        let start = starts.get(index)?;
        let input_value = input.get(index)?;
        let text = match input_value {
            Value::Null => None,
            Value::Varchar(text) => Some(text.as_str()),
            _ => {
                return Err(Error::Internal(
                    "substring input encoding is not VARCHAR".into(),
                ));
            }
        };
        let key = (text, start, length);
        // Typical SQL batches repeat a small number of physical triples. A
        // tiny linear cache avoids hashing short VARCHARs with a randomized
        // hasher, while the hard cap bounds high-cardinality prefix work.
        let entry = if let Some(entry) = keys.iter().position(|candidate| *candidate == key) {
            entry
        } else {
            let value = flat_substring_value(input_value, start, length)?;
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
                .try_reserve_exact(count)
                .map_err(|_| Error::Resource("cannot allocate substring result column".into()))?;
            output.extend(selection.iter().map(|&entry| values[entry].clone()));
            while index < count {
                if index % 1024 == 0 {
                    query.check()?;
                }
                output.push(flat_substring_value(
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn substring_lengths_batch(
    arguments: &DataChunk,
    query: &QueryContext,
) -> Result<Vector> {
    let columns = arguments.columns();
    let length = match columns.len() {
        2 => Some(None),
        3 => BigintBatch::new(&columns[2], query)?.map(Some),
        _ => None,
    };
    let starts = match columns.get(1) {
        Some(column) => BigintBatch::new(column, query)?,
        None => None,
    };
    if let (Some(input), Some(starts), Some(length)) =
        (columns.first().and_then(VarcharBatch::new), starts, length)
    {
        return batch_encoded_substring_lengths(input, starts, length, arguments.len(), query);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(arguments.len())
        .map_err(|_| Error::Resource("cannot allocate substring length column".into()))?;
    let mut row = Vec::with_capacity(columns.len());
    for index in 0..arguments.len() {
        if index % 1024 == 0 {
            query.check()?;
        }
        arguments.read_row(index, &mut row)?;
        output.push(substring_length_value(&row)?);
    }
    query.check()?;
    Vector::flat(DataType::BigInt, output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_encoded_substring_lengths(
    input: VarcharBatch<'_>,
    starts: BigintBatch<'_>,
    lengths: Option<BigintBatch<'_>>,
    count: usize,
    query: &QueryContext,
) -> Result<Vector> {
    if count == 0 {
        return Vector::flat(DataType::BigInt, Vec::new());
    }
    if let Some(output) = batch_flat_substring_lengths(input, starts, lengths, count, query)? {
        return Ok(output);
    }
    if let Some(output) =
        batch_dictionary_substring_lengths_flat(input, starts, lengths, count, query)?
    {
        return Ok(output);
    }
    if let Some(output) = batch_dictionary_flat_bound_lengths(input, starts, lengths, count, query)?
    {
        return Ok(output);
    }
    if let Some(output) = batch_dictionary_substring(
        input,
        starts,
        lengths,
        count,
        DataType::BigInt,
        flat_substring_length_value,
        query,
    )? {
        return Ok(output);
    }
    let maximum_unique = (count / 8).clamp(1, 32);
    let mut keys = Vec::new();
    keys.try_reserve_exact(maximum_unique.saturating_add(1))
        .map_err(|_| Error::Resource("cannot allocate substring length keys".into()))?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(maximum_unique.saturating_add(1))
        .map_err(|_| Error::Resource("cannot allocate substring length values".into()))?;
    let mut selection = Vec::new();
    selection
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate substring length selection".into()))?;
    let mut index = 0;
    while index < count {
        if index % 1024 == 0 {
            query.check()?;
        }
        let length = lengths.map(|lengths| lengths.get(index)).transpose()?;
        let start = starts.get(index)?;
        let input_value = input.get(index)?;
        let text = match input_value {
            Value::Null => None,
            Value::Varchar(text) => Some(text.as_str()),
            _ => {
                return Err(Error::Internal(
                    "substring input encoding is not VARCHAR".into(),
                ));
            }
        };
        let key = (text, start, length);
        let entry = if let Some(entry) = keys.iter().position(|candidate| *candidate == key) {
            entry
        } else {
            let value = flat_substring_length_value(input_value, start, length)?;
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
                .try_reserve_exact(count)
                .map_err(|_| Error::Resource("cannot allocate substring length column".into()))?;
            output.extend(selection.iter().map(|&entry| values[entry].clone()));
            while index < count {
                if index % 1024 == 0 {
                    query.check()?;
                }
                output.push(flat_substring_length_value(
                    input.get(index)?,
                    starts.get(index)?,
                    lengths.map(|lengths| lengths.get(index)).transpose()?,
                )?);
                index += 1;
            }
            query.check()?;
            return Vector::flat(DataType::BigInt, output);
        }
    }
    query.check()?;
    Arc::new(Vector::flat(DataType::BigInt, values)?).select(selection)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_flat_substring_lengths(
    input: VarcharBatch<'_>,
    starts: BigintBatch<'_>,
    lengths: Option<BigintBatch<'_>>,
    count: usize,
    query: &QueryContext,
) -> Result<Option<Vector>> {
    let VarcharBatch::Flat(input) = input else {
        return Ok(None);
    };
    let Some(lengths) = lengths else {
        return Ok(None);
    };
    if input.len() != count {
        return Err(Error::Internal(
            "substring flat cardinality differs from batch".into(),
        ));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate substring length column".into()))?;
    for (index, value) in input.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        let Value::Varchar(text) = value else {
            if value.is_null() {
                return Ok(None);
            }
            return Err(Error::Internal(
                "substring input encoding is not VARCHAR".into(),
            ));
        };
        let start = bound_i128(i128::from(starts.get(index)?), "offset")?;
        let length = bound_i128(i128::from(lengths.get(index)?), "length")?;
        output.push(slice_length(text, start, Some(length)));
    }
    query.check()?;
    Ok(Some(Vector::bigints_prevalidated(output)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_dictionary_substring_lengths_flat(
    input: VarcharBatch<'_>,
    starts: BigintBatch<'_>,
    lengths: Option<BigintBatch<'_>>,
    count: usize,
    query: &QueryContext,
) -> Result<Option<Vector>> {
    const MAX_COMBINATIONS: usize = 4_096;
    let VarcharBatch::Dictionary {
        values: input_values,
        selection: input_selection,
    } = input
    else {
        return Ok(None);
    };
    let Some(start_selection) = starts.dictionary_selection() else {
        return Ok(None);
    };
    let Some(lengths) = lengths else {
        return Ok(None);
    };
    let Some(length_selection) = lengths.dictionary_selection() else {
        return Ok(None);
    };
    if input_selection.len() != count
        || start_selection.len() != count
        || length_selection.len() != count
    {
        return Err(Error::Internal(
            "substring dictionary cardinality differs from batch".into(),
        ));
    }
    let start_values = starts.physical_len().expect("dictionary start cardinality");
    let length_values = lengths
        .physical_len()
        .expect("dictionary length cardinality");
    let Some(combinations) = input_values
        .len()
        .checked_mul(start_values)
        .and_then(|value| value.checked_mul(length_values))
        .filter(|&value| value <= MAX_COMBINATIONS)
    else {
        return Ok(None);
    };
    for value in input_values {
        match value {
            Value::Varchar(_) => {}
            Value::Null => return Ok(None),
            _ => {
                return Err(Error::Internal(
                    "substring input encoding is not VARCHAR".into(),
                ));
            }
        }
    }
    let mut entries = vec![-1_i64; combinations];
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate substring length column".into()))?;
    for (index, ((&input_index, &start_index), &length_index)) in input_selection
        .iter()
        .zip(start_selection)
        .zip(length_selection)
        .enumerate()
    {
        if index % 1024 == 0 {
            query.check()?;
        }
        let combination = (input_index * start_values + start_index) * length_values + length_index;
        let entry = entries
            .get_mut(combination)
            .ok_or_else(|| Error::Internal("substring dictionary key is out of bounds".into()))?;
        if *entry < 0 {
            let Value::Varchar(text) = input_values.get(input_index).ok_or_else(|| {
                Error::Internal("substring VARCHAR parent is out of bounds".into())
            })?
            else {
                unreachable!("validated VARCHAR dictionary parent");
            };
            let start = bound_i128(i128::from(starts.physical_value(start_index)?), "offset")?;
            let length = bound_i128(i128::from(lengths.physical_value(length_index)?), "length")?;
            *entry = slice_length(text, start, Some(length));
        }
        output.push(*entry);
    }
    query.check()?;
    Ok(Some(Vector::bigints_prevalidated(output)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_dictionary_flat_bound_lengths(
    input: VarcharBatch<'_>,
    starts: BigintBatch<'_>,
    lengths: Option<BigintBatch<'_>>,
    count: usize,
    query: &QueryContext,
) -> Result<Option<Vector>> {
    let VarcharBatch::Dictionary {
        values: input_values,
        selection: input_selection,
    } = input
    else {
        return Ok(None);
    };
    let Some(lengths) = lengths else {
        return Ok(None);
    };
    if input_selection.len() != count {
        return Err(Error::Internal(
            "substring dictionary cardinality differs from batch".into(),
        ));
    }
    for value in input_values {
        match value {
            Value::Varchar(_) => {}
            Value::Null => return Ok(None),
            _ => {
                return Err(Error::Internal(
                    "substring input encoding is not VARCHAR".into(),
                ));
            }
        }
    }
    let mut keys = Vec::with_capacity(32);
    let mut values = Vec::with_capacity(32);
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate substring length column".into()))?;
    for (index, &input_index) in input_selection.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        let start = starts.get(index)?;
        let length = lengths.get(index)?;
        let key = (input_index, start, length);
        let entry = if let Some(entry) = keys.iter().position(|candidate| *candidate == key) {
            entry
        } else {
            if keys.len() == 32 {
                return Ok(None);
            }
            let Value::Varchar(text) = input_values.get(input_index).ok_or_else(|| {
                Error::Internal("substring VARCHAR parent is out of bounds".into())
            })?
            else {
                unreachable!("validated VARCHAR dictionary parent");
            };
            let start = bound_i128(i128::from(start), "offset")?;
            let length = bound_i128(i128::from(length), "length")?;
            let entry = values.len();
            keys.push(key);
            values.push(slice_length(text, start, Some(length)));
            entry
        };
        output.push(values[entry]);
    }
    query.check()?;
    Ok(Some(Vector::bigints_prevalidated(output)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_dictionary_substring(
    input: VarcharBatch<'_>,
    starts: BigintBatch<'_>,
    lengths: Option<BigintBatch<'_>>,
    count: usize,
    data_type: DataType,
    evaluate: fn(&Value, i64, Option<i64>) -> Result<Value>,
    query: &QueryContext,
) -> Result<Option<Vector>> {
    const MAX_COMBINATIONS: usize = 4_096;
    let Some(input_values) = input.physical_len() else {
        return Ok(None);
    };
    let Some(start_values) = starts.physical_len() else {
        return Ok(None);
    };
    let length_values = match lengths {
        Some(lengths) => {
            let Some(values) = lengths.physical_len() else {
                return Ok(None);
            };
            values
        }
        None => 1,
    };
    let Some(combinations) = input_values
        .checked_mul(start_values)
        .and_then(|value| value.checked_mul(length_values))
        .filter(|&value| value <= MAX_COMBINATIONS)
    else {
        return Ok(None);
    };
    let mut entries = vec![usize::MAX; combinations];
    let mut values = Vec::new();
    values
        .try_reserve_exact(combinations.min(count))
        .map_err(|_| Error::Resource("cannot allocate substring dictionary values".into()))?;
    let mut selection = Vec::new();
    selection
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate substring dictionary selection".into()))?;
    for index in 0..count {
        if index % 1024 == 0 {
            query.check()?;
        }
        let input_index = input.physical_index(index)?;
        let start_index = starts.physical_index(index)?;
        let length_index = lengths
            .map(|lengths| lengths.physical_index(index))
            .transpose()?
            .unwrap_or(0);
        let combination = input_index
            .checked_mul(start_values)
            .and_then(|value| value.checked_add(start_index))
            .and_then(|value| value.checked_mul(length_values))
            .and_then(|value| value.checked_add(length_index))
            .filter(|&value| value < combinations)
            .ok_or_else(|| Error::Internal("substring dictionary key is out of bounds".into()))?;
        let entry = if entries[combination] == usize::MAX {
            let value = evaluate(
                input.physical_value(input_index)?,
                starts.physical_value(start_index)?,
                lengths
                    .map(|lengths| lengths.physical_value(length_index))
                    .transpose()?,
            )?;
            let entry = values.len();
            entries[combination] = entry;
            values.push(value);
            entry
        } else {
            entries[combination]
        };
        selection.push(entry);
    }
    query.check()?;
    Arc::new(Vector::flat(data_type, values)?)
        .select(selection)
        .map(Some)
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
fn substring_length_value(arguments: &[Value]) -> Result<Value> {
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
    Ok(Value::Integer(i128::from(slice_length(
        input, start, length,
    ))))
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
fn flat_substring_length_value(input: &Value, start: i64, length: Option<i64>) -> Result<Value> {
    let Value::Varchar(input) = input else {
        return if input.is_null() {
            Ok(Value::Null)
        } else {
            Err(Error::Internal(
                "substring input encoding is not VARCHAR".into(),
            ))
        };
    };
    let start = bound_i128(i128::from(start), "offset")?;
    let length = length
        .map(|length| bound_i128(i128::from(length), "length"))
        .transpose()?;
    Ok(Value::Integer(i128::from(slice_length(
        input, start, length,
    ))))
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
    let (begin, end) = slice_character_range(input, start, length);

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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn slice_length(input: &str, start: i128, length: Option<i128>) -> i64 {
    let (begin, end) = slice_character_range(input, start, length);
    end.saturating_sub(begin) as i64
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn slice_character_range(input: &str, start: i128, length: Option<i128>) -> (usize, usize) {
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
    (begin, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::vector::{DataChunk, Vector};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn character_offsets_retain_utf8_and_nuls() {
        assert_eq!(slice("é🦆x", 2, Some(1)), "🦆");
        assert_eq!(slice("a\0é", 2, Some(2)), "\0é");
        assert_eq!(slice("abcdef", 0, Some(3)), "ab");
        assert_eq!(slice("abcdef", 3, Some(-2)), "ab");
        assert_eq!(slice_length("é🦆x", 2, Some(1)), 1);
        assert_eq!(slice_length("abcdef", 3, Some(-2)), 2);
        let long = "é🦆".repeat(50_000);
        assert_eq!(slice(&long, 99_999, Some(2)), "é🦆");
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

        let text = Arc::new(Vector::flat(
            DataType::Varchar,
            vec![Value::Varchar("abcdef".into())],
        )?);
        let nullable_bound = Arc::new(Vector::flat(
            DataType::BigInt,
            vec![Value::Null, Value::Integer(2)],
        )?);
        let length = Vector::constant(DataType::BigInt, Value::Integer(3), 2)?;
        let selected_non_null = DataChunk::new(
            vec![
                text.select(vec![0, 0])?,
                nullable_bound.select(vec![1, 1])?,
                length.clone(),
            ],
            2,
        )?;
        assert_eq!(
            substring_lengths_batch(&selected_non_null, &query)?
                .values()
                .collect::<Vec<_>>(),
            vec![Value::Integer(3), Value::Integer(3)]
        );
        let selected_null = DataChunk::new(
            vec![
                text.select(vec![0, 0])?,
                nullable_bound.select(vec![1, 0])?,
                length,
            ],
            2,
        )?;
        assert_eq!(
            substring_lengths_batch(&selected_null, &query)?
                .values()
                .collect::<Vec<_>>(),
            vec![Value::Integer(3), Value::Null]
        );
        Ok(())
    }
}
