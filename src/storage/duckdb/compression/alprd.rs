use super::{
    super::binary::{corrupt, u16_at},
    floating::Floating,
    layout::OffsetGroups,
    packed,
};
use crate::{
    common::{Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};

/// ALP-RD preserves the original IEEE bits by dictionary coding the high bits
/// and packing the low bits. Exceptions replace only the high part.
pub struct AlpRdDecoder;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SegmentDecoder for AlpRdDecoder {
    fn id(&self) -> CodecId {
        CodecId(11)
    }
    fn name(&self) -> &'static str {
        "duckdb-alprd"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        Floating::for_segment(kind).is_ok()
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        context.query.check_rows(input.count)?;
        let floating = Floating::for_segment(input.kind)?;
        let header = input
            .data
            .get(..7)
            .ok_or_else(|| corrupt("truncated ALP-RD header"))?;
        let right_width = header[4] as usize;
        let left_width = header[5] as usize;
        let dictionary_count = header[6] as usize;
        if !(floating.bits() - 16..floating.bits()).contains(&right_width)
            || !(1..=3).contains(&left_width)
            || !(1..=8).contains(&dictionary_count)
        {
            return Err(corrupt("invalid ALP-RD widths or dictionary size"));
        }
        let expected_width =
            ((usize::BITS - (dictionary_count - 1).leading_zeros()) as usize).max(1);
        if left_width != expected_width {
            return Err(corrupt("ALP-RD dictionary width mismatch"));
        }
        let dictionary = (0..dictionary_count)
            .map(|i| u16_at(input.data, 7 + 2 * i))
            .collect::<Result<Vec<_>>>()?;
        for &left in &dictionary {
            floating.combine(left, 0, right_width)?;
        }
        let groups = input.count.div_ceil(1024);
        let layout = OffsetGroups::new(input.data, 7 + 2 * dictionary_count, groups)?;
        let mut values = Vec::with_capacity(input.count);
        for group in 0..groups {
            context.query.check()?;
            let data = layout.group(group)?;
            let count = 1024.min(input.count - values.len());
            let exceptions = u16_at(data, 0)? as usize;
            if exceptions > count {
                return Err(corrupt("ALP-RD exception count exceeds group"));
            }
            let left_end = 2 + packed::byte_count(count, left_width)?;
            let right_end = left_end + packed::byte_count(count, right_width)?;
            let positions_start = right_end + 2 * exceptions;
            if positions_start + 2 * exceptions > data.len() {
                return Err(corrupt("truncated ALP-RD payload"));
            }
            let left = packed::words(&data[2..left_end], count, left_width, context.query)?;
            let right = packed::words(
                &data[left_end..right_end],
                count,
                right_width,
                context.query,
            )?;
            let mut replacements = vec![None; count];
            let mut previous = None;
            for i in 0..exceptions {
                let position = u16_at(data, positions_start + 2 * i)? as usize;
                if position >= count || previous.is_some_and(|prev| position <= prev) {
                    return Err(corrupt("invalid ALP-RD exception position"));
                }
                previous = Some(position);
                replacements[position] = Some(u16_at(data, right_end + 2 * i)?);
            }
            for ((left, right), replacement) in left.into_iter().zip(right).zip(replacements) {
                // Exception indexes can be outside the actual dictionary, or
                // truncated by packing. Resolve exceptions before dereferencing.
                let left = match replacement {
                    Some(left) => left,
                    None => *dictionary
                        .get(left as usize)
                        .ok_or_else(|| corrupt("ALP-RD dictionary index out of range"))?,
                };
                values.push(floating.combine(left, right as u64, right_width)?);
            }
        }
        Ok(values)
    }
}
