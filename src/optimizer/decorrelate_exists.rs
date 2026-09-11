use super::*;
use crate::planner::{
    expression::{BinaryOp, SubqueryKind},
    logical::JoinKind,
};

/// Converts a direct EXISTS equality filter into a semi/anti join. Eligibility
/// requires two plain scans, a pure total key from the immediate outer scope,
/// and exact snapshot cardinalities within the materialization budget. Other
/// shapes retain dependent execution, including LIMIT, local residuals, effects,
/// throwing casts/arithmetic and deeper correlations.
pub struct DecorrelateExists;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OptimizerPass for DecorrelateExists {
    fn name(&self) -> &'static str {
        "decorrelate-exists"
    }
    fn rewrite(&self, plan: LogicalPlan, context: &OptimizerContext<'_>) -> Result<LogicalPlan> {
        context.query.check()?;
        let PlanNode::Filter {
            input: outer,
            predicate,
        } = &plan.node
        else {
            return Ok(plan);
        };
        let (PlanNode::Scan(outer_table), ExprKind::Subquery(query)) =
            (&outer.node, &predicate.kind)
        else {
            return Ok(plan);
        };
        let SubqueryKind::Exists { negated } = query.kind else {
            return Ok(plan);
        };
        let PlanNode::Filter {
            input: inner,
            predicate: condition,
        } = &query.plan.node
        else {
            return Ok(plan);
        };
        let (PlanNode::Scan(inner_table), ExprKind::Binary(BinaryOp::Equal, a, b, data_type)) =
            (&inner.node, &condition.kind)
        else {
            return Ok(plan);
        };
        let Some((local, captured)) = [(a, b), (b, a)].into_iter().find(|(local, captured)| {
            matches!(local.kind, ExprKind::Column(_)) && captured_key(captured)
        }) else {
            return Ok(plan);
        };
        let (Some(outer_count), Some(inner_count)) = (
            context.storage.row_count(outer_table.name())?,
            context.storage.row_count(inner_table.name())?,
        ) else {
            return Ok(plan);
        };
        context.query.check()?;
        // Both sides fit even when the selected join adapter materializes. A
        // one-row outer input retains demand-driven dependent execution.
        let budget = context.query.max_intermediate_rows();
        if outer_count <= 1 || outer_count > budget || inner_count > budget {
            return Ok(plan);
        }
        let ExprKind::Column(column) = local.kind else {
            unreachable!("eligible local key")
        };
        let condition = BoundExpr {
            data_type: predicate.data_type.clone(),
            kind: ExprKind::Binary(
                BinaryOp::Equal,
                Box::new(rebase(captured.as_ref().clone())?),
                Box::new(BoundExpr::column(
                    outer.schema.len() + column,
                    local.data_type.clone(),
                )),
                data_type.clone(),
            ),
        };
        Ok(LogicalPlan {
            schema: plan.schema,
            node: PlanNode::Join {
                left: outer.clone(),
                right: inner.clone(),
                kind: if negated {
                    JoinKind::Anti
                } else {
                    JoinKind::Semi
                },
                condition,
            },
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn captured_key(expression: &BoundExpr) -> bool {
    fn scope(expression: &BoundExpr, captured: &mut bool) -> bool {
        match expression.kind {
            ExprKind::Column(_) | ExprKind::Subquery(_) => return false,
            ExprKind::OuterColumn { depth: 1, .. } => *captured = true,
            ExprKind::OuterColumn { .. } => return false,
            _ => (),
        }
        let mut valid = true;
        expression.visit_children(&mut |child| valid &= scope(child, captured));
        valid
    }
    let mut captured = false;
    expression.is_pure_and_total() && scope(expression, &mut captured) && captured
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn rebase(expression: BoundExpr) -> Result<BoundExpr> {
    let mut expression = expression.map_children(rebase)?;
    if let ExprKind::OuterColumn { depth: 1, column } = expression.kind {
        expression.kind = ExprKind::Column(column);
    }
    Ok(expression)
}
