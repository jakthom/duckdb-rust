//! Consume both legacy and development string statistics. Statistics do not
//! substitute for physical string decoding or become execution pruning keys.
use super::{Reader, Result, corrupt};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(reader: &mut Reader) -> Result<()> {
    if reader.optional(200)? {
        if reader.blob()?.len() != 8 {
            return Err(corrupt("legacy string minimum statistics width"));
        }
        reader.field(201)?;
        if reader.blob()?.len() != 8 {
            return Err(corrupt("legacy string maximum statistics width"));
        }
        reader.field(202)?;
        reader.boolean()?;
        reader.field(203)?;
        reader.boolean()?;
        reader.field(204)?;
        u32::try_from(reader.unsigned()?).map_err(|_| corrupt("string maximum length overflow"))?;
    } else {
        if reader.optional(204)? {
            u32::try_from(reader.unsigned()?)
                .map_err(|_| corrupt("string maximum length overflow"))?;
        }
        let has_min_max = reader.optional(205)?;
        if has_min_max && reader.blob()?.len() != 24 {
            return Err(corrupt("packed string statistics width"));
        }
        reader.field(206)?;
        let flags = u32::try_from(reader.unsigned()?)
            .map_err(|_| corrupt("packed string statistics overflow"))?;
        // StringStatsField dedicates bits 3..6 and 7..10 to two lengths;
        // every other bit has defined semantics, so there is no reserved mask.
        if has_min_max && (((flags >> 3) & 15) > 12 || ((flags >> 7) & 15) > 12) {
            return Err(corrupt("packed string statistics length exceeds 12"));
        }
        if reader.optional(207)? {
            reader.unsigned()?; // optional_idx: UINT64_MAX means unavailable.
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::duckdb::binary::Encoder;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn string_statistics_bound_packed_blobs_lengths_and_integer_widths() -> Result<()> {
        for (bytes, flags, accepted) in [
            (24, 12_u64 << 3 | 12 << 7, true),
            (23, 0, false),
            (25, 0, false),
            (24, 13 << 3, false),
            (24, 13 << 7, false),
            (24, 1 << 32, false),
        ] {
            let mut output = Encoder::default();
            output.property(204, 42);
            output.field(205);
            output.blob(&vec![0; bytes]);
            output.property(206, flags);
            output.property(207, u64::MAX);
            output.end();
            let mut reader = Reader::new(output.0);
            assert_eq!(
                read(&mut reader).is_ok(),
                accepted,
                "bytes={bytes}, flags={flags}"
            );
            if accepted {
                reader.end()?;
                assert!(reader.finished());
            }
        }
        let mut output = Encoder::default();
        output.property(206, 7);
        output.end();
        let mut reader = Reader::new(output.0);
        read(&mut reader)?;
        reader.end()?;
        Ok(())
    }
}
