//! Development DICT_FSST combines a forward string pool, optional FSST symbol
//! table, packed dictionary-entry lengths and optional packed row indices.
use super::super::binary::{corrupt, u32_at};
use super::{packed, strings};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn aligned_end(start: usize, size: usize) -> Result<usize> {
    start
        .checked_add(size)
        .and_then(|end| end.checked_add(7))
        .map(|end| end & !7)
        .ok_or_else(|| corrupt("DICT_FSST offset overflow"))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn decode(
    data: &[u8],
    count: usize,
    query: &QueryContext,
    data_type: &DataType,
) -> Result<Vec<Value>> {
    query.check_rows(count)?;
    let dictionary_size = u32_at(data, 0)? as usize;
    let dictionary_count = u32_at(data, 4)? as usize;
    let symbol_size = u32_at(data, 12)? as usize;
    let mode = data[8];
    let length_width = usize::from(data[9]);
    let index_width = usize::from(data[10]);
    if mode > 2
        || dictionary_count == 0
        || dictionary_count > count.saturating_add(1)
        || length_width > 32
        || index_width > 32
        || (mode == 0 && symbol_size != 0)
        || (mode != 0 && symbol_size < 17)
    {
        return Err(corrupt("invalid DICT_FSST header"));
    }
    let expected_index_width = if mode == 2 {
        0
    } else {
        (usize::BITS - (dictionary_count - 1).leading_zeros()) as usize
    };
    if index_width != expected_index_width
        || (mode == 2 && dictionary_count != count.saturating_add(1))
    {
        return Err(corrupt("invalid DICT_FSST dictionary shape"));
    }
    let symbol_start = aligned_end(16, dictionary_size)?;
    let lengths_start = aligned_end(symbol_start, symbol_size)?;
    let lengths_size = packed::byte_count(dictionary_count, length_width)?;
    let indices_start = aligned_end(lengths_start, lengths_size)?;
    let indices_size = packed::byte_count(count, index_width)?;
    if indices_start
        .checked_add(indices_size)
        .is_none_or(|end| end > data.len())
    {
        return Err(corrupt("DICT_FSST payload outside segment"));
    }
    let symbols = if mode == 0 {
        Vec::new()
    } else {
        strings::symbol_table(&data[symbol_start..symbol_start + symbol_size]).map_err(|error| {
            match error {
                Error::Unsupported(_) => corrupt("invalid DICT_FSST symbol table version"),
                other => other,
            }
        })?
    };
    let lengths = strings::unpack(
        &data[lengths_start..lengths_start + lengths_size],
        dictionary_count,
        length_width,
        query,
    )?;
    if lengths[0] != 0 {
        return Err(corrupt("DICT_FSST NULL entry has bytes"));
    }
    let mut dictionary = Vec::with_capacity(dictionary_count);
    dictionary.push(Value::Null);
    let mut offset = 0_usize;
    for &length in &lengths[1..] {
        query.check()?;
        let end = offset
            .checked_add(length)
            .filter(|&end| end <= dictionary_size)
            .ok_or_else(|| corrupt("DICT_FSST string outside dictionary"))?;
        let bytes = &data[16 + offset..16 + end];
        let bytes = if mode == 0 {
            bytes.to_vec()
        } else {
            strings::decompress(bytes, &symbols, query)?
        };
        dictionary.push(strings::string_value(bytes, data_type)?);
        offset = end;
    }
    if offset != dictionary_size {
        return Err(corrupt("DICT_FSST dictionary length mismatch"));
    }
    if mode == 2 {
        return Ok(dictionary.into_iter().skip(1).collect());
    }
    strings::unpack(
        &data[indices_start..indices_start + indices_size],
        count,
        index_width,
        query,
    )?
    .into_iter()
    .enumerate()
    .map(|(row, index)| {
        if row % 1024 == 0 {
            query.check()?;
        }
        dictionary
            .get(index)
            .cloned()
            .ok_or_else(|| corrupt("DICT_FSST dictionary index out of range"))
    })
    .collect()
}
