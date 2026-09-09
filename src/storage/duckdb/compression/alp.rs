use super::{
    super::binary::{corrupt, u64_at},
    floating::Floating,
    layout::OffsetGroups,
    packed,
};
use crate::{
    common::{Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};

/// DuckDB ALP FLOAT/DOUBLE storage (codec 10). ALP groups contain 1,024 values,
/// independently of DuckDB's stored or the client's execution vector size.
pub struct AlpDecoder;
impl SegmentDecoder for AlpDecoder {
    fn id(&self) -> CodecId {
        CodecId(10)
    }
    fn name(&self) -> &'static str {
        "duckdb-alp"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        Floating::for_segment(kind).is_ok()
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        context.query.check_rows(input.count)?;
        decode(
            input.data,
            input.count,
            Floating::for_segment(input.kind)?,
            context,
        )
    }
}

// Exact constants and operation order are part of ALP's lossless decoding.
// Computing a reciprocal with powi or folding the two products can round
// differently from the writer's decode-and-compare step.
const FACTORS: [f64; 19] = [
    1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18,
];
const FRACTIONS: [f64; 19] = [
    1.0, 1e-1, 1e-2, 1e-3, 1e-4, 1e-5, 1e-6, 1e-7, 1e-8, 1e-9, 1e-10, 1e-11, 1e-12, 1e-13, 1e-14,
    1e-15, 1e-16, 1e-17, 1e-18,
];

fn decode(
    data: &[u8],
    count: usize,
    floating: Floating,
    context: &DecodeContext<'_>,
) -> Result<Vec<Value>> {
    let groups = count.div_ceil(1024);
    let layout = OffsetGroups::new(data, 4, groups)?;
    let maximum_exponent = match floating {
        Floating::Single => 10,
        Floating::Double => 18,
    };
    let exception_width = floating.bits() / 8;
    let mut values = Vec::with_capacity(count);
    for group in 0..groups {
        context.query.check()?;
        let data = layout.group(group)?;
        let header = data
            .get(..13)
            .ok_or_else(|| corrupt("truncated ALP group header"))?;
        let exponent = header[0] as usize;
        let factor = header[1] as usize;
        let exceptions = u16::from_le_bytes([header[2], header[3]]) as usize;
        let frame = u64_at(header, 4)?;
        let width = header[12] as usize;
        let n = 1024.min(count - values.len());
        if exponent > maximum_exponent || factor > exponent || width > 64 || exceptions > n {
            return Err(corrupt("invalid ALP group parameters"));
        }
        let packed_end = 13 + packed::byte_count(n, width)?;
        let exceptions_end = packed_end + exceptions * exception_width;
        if exceptions_end + exceptions * 2 > data.len() {
            return Err(corrupt("truncated ALP group payload"));
        }
        let base = values.len();
        for value in packed::words(&data[13..packed_end], n, width, context.query)? {
            let integer = (value as u64).wrapping_add(frame) as i64;
            values.push(match floating {
                Floating::Single => Value::Float(
                    integer as f32 * FACTORS[factor] as f32 * FRACTIONS[exponent] as f32,
                ),
                Floating::Double => {
                    Value::Double(integer as f64 * FACTORS[factor] * FRACTIONS[exponent])
                }
            });
        }
        let mut previous = None;
        for i in 0..exceptions {
            let offset = exceptions_end + 2 * i;
            let position = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
            if position >= n || previous.is_some_and(|prev| position <= prev) {
                return Err(corrupt("invalid ALP exception position"));
            }
            previous = Some(position);
            values[base + position] = floating.read(data, packed_end + i * exception_width)?;
        }
    }
    Ok(values)
}
