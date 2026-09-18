use super::OptimizerContext;
use crate::{
    common::{Error, Result},
    planner::LogicalPlan,
};

/// An owned, immutable logical plan validated against one statement context.
/// The borrowed context fixes catalog visibility, access capabilities and type
/// selection for its lifetime. There is no unchecked constructor or mutable
/// plan access. Rewrites consume ownership and validate their complete output;
/// unchanged plans may pass through without repeating validation.
pub struct ValidatedPlan<'a> {
    plan: LogicalPlan,
    context: &'a OptimizerContext<'a>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> ValidatedPlan<'a> {
    pub fn new(plan: LogicalPlan, context: &'a OptimizerContext<'a>) -> Result<Self> {
        context.query.check()?;
        let result = plan.validate(context.catalog, context.query);
        context.query.check()?;
        result?;
        Ok(Self { plan, context })
    }
    pub fn plan(&self) -> &LogicalPlan {
        &self.plan
    }
    pub fn context(&self) -> &'a OptimizerContext<'a> {
        self.context
    }
    pub fn rewrite(
        self,
        rewrite: impl FnOnce(LogicalPlan, &OptimizerContext<'_>) -> Result<LogicalPlan>,
    ) -> Result<Self> {
        self.context.query.check()?;
        let plan = rewrite(self.plan, self.context)?;
        Self::new(plan, self.context)
    }
    /// Release the owned result only to the same context object that supplied
    /// the input. A plan validated against another snapshot is not transferable
    /// without validation there, even if its schema happens to match.
    pub fn into_plan(self, expected: &OptimizerContext<'_>) -> Result<LogicalPlan> {
        if !std::ptr::eq(self.context, expected) {
            return Err(Error::Internal(
                "optimizer changed the statement context".into(),
            ));
        }
        expected.query.check()?;
        Ok(self.plan)
    }
}
