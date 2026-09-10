use super::{
    super::binary::{corrupt, u16_at},
    floating::Floating,
    layout::ReverseMetadata,
};
use crate::{
    common::{Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};

/// Historical Chimp128 storage. Its bitstream continues across group boundaries;
/// only the reference ring and leading-zero state reset every 1,024 values.
pub struct ChimpDecoder;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SegmentDecoder for ChimpDecoder {
    fn id(&self) -> CodecId {
        CodecId(8)
    }
    fn name(&self) -> &'static str {
        "duckdb-chimp"
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
            let leading_blocks = metadata.take(1)?[0] as usize;
            if leading_blocks > 128 {
                return Err(corrupt("invalid Chimp leading-zero block count"));
            }
            let leading = metadata.take(leading_blocks * 3)?;
            let flag_bytes = metadata.take((count - 1).div_ceil(4))?;
            let flags: Vec<_> = (0..count - 1)
                .map(|i| (flag_bytes[i / 4] >> (6 - 2 * (i % 4))) & 3)
                .collect();
            if flags.iter().filter(|&&flag| flag == 3).count().div_ceil(8) != leading_blocks {
                return Err(corrupt("Chimp leading-zero metadata count mismatch"));
            }
            let packed = metadata.aligned(flags.iter().filter(|&&flag| flag == 1).count() * 2)?;
            groups.push(Group {
                offset,
                count,
                flags,
                leading,
                packed,
            });
        }
        let mut bits = Bits {
            data: &input.data[4..metadata.position()],
            position: 0,
        };
        let mut values = Vec::with_capacity(input.count);
        for (group_index, group) in groups.iter().enumerate() {
            context.query.check()?;
            // Historical writers recorded rounded byte counts here, omitting
            // the header after group zero. They cannot identify the bit boundary.
            // As in DuckDB's reader, actual progress comes from the bitstream.
            if group.offset < 4
                || group.offset > metadata.position()
                || (group_index == 0 && group.offset != 4)
            {
                return Err(corrupt("invalid Chimp group offset"));
            }
            let mut ring = [None; 128];
            let mut previous = bits.read(floating.bits())?;
            ring[0] = Some(previous);
            values.push(floating.value(previous));
            let mut leading = None;
            let mut leading_index = 0;
            let mut packed_index = 0;
            for i in 1..group.count {
                let value = match group.flags[i - 1] {
                    0 => {
                        let index = bits.read(7)? as usize;
                        reference(&ring, index)?
                    }
                    1 => {
                        let packed = u16_at(group.packed, 2 * packed_index)?;
                        packed_index += 1;
                        let zeros = LEADING[((packed >> 6) & 7) as usize];
                        let width = (packed & 63) as usize;
                        // FLOAT uses five significant-width bits. Zero has no
                        // valid FLOAT meaning here; the historical reader's
                        // zero-to-64 sentinel applies only to DOUBLE.
                        if floating.bits() == 32 && (width == 0 || width & 32 != 0) {
                            return Err(corrupt("invalid Chimp FLOAT width encoding"));
                        }
                        let width = if width == 0 { floating.bits() } else { width };
                        if width + zeros > floating.bits() {
                            return Err(corrupt("invalid Chimp significant width"));
                        }
                        leading = Some(zeros);
                        (bits.read(width)? << (floating.bits() - width - zeros))
                            ^ reference(&ring, (packed >> 9) as usize)?
                    }
                    flag @ (2 | 3) => {
                        if flag == 3 {
                            let start = (leading_index / 8) * 3;
                            let bytes = &group.leading[start..start + 3];
                            let codes = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]);
                            leading =
                                Some(LEADING[((codes >> (3 * (leading_index % 8))) & 7) as usize]);
                            leading_index += 1;
                        }
                        let zeros = leading
                            .ok_or_else(|| corrupt("Chimp reuses missing leading-zero state"))?;
                        bits.read(floating.bits() - zeros)? ^ previous
                    }
                    _ => unreachable!("two-bit flags"),
                };
                previous = value;
                ring[i % 128] = Some(value);
                values.push(floating.value(value));
            }
        }
        Ok(values)
    }
}

const LEADING: [usize; 8] = [0, 8, 12, 16, 18, 20, 22, 24];

struct Group<'a> {
    offset: usize,
    count: usize,
    flags: Vec<u8>,
    leading: &'a [u8],
    packed: &'a [u8],
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn reference(ring: &[Option<u64>; 128], index: usize) -> Result<u64> {
    ring.get(index)
        .copied()
        .flatten()
        .ok_or_else(|| corrupt("invalid Chimp ring reference"))
}

/// MSB-first bits, unlike DuckDB's little-endian integer bitpacking. Reads
/// touch only required bytes, including the final partially used byte.
struct Bits<'a> {
    data: &'a [u8],
    position: usize,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Bits<'_> {
    fn read(&mut self, count: usize) -> Result<u64> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| corrupt("Chimp bit offset overflow"))?;
        if count > 64 || end.div_ceil(8) > self.data.len() {
            return Err(corrupt("truncated Chimp bitstream"));
        }
        let mut result = 0u64;
        while self.position < end {
            let available = 8 - self.position % 8;
            let take = available.min(end - self.position);
            let mask = ((1u16 << take) - 1) as u8;
            result = (result << take)
                | u64::from((self.data[self.position / 8] >> (available - take)) & mask);
            self.position += take;
        }
        Ok(result)
    }
}
