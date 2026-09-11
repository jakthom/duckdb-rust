//! Bound grouping semantics shared by frontends, optimizers and algorithms.
use super::{BoundExpr, logical::AggregateExpr};
use crate::{
    common::{DataType, Error, Result},
    parallel::QueryContext,
};

/// Canonical group ordinals. Repetition inside one set collapses; repeated
/// sets in an aggregation remain independent and preserve duplicate results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupingSet(Vec<usize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl GroupingSet {
    pub fn new(indices: impl IntoIterator<Item = usize>) -> Self {
        let mut indices: Vec<_> = indices.into_iter().collect();
        indices.sort_unstable();
        indices.dedup();
        Self(indices)
    }
    pub fn indices(&self) -> &[usize] {
        &self.0
    }
    pub fn contains(&self, index: usize) -> bool {
        self.0.binary_search(&index).is_ok()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug)]
pub enum AggregateOutput {
    Function(AggregateExpr),
    /// Ordered group ordinals. The rightmost argument is the low mask bit;
    /// a bit is one when the set excludes that group, even for a stored NULL.
    Grouping(Vec<usize>),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateOutput {
    pub fn data_type(&self) -> &DataType {
        match self {
            Self::Function(expression) => &expression.data_type,
            Self::Grouping(_) => &DataType::BigInt,
        }
    }
}

/// Output columns are group values followed by outputs in declaration order.
/// Missing group values become NULL. Each set owns independent aggregate and
/// DISTINCT state; an empty set produces a row even when the input is empty.
#[derive(Clone, Debug)]
pub struct Aggregation {
    pub groups: Vec<BoundExpr>,
    pub sets: Vec<GroupingSet>,
    pub outputs: Vec<AggregateOutput>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Aggregation {
    /// Check shape before an optimizer or algorithm can index group ordinals.
    /// This does not replace expression/schema validation against the input.
    pub fn validate_metadata(&self, context: &QueryContext) -> Result<()> {
        context.check()?;
        if self.sets.is_empty() || self.sets.len() > 65535 {
            return Err(Error::Internal("invalid grouping set count".into()));
        }
        for set in &self.sets {
            context.check()?;
            if set
                .indices()
                .iter()
                .any(|&index| index >= self.groups.len())
            {
                return Err(Error::Internal(
                    "grouping set ordinal outside groups".into(),
                ));
            }
        }
        for output in &self.outputs {
            if let AggregateOutput::Grouping(indices) = output
                && (indices.is_empty()
                    || indices.len() > 63
                    || indices.iter().any(|&index| index >= self.groups.len()))
            {
                return Err(Error::Internal("invalid GROUPING arguments".into()));
            }
        }
        context.check()
    }
    pub fn functions(&self) -> impl Iterator<Item = &AggregateExpr> {
        self.outputs.iter().filter_map(|output| match output {
            AggregateOutput::Function(function) => Some(function),
            AggregateOutput::Grouping(_) => None,
        })
    }
}
