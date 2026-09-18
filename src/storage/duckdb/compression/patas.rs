use super::{
    super::binary::{corrupt, u16_at},
    floating::Floating,
    layout::ReverseMetadata,
};
use crate::{
    common::{Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};

/// Historical Patas FLOAT/DOUBLE storage. Metadata describes byte-aligned XOR
/// residuals and backward references within each 1,024-value group.
pub struct PatasDecoder;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SegmentDecoder for PatasDecoder {
    fn id(&self) -> CodecId {
        CodecId(9)
    }
    fn name(&self) -> &'static str {
        "duckdb-patas"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        Floating::for_segment(kind).is_ok()
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        context.query.check_rows(input.count)?;
        let floating = Floating::for_segment(input.kind)?;
        let mut metadata = ReverseMetadata::new(input.data)?;
        let mut groups = Vec::new();
        for first in (0..input.count).step_by(1024) {
            context.query.check()?;
            let count = 1024.min(input.count - first);
            let offset = metadata.offset()?;
            let stats = metadata.take(count * 2)?;
            groups.push((offset, count, stats));
        }
        let mut values = Vec::with_capacity(input.count);
        for (group, &(start, count, stats)) in groups.iter().enumerate() {
            context.query.check()?;
            let end = groups.get(group + 1).map_or(metadata.position(), |g| g.0);
            if start < 4 || end < start || end > metadata.position() {
                return Err(corrupt("invalid Patas group bounds"));
            }
            let data = &input.data[start..end];
            let mut position = 0;
            let mut previous = [0u64; 1024];
            for i in 0..count {
                let packed = u16_at(stats, 2 * i)?;
                let distance = (packed >> 9) as usize;
                let stored_bytes = ((packed >> 6) & 7) as usize;
                let trailing = (packed & 63) as usize;
                if trailing >= floating.bits()
                    || distance > i
                    || (i != 0 && distance == 0)
                    || (i == 0 && (trailing != 0 || stored_bytes != floating.bits() / 8 % 8))
                {
                    return Err(corrupt("invalid Patas backward reference or width"));
                }
                let bytes = match (stored_bytes, trailing) {
                    (0, 0..8) => floating.bits() / 8,
                    (0, _) => 0,
                    (n, _) => n,
                };
                if bytes > floating.bits() / 8 {
                    return Err(corrupt("Patas residual exceeds floating width"));
                }
                let source = data
                    .get(position..position + bytes)
                    .ok_or_else(|| corrupt("truncated Patas residual"))?;
                let mut residual = [0; 8];
                residual[..bytes].copy_from_slice(source);
                position += bytes;
                let residual = u64::from_le_bytes(residual);
                if trailing != 0 && residual >> (floating.bits() - trailing) != 0 {
                    return Err(corrupt("Patas residual overflows shifted width"));
                }
                let value = previous[i - distance] ^ (residual << trailing);
                previous[i] = value;
                values.push(floating.value(value));
            }
        }
        Ok(values)
    }
}
