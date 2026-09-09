use crate::{
    common::{Result, type_registry::BoundType},
    planner::{BoundExpr, ExprKind, expression::BinaryOp},
};
use std::sync::Arc;

/// Each expression is local to its own input after rebasing. The equality type
/// retains the selected adapter; canonical keys must implement that equality.
pub(super) struct EqualityKeys {
    pub left: BoundExpr,
    pub right: BoundExpr,
    pub data_type: Arc<BoundType>,
}

impl EqualityKeys {
    pub fn bind(condition: &BoundExpr, left_width: usize) -> Option<Self> {
        let ExprKind::Binary(BinaryOp::Equal, a, b, data_type) = &condition.kind else {
            return None;
        };
        if a.data_type != b.data_type || !a.is_pure_and_total() || !b.is_pure_and_total() {
            return None;
        }
        let (left, right) = [(a, b), (b, a)].into_iter().find(|(left, right)| {
            local(left, 0, left_width) && local(right, left_width, usize::MAX)
        })?;
        Some(Self {
            left: left.as_ref().clone(),
            right: rebase(right.as_ref().clone(), left_width).ok()?,
            data_type: data_type.clone(),
        })
    }
}

fn local(expression: &BoundExpr, start: usize, end: usize) -> bool {
    fn visit(expression: &BoundExpr, start: usize, end: usize, found: &mut bool) -> bool {
        match expression.kind {
            ExprKind::Column(column) => {
                *found = true;
                return (start..end).contains(&column);
            }
            ExprKind::OuterColumn { .. } | ExprKind::Subquery(_) => return false,
            _ => (),
        }
        let mut valid = true;
        expression.visit_children(&mut |child| valid &= visit(child, start, end, found));
        valid
    }
    let mut found = false;
    visit(expression, start, end, &mut found) && found
}

fn rebase(expression: BoundExpr, offset: usize) -> Result<BoundExpr> {
    let mut expression = expression.map_children(|child| rebase(child, offset))?;
    if let ExprKind::Column(column) = &mut expression.kind {
        *column -= offset;
    }
    Ok(expression)
}
