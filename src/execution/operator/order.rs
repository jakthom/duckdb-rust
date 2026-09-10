//! Replaceable blocking sort algorithms over the ordinary stream contract.
mod comparison;
mod radix;
pub use comparison::ComparisonSort;
pub use radix::RadixSort;

use crate::{
    common::{Result, Row},
    execution::{ExecutionContext, stream::BatchStream},
    planner::logical::OrderExpr,
};
use std::fmt::Debug;

/// Consume a validated input stream once, evaluate each sort expression once
/// per input row, and return owned rows in lexicographic order. NULL placement
/// is independent of direction. Ties retain input order. Evaluation effects and
/// errors retain row order unless every expression is proved pure and total.
/// Each invocation owns its state; failure or cancellation returns no partial
/// result. Input and permutation cardinality obey the query row budget. Byte
/// accounting and spill remain separate, unfinished resource capabilities.
pub trait SortAlgorithm: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn sort(
        &self,
        input: &mut dyn BatchStream,
        order: &[OrderExpr],
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>>;
}
