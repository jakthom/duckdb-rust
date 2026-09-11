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
    count: usize,
) -> Result<Vec<bool>> {
    let Some(&first) = pointers.first() else {
        return Ok(vec![false; count]);
    };
    read(&mut blocks.metadata(first)?, blocks.vector_size, count)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn read(reader: &mut Reader, vector_size: usize, count: usize) -> Result<Vec<bool>> {
    let mut deleted = vec![false; count];
    let chunks = reader.fixed_u64()?;
    if chunks > count.div_ceil(vector_size) as u64 {
        return Err(corrupt("too many deletion vectors"));
    }
    let mut visited = HashSet::new();
    for _ in 0..chunks {
        let index = reader.fixed_u64()?;
        let offset = usize::try_from(index)
            .ok()
            .and_then(|i| i.checked_mul(vector_size))
            .filter(|&i| i < count)
            .ok_or_else(|| corrupt("deletion vector outside row group"))?;
        if !visited.insert(index) {
            return Err(corrupt("duplicate deletion vector"));
        }
        let kind = reader.byte()?;
        if kind == 2 {
            continue;
        }
        // Retained compatibility field, not the address of this mask. v1.3
        // wrote absolute starts; both current pins create relative starts but
        // read/rewrite historical values unchanged. RowVersionManager indexes
        // masks by the separately validated vector index above. Checking this
        // field against either current origin would reject valid reused masks.
        reader.fixed_u64()?;
        let mask = match kind {
            0 => vec![true; vector_size],
            1 => {
                let mask = mask(reader, vector_size)?;
                if mask.iter().all(|deleted| *deleted) || mask.iter().all(|deleted| !deleted) {
                    return Err(corrupt(
                        "partial deletion mask is entirely deleted or alive",
                    ));
                }
                mask
            }
            _ => return Err(corrupt("unknown deletion vector encoding")),
        };
        let length = vector_size.min(count - offset);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn stream(index: u64, start: u64, mask_kind: u8, entries: &[u16]) -> Vec<u8> {
        let mut bytes = 1_u64.to_le_bytes().to_vec();
        bytes.extend(index.to_le_bytes());
        bytes.push(1); // VECTOR_INFO, independent of the mask's encoding.
        bytes.extend(start.to_le_bytes());
        bytes.push(mask_kind);
        bytes.extend((entries.len() as u32).to_le_bytes());
        bytes.extend(entries.iter().flat_map(|entry| entry.to_le_bytes()));
        bytes
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn deletion_addresses_use_vector_indices_not_compatibility_starts() -> Result<()> {
        for start in [2048, 124928, 17, u64::MAX] {
            let decoded = read(&mut Reader::new(stream(1, start, 1, &[17])), 2048, 4096)?;
            assert_eq!(decoded.iter().filter(|deleted| **deleted).count(), 1);
            assert!(decoded[2065]);
        }
        for (index, kind, entries) in [
            (2, 1, vec![17]),
            (u64::MAX, 1, vec![17]),
            (1, 1, vec![2048]),
            (1, 1, vec![17, 17]),
            (1, 1, vec![18, 17]),
            (1, 1, vec![]),
            (1, 2, vec![]),
            (1, 3, vec![17]),
        ] {
            assert!(matches!(
                read(
                    &mut Reader::new(stream(index, 0, kind, &entries)),
                    2048,
                    4096
                ),
                Err(Error::Corrupt(_))
            ));
        }
        let bytes = stream(1, 0, 1, &[17]);
        for length in 0..bytes.len() {
            assert!(read(&mut Reader::new(bytes[..length].to_vec()), 2048, 4096).is_err());
        }
        let mut duplicated = bytes.clone();
        duplicated[..8].copy_from_slice(&2_u64.to_le_bytes());
        duplicated.extend_from_slice(&bytes[8..]);
        assert!(matches!(
            read(&mut Reader::new(duplicated), 2048, 4096),
            Err(Error::Corrupt(_))
        ));
        Ok(())
    }
}
