use super::super::binary::{corrupt, u32_at, u64_at};
use crate::{
    common::{DataType, Error, Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};
macro_rules! string_decoder {
    ($name:ident, $id:literal, $label:literal, $decode:ident) => {
        pub struct $name;
        #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
        impl SegmentDecoder for $name {
            fn id(&self) -> CodecId {
                CodecId($id)
            }
            fn name(&self) -> &'static str {
                $label
            }
            fn supports(&self, kind: SegmentType<'_>) -> bool {
                kind == SegmentType::Values(&DataType::Varchar)
            }
            fn decode(
                &self,
                input: DecodeInput<'_>,
                context: &DecodeContext<'_>,
            ) -> Result<Vec<Value>> {
                context.query.check_rows(input.count)?;
                if !self.supports(input.kind) {
                    return Err(Error::Unsupported("string segment type".into()));
                }
                $decode(input.data, input.count, context.query)
            }
        }
    };
}
string_decoder!(DictionaryDecoder, 4, "duckdb-dictionary", dictionary);
string_decoder!(FsstDecoder, 7, "duckdb-fsst", fsst);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unpack(
    data: &[u8],
    count: usize,
    width: usize,
    query: &crate::parallel::QueryContext,
) -> Result<Vec<usize>> {
    if width > 32 {
        return Err(corrupt("invalid string bitpacking width"));
    }
    super::packed::words(data, count, width, query)
        .map(|values| values.into_iter().map(|v| v as usize).collect())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn dictionary(
    data: &[u8],
    count: usize,
    query: &crate::parallel::QueryContext,
) -> Result<Vec<Value>> {
    let size = u32_at(data, 0)? as usize;
    let end = u32_at(data, 4)? as usize;
    let index_offset = u32_at(data, 8)? as usize;
    let index_count = u32_at(data, 12)? as usize;
    let width = u32_at(data, 16)? as usize;
    if index_count == 0 || index_count > data.len() / 4 || width > 32 {
        return Err(corrupt("invalid dictionary count or width"));
    }
    let expected_width = (usize::BITS - (index_count - 1).leading_zeros()) as usize;
    if width != expected_width
        || index_offset != 20 + count.div_ceil(32) * 32 * width / 8
        || size > end
        || end > data.len()
        || end - size < index_offset + index_count * 4
    {
        return Err(corrupt("invalid dictionary bounds"));
    }
    let mut dictionary = vec![Value::Null];
    let mut previous = 0;
    for i in 0..index_count {
        if i % 1024 == 0 {
            query.check()?;
        }
        let offset = u32_at(data, index_offset + i * 4)? as usize;
        if offset < previous || offset > size || (i == 0 && offset != 0) {
            return Err(corrupt("invalid dictionary string offset"));
        }
        if i > 0 {
            let bytes = &data[end - offset..end - previous];
            dictionary.push(Value::Varchar(
                std::str::from_utf8(bytes)
                    .map_err(|_| corrupt("invalid dictionary UTF-8"))?
                    .into(),
            ));
        }
        previous = offset;
    }
    unpack(&data[20..index_offset], count, width, query)?
        .into_iter()
        .map(|index| {
            dictionary
                .get(index)
                .cloned()
                .ok_or_else(|| corrupt("dictionary index out of range"))
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn fsst(
    data: &[u8],
    count: usize,
    query: &crate::parallel::QueryContext,
) -> Result<Vec<Value>> {
    let size = u32_at(data, 0)? as usize;
    let end = u32_at(data, 4)? as usize;
    let width = u32_at(data, 8)? as usize;
    let symbols_offset = u32_at(data, 12)? as usize;
    if width > 32
        || size > end
        || end > data.len()
        || symbols_offset != 16 + count.div_ceil(32) * 32 * width / 8
        || symbols_offset > end - size
    {
        return Err(corrupt("invalid FSST bounds"));
    }
    let lengths = unpack(&data[16..symbols_offset], count, width, query)?;
    let table = &data[symbols_offset..end - size];
    let symbols = if size == 0 {
        Vec::new()
    } else {
        symbol_table(table)?
    };
    let mut offset = 0usize;
    let mut values = Vec::with_capacity(count);
    for length in lengths {
        query.check()?;
        offset = offset
            .checked_add(length)
            .ok_or_else(|| corrupt("FSST string offset overflow"))?;
        if offset > size {
            return Err(corrupt("FSST string outside dictionary"));
        }
        let input = &data[end - offset..end - offset + length];
        let mut output = Vec::new();
        let mut position = 0;
        while position < input.len() {
            if position % 1024 == 0 {
                query.check()?;
            }
            let code = input[position];
            position += 1;
            if code == 255 {
                output.push(
                    *input
                        .get(position)
                        .ok_or_else(|| corrupt("truncated FSST escape"))?,
                );
                position += 1;
            } else {
                output.extend(
                    symbols
                        .get(code as usize)
                        .ok_or_else(|| corrupt("invalid FSST symbol"))?,
                );
            }
        }
        values.push(Value::Varchar(
            String::from_utf8(output).map_err(|_| corrupt("invalid FSST UTF-8"))?,
        ));
    }
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn symbol_table(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    if data.len() < 17 {
        return Err(corrupt("truncated FSST symbol table"));
    }
    if u64_at(data, 0)? >> 32 != 20_190_218 {
        return Err(Error::Unsupported("FSST symbol table version".into()));
    }
    let terminated = data[8] & 1 != 0;
    let mut histogram = data[9..17].to_vec();
    let mut symbols = Vec::new();
    if terminated {
        histogram[0] = histogram[0]
            .checked_sub(1)
            .ok_or_else(|| corrupt("invalid FSST terminator"))?;
        symbols.push(vec![0]);
    }
    let mut offset = 17;
    for index in [1, 2, 3, 4, 5, 6, 7, 0] {
        let length = index + 1;
        for _ in 0..histogram[index] {
            if symbols.len() >= 255 {
                return Err(corrupt("FSST symbol count exceeds 255"));
            }
            let symbol = data
                .get(offset..offset + length)
                .ok_or_else(|| corrupt("truncated FSST symbol"))?;
            symbols.push(symbol.to_vec());
            offset += length;
        }
    }
    Ok(symbols)
}
