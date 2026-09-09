//! Prepare relational dependencies before evaluating the enclosing scalar value.
use super::{ExecutionContext, SubqueryRequest};
use crate::{
    common::{Result, Row, Value},
    execution::expression_executor::EvaluationContext,
    parallel::QueryContext,
    planner::{BoundExpr, ExprKind, expression::BoundSubquery},
};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

struct Binding {
    _query: Arc<BoundSubquery>,
    value: Value,
}
pub(super) struct Frame<'a, 'b> {
    context: &'a ExecutionContext<'b>,
    bindings: RefCell<BTreeMap<usize, Binding>>,
}
impl<'a, 'b> Frame<'a, 'b> {
    pub(super) fn new(context: &'a ExecutionContext<'b>) -> Self {
        Self {
            context,
            bindings: RefCell::new(BTreeMap::new()),
        }
    }
}
impl EvaluationContext for Frame<'_, '_> {
    fn query(&self) -> &QueryContext {
        self.context.query
    }
    fn outer_column(&self, depth: usize, column: usize) -> Result<Value> {
        self.context.outer_column(depth, column)
    }
    fn prepared_subquery(&self, query: &Arc<BoundSubquery>) -> Option<Value> {
        self.bindings
            .borrow()
            .get(&(Arc::as_ptr(query) as usize))
            .map(|binding| binding.value.clone())
    }
    fn subquery(
        &self,
        query: &Arc<BoundSubquery>,
        request: SubqueryRequest<'_>,
        row: &Row,
    ) -> Result<Value> {
        let key = Arc::as_ptr(query) as usize;
        if let Some(binding) = self.bindings.borrow().get(&key) {
            return Ok(binding.value.clone());
        }
        let value = self.context.subquery(query, request, row)?;
        self.bindings.borrow_mut().insert(
            key,
            Binding {
                _query: query.clone(),
                value: value.clone(),
            },
        );
        Ok(value)
    }
}

/// A borrowed expression with its relational dependencies enumerated once.
/// Ordinary scalar expressions allocate no dependency storage or row frame.
pub struct PreparedExpression<'a> {
    expression: &'a BoundExpr,
    dependencies: Vec<&'a BoundExpr>,
}
impl<'a> PreparedExpression<'a> {
    pub fn new(expression: &'a BoundExpr) -> Self {
        let mut dependencies = Vec::new();
        collect(expression, &mut dependencies);
        Self {
            expression,
            dependencies,
        }
    }
    pub fn evaluate(&self, row: &Row, context: &ExecutionContext<'_>) -> Result<Value> {
        if self.dependencies.is_empty() {
            return context.expressions.evaluate(self.expression, row, context);
        }
        let frame = Frame::new(context);
        for dependency in &self.dependencies {
            context.expressions.evaluate(dependency, row, &frame)?;
        }
        context.expressions.evaluate(self.expression, row, &frame)
    }
}
fn collect<'a>(expression: &'a BoundExpr, output: &mut Vec<&'a BoundExpr>) {
    expression.visit_children(&mut |child| collect(child, output));
    if matches!(expression.kind, ExprKind::Subquery(_)) {
        output.push(expression);
    }
}
