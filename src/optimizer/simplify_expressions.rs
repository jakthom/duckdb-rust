use super::{OptimizerContext, OptimizerPass, RemoveTrueFilters};
use crate::{
    common::Result,
    parallel::QueryContext,
    planner::{BoundExpr, ExprKind, LogicalPlan},
};

/// Simplify pure expressions and remove filters that are known to be true.
/// Constant conversions and pure operators use their retained adapters.
/// Casts are pure by contract; operator effects must permit folding. A failed
/// attempt leaves the expression intact:
/// CASE, short-circuit functions and empty inputs must retain their error timing.
pub struct SimplifyExpressions;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OptimizerPass for SimplifyExpressions {
    fn name(&self) -> &'static str {
        "simplify-expressions"
    }
    fn rewrite(&self, plan: LogicalPlan, context: &OptimizerContext<'_>) -> Result<LogicalPlan> {
        let plan = plan.map_expressions(|expression| fold(expression, context.query))?;
        RemoveTrueFilters.rewrite(plan, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fold(expression: BoundExpr, query: &QueryContext) -> Result<BoundExpr> {
    query.check()?;
    let mut expression = expression.map_children(|child| fold(child, query))?;
    if let ExprKind::Cast(inner, cast, _) = &expression.kind
        && let Some(value) = inner.constant_value()
        && let Ok(value) = cast.apply(value, query)
    {
        expression.kind = ExprKind::Literal(value);
    }
    if let ExprKind::Operator(function, arguments) = &expression.kind {
        let effects = function.effects();
        if !effects.volatile && !effects.external_access {
            let values: Option<Vec<_>> = arguments
                .iter()
                .map(|argument| argument.constant_value().cloned())
                .collect();
            if let Some(values) = values
                && let Ok(value) = function.apply(&values, query)
            {
                expression.kind = ExprKind::Literal(value);
            }
        }
    }
    query.check()?;
    Ok(expression)
}
