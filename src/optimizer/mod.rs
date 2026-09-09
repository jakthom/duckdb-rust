mod key_lookup;
pub use key_lookup::UseKeyLookup;
mod simplify_expressions;
pub use simplify_expressions::SimplifyExpressions;
mod validated_plan;
pub use validated_plan::ValidatedPlan;

use crate::{
    catalog::Catalog,
    common::{Result, Value},
    parallel::QueryContext,
    planner::{BoundExpr, ExprKind, LogicalPlan, PlanNode},
    storage::TableStorage,
};
use std::sync::Arc;

/// All observations belong to the same statement snapshot. Passes must not
/// retain these references or infer capabilities from a concrete adapter type.
pub struct OptimizerContext<'a> {
    pub catalog: &'a dyn Catalog,
    pub storage: &'a dyn TableStorage,
    pub query: &'a QueryContext,
}

/// Owns its input and preserves schema, effects, errors and visibility. Input
/// and output use the same validation context; adapters transform plans through
/// ValidatedPlan::rewrite rather than transferring unvalidated mutable state.
pub trait Optimizer: Send + Sync {
    fn name(&self) -> &'static str;
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        vec![("optimizer", self.name())]
    }
    fn optimize<'a>(&self, plan: ValidatedPlan<'a>) -> Result<ValidatedPlan<'a>>;
}

/// A local rewrite after the pipeline has visited this node's inputs. Each pass
/// must remain valid when run alone or with any other conforming pass sequence.
pub trait OptimizerPass: Send + Sync {
    fn name(&self) -> &'static str;
    fn rewrite(&self, plan: LogicalPlan, context: &OptimizerContext<'_>) -> Result<LogicalPlan>;
}

#[derive(Default)]
pub struct IdentityOptimizer;
impl Optimizer for IdentityOptimizer {
    fn name(&self) -> &'static str {
        "identity"
    }
    fn optimize<'a>(&self, plan: ValidatedPlan<'a>) -> Result<ValidatedPlan<'a>> {
        plan.context().query.check()?;
        Ok(plan)
    }
}

pub struct PipelineOptimizer {
    passes: Vec<Arc<dyn OptimizerPass>>,
}

impl Default for PipelineOptimizer {
    fn default() -> Self {
        Self::new(vec![Arc::new(SimplifyExpressions), Arc::new(UseKeyLookup)])
    }
}

impl PipelineOptimizer {
    pub fn new(passes: Vec<Arc<dyn OptimizerPass>>) -> Self {
        Self { passes }
    }
}

impl Optimizer for PipelineOptimizer {
    fn name(&self) -> &'static str {
        "pipeline"
    }
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let mut adapters = vec![("optimizer", self.name())];
        adapters.extend(
            self.passes
                .iter()
                .map(|pass| ("optimizer_pass", pass.name())),
        );
        adapters
    }
    fn optimize<'a>(&self, mut plan: ValidatedPlan<'a>) -> Result<ValidatedPlan<'a>> {
        plan.context().query.check()?;
        for pass in &self.passes {
            plan = plan.rewrite(|plan, context| visit(plan, pass.as_ref(), context))?;
        }
        Ok(plan)
    }
}

fn visit(
    plan: LogicalPlan,
    pass: &dyn OptimizerPass,
    context: &OptimizerContext<'_>,
) -> Result<LogicalPlan> {
    context.query.check()?;
    let plan = plan.map_inputs(|input| visit(input, pass, context))?;
    let plan = plan.map_expressions(|expr| visit_expression(expr, pass, context))?;
    pass.rewrite(plan, context)
}

fn visit_expression(
    expression: BoundExpr,
    pass: &dyn OptimizerPass,
    context: &OptimizerContext<'_>,
) -> Result<BoundExpr> {
    let mut expression = expression.map_children(|child| visit_expression(child, pass, context))?;
    if let ExprKind::Subquery(query) = &mut expression.kind {
        let query = Arc::make_mut(query);
        query.plan = Arc::new(visit(query.plan.as_ref().clone(), pass, context)?);
    }
    Ok(expression)
}

pub struct RemoveTrueFilters;
impl OptimizerPass for RemoveTrueFilters {
    fn name(&self) -> &'static str {
        "remove-true-filters"
    }
    fn rewrite(&self, plan: LogicalPlan, _: &OptimizerContext<'_>) -> Result<LogicalPlan> {
        let node = match plan.node {
            PlanNode::Filter {
                input,
                predicate:
                    BoundExpr {
                        kind: ExprKind::Literal(Value::Boolean(true)),
                        ..
                    },
            } => input.node,
            node => node,
        };
        Ok(LogicalPlan {
            schema: plan.schema,
            node,
        })
    }
}
