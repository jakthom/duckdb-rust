use super::super::binary::{corrupt, u32_at, u64_at};
use super::{
    packed,
    primitive::{integer, integer_value, width},
};
use crate::{
    common::{DataType, Error, Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};

type Unpack = fn(&[u8], usize, usize, &crate::parallel::QueryContext) -> Result<Vec<u128>>;

pub struct BitPackingDecoder;
/// Bit-at-a-time implementation of the same format, selectable through the
/// registry for algorithm comparisons. No checkpoint or SQL caller changes.
pub struct ScalarBitPackingDecoder;

macro_rules! decoder {
    ($name:ident, $label:literal, $unpack:ident) => {
        #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
        impl SegmentDecoder for $name {
            fn id(&self) -> CodecId { CodecId(6) }
            fn name(&self) -> &'static str { $label }
            fn supports(&self, kind: SegmentType<'_>) -> bool { matches!(kind, SegmentType::Values(t) if t.is_integer() || t.is_decimal() || matches!(t, DataType::Date | DataType::Uuid)) }
            fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
                context.query.check_rows(input.count)?;
                match input.kind {
                    SegmentType::Values(t) if self.supports(input.kind) => bitpacking(input.data, input.count, t, context.vector_size.max(2048), context, packed::$unpack),
                    _ => Err(Error::Unsupported("bitpacking segment type".into())),
                }
            }
        }
    };
}
decoder!(BitPackingDecoder, "duckdb-bitpacking-word", words);
decoder!(ScalarBitPackingDecoder, "duckdb-bitpacking-scalar", scalar);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bitpacking(
    data: &[u8],
    count: usize,
    data_type: &DataType,
    group_size: usize,
    context: &DecodeContext<'_>,
    unpack: Unpack,
) -> Result<Vec<Value>> {
    let size = width(data_type)?;
    let groups = count.div_ceil(group_size);
    let metadata_end =
        usize::try_from(u64_at(data, 0)?).map_err(|_| corrupt("bitpacking metadata overflow"))?;
    let metadata_start = metadata_end
        .checked_sub(groups * 4)
        .ok_or_else(|| corrupt("invalid bitpacking metadata"))?;
    if metadata_start < 8 || metadata_end > data.len() {
        return Err(corrupt("bitpacking metadata outside segment"));
    }
    let mut values = Vec::with_capacity(count);
    for group in 0..groups {
        context.query.check()?;
        let metadata = u32_at(data, metadata_end - 4 * (group + 1))?;
        let mode = metadata >> 24;
        let start = (metadata & 0x00ff_ffff) as usize;
        let end = if group + 1 < groups {
            (u32_at(data, metadata_end - 4 * (group + 2))? & 0x00ff_ffff) as usize
        } else {
            metadata_start
        };
        if start < 8 || end < start || end > metadata_start {
            return Err(corrupt("invalid bitpacking group bounds"));
        }
        let data = &data[start..end];
        let frame = integer(data, 0, size)?;
        let n = group_size.min(count - values.len());
        match mode {
            2 => values.extend(std::iter::repeat_n(integer_value(frame, data_type)?, n)),
            3 => {
                let delta = integer(data, size, size)?;
                for i in 0..n {
                    values.push(integer_value(
                        wrap(frame.wrapping_add(delta.wrapping_mul(i as i128)), size),
                        data_type,
                    )?);
                }
            }
            4 | 5 => {
                let width = usize::try_from(integer(data, size, size)?)
                    .map_err(|_| corrupt("negative bitpacking width"))?;
                if width > size * 8 {
                    return Err(corrupt("bitpacking width exceeds integer"));
                }
                let mut previous = if mode == 4 {
                    integer(data, size * 2, size)?
                } else {
                    0
                };
                let payload = data
                    .get(size * if mode == 4 { 3 } else { 2 }..)
                    .ok_or_else(|| corrupt("truncated bitpacking header"))?;
                for packed in unpack(payload, n, width, context.query)? {
                    let value = wrap((packed as i128).wrapping_add(frame), size);
                    let value = if mode == 4 {
                        previous = wrap(previous.wrapping_add(value), size);
                        previous
                    } else {
                        value
                    };
                    values.push(integer_value(value, data_type)?);
                }
            }
            _ => return Err(corrupt(format!("unknown bitpacking mode {mode}"))),
        }
    }
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn wrap(value: i128, size: usize) -> i128 {
    let shift = (16 - size) * 8;
    (value << shift) >> shift
}
