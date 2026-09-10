use super::super::{
    binary::{Reader, corrupt, u32_at},
    catalog::logical_type,
    primitive,
};
use crate::{
    common::{DataType, Error, Result, Row, Value},
    parallel::QueryContext,
};

pub(super) struct Chunk {
    pub types: Vec<DataType>,
    pub rows: Vec<Row>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(reader: &mut Reader, context: &QueryContext) -> Result<Chunk> {
    reader.field(100)?;
    let count = reader.length()?;
    if count == 0 || count > 65536 {
        return Err(corrupt("WAL chunk cardinality out of range"));
    }
    context.check_rows(count)?;
    reader.field(101)?;
    let columns = reader.length()?;
    if columns == 0 || columns > 16384 || count * columns > 16_777_216 {
        return Err(Error::Resource(
            "WAL chunk exceeds column or cell limit".into(),
        ));
    }
    let types = (0..columns)
        .map(|_| logical_type(reader))
        .collect::<Result<Vec<_>>>()?;
    reader.field(102)?;
    if reader.length()? != columns {
        return Err(corrupt("WAL chunk column count mismatch"));
    }
    let mut rows = vec![vec![Value::Null; columns]; count];
    let mut remaining = super::nested::MAX_CELLS;
    for (column, data_type) in types.iter().enumerate() {
        let values = vector(reader, data_type, count, 0, &mut remaining, context)?;
        reader.end()?;
        for (row, value) in rows.iter_mut().zip(values) {
            row[column] = value;
        }
    }
    reader.end()?;
    Ok(Chunk { types, rows })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn vector(
    reader: &mut Reader,
    data_type: &DataType,
    count: usize,
    depth: usize,
    remaining: &mut usize,
    context: &QueryContext,
) -> Result<Vec<Value>> {
    super::nested::charge(remaining, count, context)?;
    if depth > 64 {
        return Err(Error::Resource("WAL vector nesting exceeds 64".into()));
    }
    match reader.optional_unsigned(90, 0)? {
        0 => flat(reader, data_type, count, depth, remaining, context),
        2 => {
            let value = vector(reader, data_type, 1, depth + 1, remaining, context)?.remove(0);
            Ok(vec![value; count])
        }
        3 => {
            reader.field(91)?;
            let selection = reader.blob()?;
            if selection.len() != count * 4 {
                return Err(corrupt("WAL dictionary selection length"));
            }
            reader.field(92)?;
            let size = reader.length()?;
            if size == 0 || size > 65536 {
                return Err(corrupt("WAL dictionary size"));
            }
            let dictionary = vector(reader, data_type, size, depth + 1, remaining, context)?;
            (0..count)
                .map(|i| {
                    context.check()?;
                    dictionary
                        .get(u32_at(&selection, i * 4)? as usize)
                        .cloned()
                        .ok_or_else(|| corrupt("WAL dictionary index out of bounds"))
                })
                .collect()
        }
        4 => {
            if !matches!(
                data_type,
                DataType::TinyInt
                    | DataType::SmallInt
                    | DataType::Integer
                    | DataType::BigInt
                    | DataType::HugeInt
            ) {
                return Err(Error::Unsupported(format!("WAL sequence of {data_type}")));
            }
            reader.field(91)?;
            let start = i128::from(reader.signed()?);
            reader.field(92)?;
            let increment = i128::from(reader.signed()?);
            let bound = context.types().bind(data_type)?;
            (0..count)
                .map(|i| {
                    context.check()?;
                    let value = Value::Integer(start + increment * i as i128);
                    bound
                        .validate(&value, context)
                        .map_err(super::recovery_error)?;
                    Ok(value)
                })
                .collect()
        }
        kind => Err(Error::Unsupported(format!(
            "DuckDB WAL vector encoding {kind}"
        ))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn flat(
    reader: &mut Reader,
    data_type: &DataType,
    count: usize,
    depth: usize,
    remaining: &mut usize,
    context: &QueryContext,
) -> Result<Vec<Value>> {
    reader.field(100)?;
    let validity = if reader.boolean()? {
        reader.field(101)?;
        let mask = reader.blob()?;
        if mask.len() != count.div_ceil(64) * 8 {
            return Err(corrupt("WAL validity mask length"));
        }
        Some(mask)
    } else {
        None
    };
    let valid = |i: usize| {
        validity
            .as_ref()
            .is_none_or(|mask| mask[i / 8] & (1 << (i % 8)) != 0)
    };
    if matches!(data_type, DataType::Nested(_)) {
        return super::nested::read(
            reader,
            data_type,
            count,
            validity.as_deref(),
            depth,
            remaining,
            context,
        );
    }
    if matches!(
        data_type,
        DataType::Varchar | DataType::Blob | DataType::Bit
    ) {
        if reader.optional(107)? && reader.boolean()? {
            let byte_count = usize::try_from(reader.unsigned()?)
                .map_err(|_| corrupt("WAL string byte count overflow"))?;
            if byte_count > 16 * 1024 * 1024 {
                return Err(Error::Resource("WAL string vector exceeds 16 MiB".into()));
            }
            reader.field(108)?;
            let lengths = reader.blob()?;
            if lengths.len() != count * 4 {
                return Err(corrupt("WAL string length vector mismatch"));
            }
            reader.field(109)?;
            let bytes = reader.blob()?;
            if bytes.len() != byte_count {
                return Err(corrupt("WAL string byte vector mismatch"));
            }
            let mut offset = 0usize;
            let mut values = Vec::with_capacity(count);
            for i in 0..count {
                context.check()?;
                let length = u32_at(&lengths, i * 4)? as usize;
                if !valid(i) {
                    if length != 0 {
                        return Err(corrupt("WAL NULL string has nonzero length"));
                    }
                    values.push(Value::Null);
                    continue;
                }
                let end = offset
                    .checked_add(length)
                    .ok_or_else(|| corrupt("WAL string offset overflow"))?;
                let value = bytes
                    .get(offset..end)
                    .ok_or_else(|| corrupt("WAL string outside byte vector"))?;
                values.push(string_value(value.to_vec(), data_type, context)?);
                offset = end;
            }
            if offset != byte_count {
                return Err(corrupt("WAL string vector has unused bytes"));
            }
            return Ok(values);
        }
        reader.field(102)?;
        if reader.length()? != count {
            return Err(corrupt("WAL string count mismatch"));
        }
        (0..count)
            .map(|i| {
                context.check()?;
                let bytes = reader.blob()?;
                if valid(i) {
                    string_value(bytes, data_type, context)
                } else {
                    Ok(Value::Null)
                }
            })
            .collect()
    } else {
        reader.field(102)?;
        let width = primitive::width(data_type)?;
        let bytes = reader.blob()?;
        if bytes.len() != width * count {
            return Err(corrupt("WAL fixed-width column length"));
        }
        (0..count)
            .map(|i| {
                context.check()?;
                if !valid(i) {
                    return Ok(Value::Null);
                }
                let value = primitive::scalar(&bytes, i * width, data_type)?;
                if value.is_null() {
                    return Err(corrupt("WAL non-NULL column contains reserved sentinel"));
                }
                Ok(value)
            })
            .collect()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn string_value(bytes: Vec<u8>, data_type: &DataType, context: &QueryContext) -> Result<Value> {
    match data_type {
        DataType::Blob => Ok(Value::Blob(bytes)),
        DataType::Bit => crate::common::BitString::from_native(&bytes, || context.check())
            .map(crate::common::BitString::value),
        DataType::Varchar => String::from_utf8(bytes)
            .map(Value::Varchar)
            .map_err(|_| corrupt("invalid WAL string UTF-8")),
        _ => Err(corrupt("WAL string logical type")),
    }
}
