use super::super::binary::corrupt;
use crate::{common::Result, parallel::QueryContext};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// DuckDB pads little-endian packed integers to groups of 32 values.
pub(super) fn byte_count(count: usize, width: usize) -> Result<usize> {
    if width > 128 {
        return Err(corrupt("packed width exceeds 128 bits"));
    }
    count
        .div_ceil(32)
        .checked_mul(4)
        .and_then(|n| n.checked_mul(width))
        .ok_or_else(|| corrupt("packed size overflow"))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate(data: &[u8], count: usize, width: usize, query: &QueryContext) -> Result<()> {
    query.check_rows(count)?;
    if byte_count(count, width)? > data.len() {
        return Err(corrupt("truncated packed values"));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn words(
    data: &[u8],
    count: usize,
    width: usize,
    query: &QueryContext,
) -> Result<Vec<u128>> {
    validate(data, count, width, query)?;
    let mask = if width == 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    };
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        if i % 1024 == 0 {
            query.check()?;
        }
        let bit = i * width;
        let offset = bit % 8;
        let start = bit / 8;
        let length = (width + offset).div_ceil(8);
        let low_length = length.min(16);
        let mut bytes = [0; 16];
        bytes[..low_length].copy_from_slice(&data[start..start + low_length]);
        let mut value = u128::from_le_bytes(bytes) >> offset;
        if length > 16 {
            value |= u128::from(data[start + 16]) << (128 - offset);
        }
        values.push(value & mask);
    }
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn scalar(
    data: &[u8],
    count: usize,
    width: usize,
    query: &QueryContext,
) -> Result<Vec<u128>> {
    validate(data, count, width, query)?;
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        if i % 1024 == 0 {
            query.check()?;
        }
        let mut value = 0;
        for bit in 0..width {
            let position = i * width + bit;
            value |= u128::from((data[position / 8] >> (position % 8)) & 1) << bit;
        }
        values.push(value);
    }
    Ok(values)
}

#[cfg(kani)]
mod verification {
    #[kani::proof]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn kani_packed_byte_count_matches_wide_arithmetic() {
        let count: usize = kani::any();
        let width: usize = kani::any();
        let actual = super::byte_count(count, width);
        if width > 128 {
            assert!(actual.is_err());
            return;
        }
        // A wider oracle can represent the padded size even when usize cannot.
        let expected = (count as u128).div_ceil(32) * 4 * width as u128;
        match actual {
            Ok(bytes) => assert_eq!(bytes as u128, expected),
            Err(_) => assert!(expected > usize::MAX as u128),
        }
    }
}
