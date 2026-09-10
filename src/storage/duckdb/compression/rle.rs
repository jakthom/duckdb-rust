use super::{
    super::binary::{corrupt, u64_at},
    primitive::{scalar, width},
};
use crate::{
    common::{DataType, Error, Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};
pub struct RleDecoder;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SegmentDecoder for RleDecoder {
    fn id(&self) -> CodecId {
        CodecId(3)
    }
    fn name(&self) -> &'static str {
        "duckdb-rle"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        matches!(kind, SegmentType::Values(t) if t.is_numeric() || t.is_temporal() || matches!(t, DataType::Boolean | DataType::Date | DataType::Uuid))
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        context.query.check_rows(input.count)?;
        match input.kind {
            SegmentType::Values(t) if self.supports(input.kind) => {
                rle(input.data, input.count, t, context)
            }
            _ => Err(Error::Unsupported("RLE segment type".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn rle(
    data: &[u8],
    count: usize,
    data_type: &DataType,
    context: &DecodeContext<'_>,
) -> Result<Vec<Value>> {
    let offset = usize::try_from(u64_at(data, 0)?).map_err(|_| corrupt("RLE offset overflow"))?;
    let width = width(data_type)?;
    if offset < 8 || offset > data.len() || (offset - 8) % width != 0 {
        return Err(corrupt("invalid RLE layout"));
    }
    let mut values = Vec::with_capacity(count);
    for run in 0..(offset - 8) / width {
        context.query.check()?;
        if values.len() == count {
            break;
        }
        let bytes = data
            .get(offset + run * 2..offset + run * 2 + 2)
            .ok_or_else(|| corrupt("truncated RLE counts"))?;
        let length = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        if length == 0 || length > count - values.len() {
            return Err(corrupt("invalid RLE run length"));
        }
        values.extend(std::iter::repeat_n(
            scalar(data, 8 + run * width, data_type)?,
            length,
        ));
    }
    if values.len() != count {
        return Err(corrupt("RLE count mismatch"));
    }
    Ok(values)
}
