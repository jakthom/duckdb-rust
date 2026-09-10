use std::collections::HashSet;

use super::{
    Blocks,
    binary::{Reader, corrupt},
};
use crate::common::Result;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Persisted masks contain committed deletions only. A set bit means deleted.
pub(super) fn deleted_rows(
    blocks: &Blocks,
    pointers: &[(u64, usize)],
    start: usize,
    count: usize,
) -> Result<Vec<bool>> {
    let mut deleted = vec![false; count];
    let Some(&first) = pointers.first() else {
        return Ok(deleted);
    };
    let mut reader = blocks.metadata(first)?;
    let chunks = reader.fixed_u64()?;
    if chunks > count.div_ceil(blocks.vector_size) as u64 {
        return Err(corrupt("too many deletion vectors"));
    }
    let mut visited = HashSet::new();
    for _ in 0..chunks {
        let index = reader.fixed_u64()?;
        let offset = usize::try_from(index)
            .ok()
            .and_then(|i| i.checked_mul(blocks.vector_size))
            .filter(|&i| i < count)
            .ok_or_else(|| corrupt("deletion vector outside row group"))?;
        if !visited.insert(index) {
            return Err(corrupt("duplicate deletion vector"));
        }
        let kind = reader.byte()?;
        if kind == 2 {
            continue;
        }
        if reader.fixed_u64()? != (start + offset) as u64 {
            return Err(corrupt("deletion vector identity differs from row group"));
        }
        let mask = match kind {
            0 => vec![true; blocks.vector_size],
            1 => mask(&mut reader, blocks.vector_size)?,
            _ => return Err(corrupt("unknown deletion vector encoding")),
        };
        let length = blocks.vector_size.min(count - offset);
        deleted[offset..offset + length].copy_from_slice(&mask[..length]);
    }
    Ok(deleted)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn mask(reader: &mut Reader, count: usize) -> Result<Vec<bool>> {
    let kind = reader.byte()?;
    if kind == 0 {
        let bytes = reader.bytes(count.div_ceil(64) * 8)?;
        return Ok((0..count)
            .map(|i| bytes[i / 8] & (1 << (i % 8)) != 0)
            .collect());
    }
    if kind != 1 && kind != 2 {
        return Err(corrupt("unknown deletion mask encoding"));
    }
    let entries = u32::from_le_bytes(
        reader
            .bytes(4)?
            .try_into()
            .map_err(|_| corrupt("mask count"))?,
    ) as usize;
    if entries > count {
        return Err(corrupt("deletion mask count exceeds vector"));
    }
    let mut mask = vec![kind == 2; count];
    let mut previous = None;
    for _ in 0..entries {
        let index = if count >= u16::MAX as usize {
            u32::from_le_bytes(
                reader
                    .bytes(4)?
                    .try_into()
                    .map_err(|_| corrupt("mask index"))?,
            ) as usize
        } else {
            u16::from_le_bytes(
                reader
                    .bytes(2)?
                    .try_into()
                    .map_err(|_| corrupt("mask index"))?,
            ) as usize
        };
        if index >= count || previous.is_some_and(|p| index <= p) {
            return Err(corrupt("invalid deletion mask index"));
        }
        previous = Some(index);
        mask[index] = kind == 1;
    }
    Ok(mask)
}
