//! C++ may truncate free tail blocks after publishing its database header.
//! The header's allocation watermark can exceed the remaining physical file.
use super::{Blocks, Reader, corrupt};
use crate::{Error, Result};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Blocks {
    pub(super) fn validate_free_tail(&self, pointer: u64) -> Result<()> {
        let payload = self.bytes.len() - 12288;
        let physical = (payload / self.block_size) as u64;
        if physical >= self.block_count {
            return Ok(());
        }
        if !payload.is_multiple_of(self.block_size) || pointer == u64::MAX {
            return Err(corrupt(
                "truncated block storage without a complete free tail",
            ));
        }
        // Metadata chains and every subsequently referenced data block still
        // require present, checksummed blocks. Never synthesize missing bytes.
        validate(
            &mut self.metadata((pointer, 0))?,
            physical,
            self.block_count,
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate(reader: &mut Reader, physical: u64, allocated: u64) -> Result<()> {
    let count = reader.fixed_u64()?;
    if count > 16_777_216 {
        return Err(Error::Resource(
            "native free list exceeds 16 million blocks".into(),
        ));
    }
    if count < allocated - physical {
        return Err(corrupt("truncated allocated blocks are not all free"));
    }
    let mut previous = None;
    let mut missing = physical;
    for _ in 0..count {
        let id = reader.fixed_u64()?;
        if id >= allocated || previous.is_some_and(|previous| id <= previous) {
            return Err(corrupt("invalid free block identity or ordering"));
        }
        if id >= physical {
            if id != missing {
                return Err(corrupt("truncated allocated block is not free"));
            }
            missing += 1;
        }
        previous = Some(id);
    }
    if missing != allocated {
        return Err(corrupt("truncated allocated block is not free"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn truncated_tail_requires_exact_bounded_ordered_free_block_coverage() {
        for (free, accepted) in [
            (vec![3_u64, 4], true),
            (vec![1, 3, 4], true),
            (vec![], false),
            (vec![4], false),
            (vec![2, 3], false),
            (vec![3, 3, 4], false),
            (vec![4, 3], false),
            (vec![3, 4, 5], false),
            (vec![3, u64::MAX], false),
        ] {
            let mut bytes = (free.len() as u64).to_le_bytes().to_vec();
            bytes.extend(free.iter().flat_map(|id| id.to_le_bytes()));
            assert_eq!(
                validate(&mut Reader::new(bytes), 3, 5).is_ok(),
                accepted,
                "{free:?}"
            );
        }
        assert!(matches!(
            validate(
                &mut Reader::new(16_777_217_u64.to_le_bytes().to_vec()),
                3,
                5
            ),
            Err(Error::Resource(_))
        ));
        assert!(matches!(
            validate(&mut Reader::new(2_u64.to_le_bytes().to_vec()), 3, 5),
            Err(Error::Corrupt(_))
        ));
        // A forged allocation watermark cannot induce a proportional loop.
        assert!(matches!(
            validate(&mut Reader::new(2_u64.to_le_bytes().to_vec()), 3, u64::MAX),
            Err(Error::Corrupt(_))
        ));
    }
}
