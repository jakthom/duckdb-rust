use super::{
    super::binary::{corrupt, u32_at, u64_at},
    primitive::{scalar, width},
};
use crate::{
    common::{DataType, Error, Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};
use std::collections::HashSet;
pub struct UncompressedDecoder;
impl SegmentDecoder for UncompressedDecoder {
    fn id(&self) -> CodecId {
        CodecId(1)
    }
    fn name(&self) -> &'static str {
        "duckdb-uncompressed"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        kind != SegmentType::Values(&DataType::Null)
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        context.query.check_rows(input.count)?;
        match input.kind {
            SegmentType::Values(DataType::Varchar) => strings(context, input.data, input.count),
            kind => (0..input.count)
                .map(|i| {
                    if i % 1024 == 0 {
                        context.query.check()?;
                    }
                    match kind {
                        SegmentType::Validity => input
                            .data
                            .get(i / 8)
                            .map(|byte| Value::Boolean(byte & (1 << (i % 8)) != 0))
                            .ok_or_else(|| corrupt("truncated validity")),
                        SegmentType::Values(data_type) => scalar(
                            input.data,
                            i.checked_mul(width(data_type)?)
                                .ok_or_else(|| corrupt("scalar offset overflow"))?,
                            data_type,
                        ),
                    }
                })
                .collect(),
        }
    }
}

fn strings(context: &DecodeContext<'_>, data: &[u8], count: usize) -> Result<Vec<Value>> {
    let size = u32_at(data, 0)? as usize;
    let end = u32_at(data, 4)? as usize;
    if size > end || end > data.len() || end - size < 8 + count * 4 {
        return Err(corrupt("invalid string dictionary bounds"));
    }
    let mut previous = 0usize;
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        context.query.check()?;
        let raw = u32_at(data, 8 + i * 4)? as i32;
        let offset = raw.unsigned_abs() as usize;
        if offset < previous || offset > size {
            return Err(corrupt("invalid string dictionary offset"));
        }
        let start = end - offset;
        let length = offset - previous;
        let bytes = if raw < 0 && length > 0 {
            if length != 12 {
                return Err(corrupt("invalid overflow string marker"));
            }
            overflow_string(
                context,
                u64_at(data, start)?,
                u32_at(data, start + 8)? as usize,
            )?
        } else {
            data.get(start..start + length)
                .ok_or_else(|| corrupt("string outside dictionary"))?
                .to_vec()
        };
        values.push(Value::Varchar(
            String::from_utf8(bytes).map_err(|_| corrupt("invalid UTF-8 string"))?,
        ));
        previous = offset;
    }
    Ok(values)
}

fn overflow_string(context: &DecodeContext<'_>, mut id: u64, mut offset: usize) -> Result<Vec<u8>> {
    let mut block = context.blocks.block(id)?;
    let length = u32_at(block, offset)? as usize;
    offset += 4;
    if length > 16 * 1024 * 1024 {
        return Err(Error::Resource("overflow string exceeds 16 MiB".into()));
    }
    let mut bytes = Vec::with_capacity(length);
    let mut visited = HashSet::new();
    while bytes.len() < length {
        context.query.check()?;
        if !visited.insert(id) {
            return Err(corrupt("cyclic overflow string"));
        }
        let end = block
            .len()
            .checked_sub(8)
            .ok_or_else(|| corrupt("truncated overflow block"))?;
        let available = end
            .checked_sub(offset)
            .ok_or_else(|| corrupt("overflow string offset outside block"))?;
        let count = available.min(length - bytes.len());
        bytes.extend(&block[offset..offset + count]);
        if bytes.len() < length {
            id = u64_at(block, end)?;
            block = context.blocks.block(id)?;
            offset = 0;
        }
    }
    Ok(bytes)
}
