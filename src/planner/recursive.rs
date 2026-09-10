use std::sync::Arc;

use super::{BoundExpr, ExprKind, LogicalPlan, PlanNode};

/// Lexical identity of a recursive relation. Cloned plans retain the binding;
/// independently bound relations never alias, even when their SQL names match.
#[derive(Clone, Debug, Default)]
pub struct RecursiveId(Arc<()>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PartialEq for RecursiveId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Eq for RecursiveId {}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl LogicalPlan {
    pub(crate) fn references_recursive(&self, id: &RecursiveId) -> bool {
        let mut found = matches!(&self.node, PlanNode::RecursiveInput(input) if input == id);
        self.visit_inputs(&mut |input| found |= input.references_recursive(id));
        self.visit_expressions(&mut |expr| found |= references_expression(expr, id));
        found
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn references_expression(expr: &BoundExpr, id: &RecursiveId) -> bool {
    let mut found =
        matches!(&expr.kind, ExprKind::Subquery(query) if query.plan.references_recursive(id));
    expr.visit_children(&mut |child| found |= references_expression(child, id));
    found
}
