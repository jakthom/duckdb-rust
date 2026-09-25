//! Typed group destinations and the optional column update contract.
use crate::{
    common::{Error, Result, Value, vector::DataChunk},
    parallel::QueryContext,
};

/// One destination per argument row. Construction validates every ordinal;
/// consumers check the retained group count against their own state count.
pub struct GroupSelection<'a> {
    indices: &'a [usize],
    group_count: usize,
    counts: Option<Vec<usize>>,
    constant: Option<usize>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> GroupSelection<'a> {
    pub fn new(indices: &'a [usize], group_count: usize, query: &QueryContext) -> Result<Self> {
        query.check()?;
        let mut constant = indices.first().copied();
        if let Some(first) = constant {
            for block in indices.chunks(1024) {
                query.check()?;
                if block.iter().any(|&index| index != first) {
                    constant = None;
                    break;
                }
            }
        }
        let counts = if let Some(group) = constant {
            if group >= group_count {
                return Err(Error::Internal("aggregate group outside state".into()));
            }
            None
        } else if group_count > 0 && group_count <= indices.len() / 4 {
            // Validate while counting. The bounded histogram is reusable by
            // functions without gathering or reordering their argument rows.
            query.check_rows(group_count)?;
            let mut counts = vec![0; group_count];
            for block in indices.chunks(1024) {
                query.check()?;
                for &group in block {
                    *counts.get_mut(group).ok_or_else(|| {
                        Error::Internal("aggregate group outside state".into())
                    })? += 1;
                }
            }
            Some(counts)
        } else {
            for block in indices.chunks(1024) {
                query.check()?;
                if block.iter().any(|&index| index >= group_count) {
                    return Err(Error::Internal("aggregate group outside state".into()));
                }
            }
            None
        };
        query.check()?;
        Ok(Self {
            indices,
            group_count,
            counts,
            constant,
        })
    }
    pub fn indices(&self) -> &[usize] {
        self.indices
    }
    pub fn group_count(&self) -> usize {
        self.group_count
    }
    /// All argument rows target this group; no row selection is necessary.
    pub fn constant_group(&self) -> Option<usize> {
        self.constant
    }
    /// Cardinality per group ordinal, including zero counts, when a bounded
    /// histogram is useful. Its length is `group_count` and its sum equals the
    /// input length. A constant destination uses `constant_group` instead;
    /// sparse destinations retain only indices. Indices always preserve the
    /// original argument-row order. Consumers own iteration and cancellation.
    pub fn counts(&self) -> Option<&[usize]> {
        self.counts.as_deref()
    }
    pub fn validate(&self, arguments: &DataChunk, group_count: usize) -> Result<()> {
        if self.indices.len() != arguments.len() || self.group_count != group_count {
            return Err(Error::Internal("grouped aggregate shape mismatch".into()));
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Independently owned states, indexed by stable contiguous group ordinals.
/// Resize only grows, initializes empty groups, and preserves earlier states.
/// Input columns are logically validated and match the bound signature. Each
/// group's updates retain input order, including NULLs; relative work between
/// groups/functions may change. Opting in promises no external effects and no
/// data-dependent update errors for valid input with at most usize::MAX rows
/// per group. Resource/cancellation failures remain possible and invalidate the
/// state. An adapter must enforce its update-count bound before arithmetic can
/// rely on it. Finish consumes all state and returns one owned value per group,
/// including empty groups, or an error without publishing partial results.
pub trait GroupedAggregateState: Send {
    fn group_count(&self) -> usize;
    fn resize(&mut self, groups: usize, query: &QueryContext) -> Result<()>;
    fn update_batch(
        &mut self,
        groups: &GroupSelection<'_>,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<()>;
    fn finish(self: Box<Self>, query: &QueryContext) -> Result<Vec<Value>>;
}
