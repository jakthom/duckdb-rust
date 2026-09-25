//! DuckDB ROARING (codec 13), distinct from the portable Roaring file format.
//! Each 2,048-bit container stores runs, sparse positions, or a raw bitset.
use super::{
    super::binary::{corrupt, u16_at, u64_at},
    packed,
};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};

pub struct RoaringDecoder;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SegmentDecoder for RoaringDecoder {
    fn id(&self) -> CodecId {
        CodecId(13)
    }
    fn name(&self) -> &'static str {
        "duckdb-roaring"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        matches!(
            kind,
            SegmentType::Validity | SegmentType::Values(DataType::Boolean)
        )
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        if !self.supports(input.kind) {
            return Err(Error::Unsupported(
                "ROARING requires validity or BOOLEAN".into(),
            ));
        }
        decode(input.data, input.count, context.query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn align(position: usize, alignment: usize) -> Result<usize> {
    position
        .checked_add(alignment - 1)
        .map(|position| position & !(alignment - 1))
        .ok_or_else(|| corrupt("ROARING alignment overflow"))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn take<'a>(data: &'a [u8], position: &mut usize, count: usize) -> Result<&'a [u8]> {
    let end = position
        .checked_add(count)
        .ok_or_else(|| corrupt("ROARING offset overflow"))?;
    let bytes = data
        .get(*position..end)
        .ok_or_else(|| corrupt("ROARING payload exceeds data region"))?;
    *position = end;
    Ok(bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decode(data: &[u8], count: usize, query: &QueryContext) -> Result<Vec<Value>> {
    query.check_rows(count)?;
    let offset = usize::try_from(u64_at(data, 0)?)
        .map_err(|_| corrupt("ROARING metadata offset overflow"))?;
    if offset % 8 != 0 {
        return Err(corrupt("ROARING metadata is not aligned"));
    }
    let mut cursor = 8;
    let payload = take(data, &mut cursor, offset)?;
    let containers = count.div_ceil(2048);
    let flags = packed::words(
        take(data, &mut cursor, packed::byte_count(containers, 2)?)?,
        containers,
        2,
        query,
    )?;
    let runs = flags.iter().filter(|flags| **flags & 2 != 0).count();
    let run_counts = packed::words(
        take(data, &mut cursor, packed::byte_count(runs, 7)?)?,
        runs,
        7,
        query,
    )?;
    let arrays = take(data, &mut cursor, containers - runs)?;
    let mut output = Vec::new();
    output
        .try_reserve(count)
        .map_err(|_| Error::Resource("ROARING output allocation failed".into()))?;
    let (mut run_index, mut array_index, mut position) = (0, 0, 0);
    for (index, flags) in flags.into_iter().enumerate() {
        query.check()?;
        let size = (count - index * 2048).min(2048);
        let run = flags & 2 != 0;
        let inverted = flags & 1 != 0;
        let amount = if run {
            let amount = run_counts[run_index] as usize;
            run_index += 1;
            amount
        } else {
            let amount = arrays[array_index] as usize;
            array_index += 1;
            amount
        };
        let mut bits = vec![inverted || run; size];
        if !run && amount == 249 {
            if inverted {
                return Err(corrupt("ROARING bitset has inverted flag"));
            }
            position = align(position, 8)?;
            // Some C++ checkpoints reserve a full final bitset even when the
            // segment ends with a partial container. Its unused bits do not
            // change the logical cardinality.
            let bytes_count =
                if index + 1 == containers && payload.len().checked_sub(position) == Some(256) {
                    256
                } else {
                    size.div_ceil(64) * 8
                };
            let bytes = take(payload, &mut position, bytes_count)?;
            for (i, bit) in bits.iter_mut().enumerate() {
                *bit = bytes[i / 8] & (1 << (i % 8)) != 0;
            }
        } else if run {
            if !inverted || amount >= 124 {
                return Err(corrupt("invalid ROARING run metadata"));
            }
            let compressed = amount >= 4;
            let pairs = if compressed {
                compressed_positions(
                    take(payload, &mut position, 8 + amount * 2)?,
                    amount * 2,
                    true,
                )?
                .chunks_exact(2)
                .map(|pair| (pair[0], pair[1]))
                .collect::<Vec<_>>()
            } else {
                position = align(position, 4)?;
                let bytes = take(payload, &mut position, amount * 4)?;
                (0..amount)
                    .map(|i| {
                        Ok((
                            u16_at(bytes, i * 4)? as usize,
                            u16_at(bytes, i * 4)? as usize + u16_at(bytes, i * 4 + 2)? as usize + 1,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            let mut previous = 0;
            for (i, (start, end)) in pairs.into_iter().enumerate() {
                // C++ Finalize stores one extra trailing invalid bit in the
                // short-run form; it is outside the logical cardinality.
                let trailing = !compressed && i + 1 == amount && end == size + 1;
                if start < previous || start >= size || end <= start || (end > size && !trailing) {
                    return Err(corrupt("ROARING run outside container or overlapping"));
                }
                bits[start..end.min(size)].fill(false);
                previous = end;
            }
        } else {
            if amount >= 248 {
                return Err(corrupt("invalid ROARING array cardinality"));
            }
            let positions = if amount >= 8 {
                compressed_positions(take(payload, &mut position, 8 + amount)?, amount, false)?
            } else {
                position = align(position, 2)?;
                let bytes = take(payload, &mut position, amount * 2)?;
                (0..amount)
                    .map(|i| u16_at(bytes, i * 2).map(usize::from))
                    .collect::<Result<Vec<_>>>()?
            };
            let mut previous = None;
            for index in positions {
                if index >= size || previous.is_some_and(|previous| index <= previous) {
                    return Err(corrupt(
                        "ROARING array index outside container or unordered",
                    ));
                }
                bits[index] = !inverted;
                previous = Some(index);
            }
        }
        output.extend(bits.into_iter().map(Value::Boolean));
    }
    if align(position, 8)? != payload.len() {
        return Err(corrupt("ROARING data and metadata boundary mismatch"));
    }
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compressed_positions(data: &[u8], count: usize, runs: bool) -> Result<Vec<usize>> {
    let segments = data
        .get(..8)
        .ok_or_else(|| corrupt("truncated ROARING segment counts"))?;
    let total = segments
        .iter()
        .map(|count| usize::from(*count))
        .sum::<usize>();
    if total != count && !(runs && total + 1 == count) {
        return Err(corrupt("ROARING segment count mismatch"));
    }
    let mut result = Vec::with_capacity(count);
    let (mut segment, mut used) = (0, 0);
    for (i, value) in data[8..].iter().enumerate() {
        while segment < 8 && used >= usize::from(segments[segment]) {
            segment += 1;
            used = 0;
        }
        if segment == 8 && !(runs && i + 1 == count && *value == 0) {
            return Err(corrupt("ROARING compressed position exceeds container"));
        }
        result.push(segment * 256 + usize::from(*value));
        used += 1;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn container(flags: u8, count: u8, mut payload: Vec<u8>) -> Vec<u8> {
        payload.resize(payload.len().div_ceil(8) * 8, 0);
        let mut output = (payload.len() as u64).to_le_bytes().to_vec();
        output.extend(payload);
        let mut types = vec![0; 8];
        types[0] = flags;
        output.extend(types);
        if flags & 2 != 0 {
            let mut runs = vec![0; 28];
            runs[0] = count;
            output.extend(runs);
        } else {
            output.push(count);
        }
        output
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn roaring_decodes_sparse_inverted_runs_raw_and_partial_containers() -> Result<()> {
        let query = QueryContext::background();
        for flags in [0, 1] {
            let values = decode(&container(flags, 2, vec![2, 0, 8, 0]), 10, &query)?;
            for (i, value) in values.iter().enumerate() {
                assert_eq!(*value, Value::Boolean([2, 8].contains(&i) ^ (flags == 1)));
            }
        }
        let values = decode(&container(3, 1, vec![2, 0, 4, 0]), 10, &query)?;
        assert_eq!(
            values,
            (0..10)
                .map(|i| Value::Boolean(!(2..7).contains(&i)))
                .collect::<Vec<_>>()
        );
        let mut array = vec![8, 0, 0, 0, 0, 0, 0, 0];
        array.extend(0..8);
        assert_eq!(
            decode(&container(1, 8, array), 13, &query)?,
            (0..13).map(|i| Value::Boolean(i >= 8)).collect::<Vec<_>>()
        );
        let mut runs = vec![4, 2, 0, 0, 0, 0, 0, 1];
        runs.extend([0, 2, 4, 6, 0, 2, 254, 0]);
        let values = decode(&container(3, 4, runs), 2048, &query)?;
        for (i, value) in values.iter().enumerate() {
            assert_eq!(
                *value,
                Value::Boolean(
                    ![0..2, 4..6, 256..258, 2046..2048]
                        .iter()
                        .any(|range| range.contains(&i))
                )
            );
        }
        for bytes in [8, 256] {
            let values = decode(&container(0, 249, vec![0b10101010; bytes]), 13, &query)?;
            assert_eq!(
                values,
                (0..13)
                    .map(|i| Value::Boolean(i % 2 == 1))
                    .collect::<Vec<_>>()
            );
        }
        // C++ short final runs can include one unobserved trailing NULL bit.
        assert_eq!(
            decode(&container(3, 1, vec![8, 0, 2, 0]), 10, &query)?,
            (0..10).map(|i| Value::Boolean(i < 8)).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn roaring_rejects_truncation_bad_counts_indices_runs_and_offsets() -> Result<()> {
        let query = QueryContext::background();
        let valid = container(1, 2, vec![2, 0, 8, 0]);
        for end in 0..valid.len() {
            assert!(
                decode(&valid[..end], 10, &query).is_err(),
                "truncation at {end}"
            );
        }
        for invalid in [
            container(1, 2, vec![2, 0, 2, 0]),
            container(1, 2, vec![8, 0, 2, 0]),
            container(1, 1, vec![10, 0]),
            container(3, 1, vec![9, 0, 5, 0]),
            container(3, 2, vec![2, 0, 4, 0, 3, 0, 2, 0]),
            container(1, 248, vec![]),
            container(3, 124, vec![]),
            container(2, 0, vec![]),
            container(1, 249, vec![0; 8]),
            container(1, 8, vec![0; 16]),
        ] {
            assert!(decode(&invalid, 10, &query).is_err());
        }
        for offset in [1, u64::MAX, 256] {
            let mut data = valid.clone();
            data[..8].copy_from_slice(&offset.to_le_bytes());
            assert!(decode(&data, 10, &query).is_err());
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn roaring_obeys_cancellation_and_row_limits_before_decoding() -> Result<()> {
        let interrupt = crate::parallel::InterruptHandle::default();
        let query = QueryContext::new(interrupt.clone(), None, 2048, 3)?;
        assert!(matches!(decode(&[], 4, &query), Err(Error::Resource(_))));
        interrupt.interrupt();
        assert!(matches!(decode(&[], 1, &query), Err(Error::Interrupted)));
        Ok(())
    }
}
