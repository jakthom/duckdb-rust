//! Validate physical cardinality independently of persisted row-ID intervals.
use super::corrupt;
use crate::Result;

pub(super) struct RowIdentity {
    version: u64,
    remaining: usize,
    next: u64,
    end: u64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RowIdentity {
    pub(super) fn new(version: u64, total: usize, next: u64) -> Result<Self> {
        if next < total as u64 || (version < 69 && next != total as u64) {
            return Err(corrupt(
                "table append identity differs from storage capabilities",
            ));
        }
        Ok(Self {
            version,
            remaining: total,
            next,
            end: 0,
        })
    }

    pub(super) fn push(&mut self, start: u64, count: usize) -> Result<usize> {
        if count > self.remaining {
            return Err(corrupt("row group exceeds table count"));
        }
        let end = start
            .checked_add(count as u64)
            .filter(|end| *end <= self.next)
            .ok_or_else(|| corrupt("row group exceeds append identity"))?;
        if start < self.end || (self.version < 69 && start != self.end) {
            return Err(corrupt(
                "row group identities overlap, regress or violate storage version",
            ));
        }
        let row_start =
            usize::try_from(start).map_err(|_| corrupt("row group identity overflows platform"))?;
        usize::try_from(end).map_err(|_| corrupt("row group end overflows platform"))?;
        self.remaining -= count;
        self.end = end;
        Ok(row_start)
    }

    pub(super) fn finish(self) -> Result<()> {
        if self.remaining != 0 {
            return Err(corrupt("table row count mismatch"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn row_identity_validates_cardinality_ranges_capability_and_append_watermark() -> Result<()> {
        for (version, total, next, groups, accepted) in [
            (69, 3, 11, vec![(3, 1), (9, 2)], true),
            (69, 0, 11, vec![], true),
            (69, 0, 11, vec![(11, 0)], true),
            (69, 3, 11, vec![(3, 2), (4, 1)], false),
            (69, 2, 11, vec![(9, 1), (3, 1)], false),
            (69, 3, 11, vec![(9, 3)], false),
            (69, 3, 11, vec![(3, 2)], false),
            (69, 3, 11, vec![(3, 4)], false),
            (69, 2, u64::MAX, vec![(u64::MAX, 2)], false),
            (69, 2, 1, vec![], false),
            (68, 3, 11, vec![(3, 1), (9, 2)], false),
            (68, 0, 11, vec![], false),
            (68, 3, 3, vec![(0, 1), (1, 2)], true),
            (64, 3, 3, vec![(1, 1), (2, 2)], false),
        ] {
            let outcome = RowIdentity::new(version, total, next).and_then(|mut ids| {
                for &(start, count) in &groups {
                    ids.push(start, count)?;
                }
                ids.finish()
            });
            assert_eq!(
                outcome.is_ok(),
                accepted,
                "v{version} total={total} next={next} groups={groups:?}"
            );
            if !accepted {
                assert!(matches!(outcome, Err(Error::Corrupt(_))));
            }
        }
        // Invalid intervals do not partially advance the physical count or ID.
        let mut ids = RowIdentity::new(69, 2, 12)?;
        assert!(ids.push(12, 2).is_err());
        ids.push(10, 2)?;
        ids.finish()
    }
}
