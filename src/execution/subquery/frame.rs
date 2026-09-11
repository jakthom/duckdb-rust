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
    query: Arc<BoundSubquery>,
    value: Value,
}
#[derive(Default)]
struct Bindings {
    first: Option<Binding>,
    others: BTreeMap<usize, Binding>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Bindings {
    fn get(&self, key: usize) -> Option<Value> {
        self.first
            .as_ref()
            .filter(|binding| Arc::as_ptr(&binding.query) as usize == key)
            .or_else(|| self.others.get(&key))
            .map(|binding| binding.value.clone())
    }
    fn insert(&mut self, key: usize, binding: Binding) {
        if self.first.is_none() {
            self.first = Some(binding);
        } else {
            self.others.insert(key, binding);
        }
    }
}
pub(super) struct Frame<'a, 'b> {
    context: &'a ExecutionContext<'b>,
    bindings: RefCell<Bindings>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a, 'b> Frame<'a, 'b> {
    pub(super) fn new(context: &'a ExecutionContext<'b>) -> Self {
        Self {
            context,
            bindings: RefCell::new(Bindings::default()),
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl EvaluationContext for Frame<'_, '_> {
    fn query(&self) -> &QueryContext {
        self.context.query
    }
    fn outer_column(&self, depth: usize, column: usize) -> Result<Value> {
        self.context.outer_column(depth, column)
    }
    fn prepared_subquery(&self, query: &Arc<BoundSubquery>) -> Option<Value> {
        self.bindings.borrow().get(Arc::as_ptr(query) as usize)
    }
    fn subquery_provenance(
        &self,
        query: &Arc<BoundSubquery>,
    ) -> crate::function::ArgumentProvenance {
        self.context.subquery_provenance(query)
    }
    fn subquery(
        &self,
        query: &Arc<BoundSubquery>,
        request: SubqueryRequest<'_>,
        row: &Row,
    ) -> Result<Value> {
        let key = Arc::as_ptr(query) as usize;
        if let Some(value) = self.bindings.borrow().get(key) {
            return Ok(value);
        }
        let value = self.context.subquery(query, request, row)?;
        self.bindings.borrow_mut().insert(
            key,
            Binding {
                query: query.clone(),
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> PreparedExpression<'a> {
    pub fn uniform_selection(
        &self,
        input: &crate::common::vector::DataChunk,
        context: &ExecutionContext<'_>,
    ) -> Result<Option<bool>> {
        if self.expression.data_type != crate::DataType::Boolean {
            return Err(crate::Error::Internal(
                "filter predicate must be Boolean".into(),
            ));
        }
        if input.len() < 2
            || !self.dependencies.is_empty()
            || context
                .query
                .types()
                .bind(&crate::DataType::Boolean)?
                .requires_logical_validation()
        {
            return Ok(None);
        }
        let result = context
            .expressions
            .uniform_selection(self.expression, input, context);
        context.query.check()?;
        result
    }
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
    pub fn select(&self, row: &Row, context: &ExecutionContext<'_>) -> Result<bool> {
        if self.dependencies.is_empty() {
            return context.expressions.select(self.expression, row, context);
        }
        let frame = Frame::new(context);
        for dependency in &self.dependencies {
            context.expressions.evaluate(dependency, row, &frame)?;
        }
        context.expressions.select(self.expression, row, &frame)
    }
    /// Preserve the current input's encoding while keeping relational
    /// preparation and selected child evaluation in the ordinary row order.
    pub fn evaluate_with_provenance(
        &self,
        row: &Row,
        input: &crate::common::vector::DataChunk,
        context: &ExecutionContext<'_>,
    ) -> Result<super::super::expression_executor::EvaluatedValue> {
        use super::super::expression_executor::BatchContext;
        if self.dependencies.is_empty() {
            let batch = BatchContext {
                parent: context,
                input,
            };
            return context
                .expressions
                .evaluate_with_provenance(self.expression, row, &batch);
        }
        let frame = Frame::new(context);
        let batch = BatchContext {
            parent: &frame,
            input,
        };
        for dependency in &self.dependencies {
            context.expressions.evaluate(dependency, row, &batch)?;
        }
        context
            .expressions
            .evaluate_with_provenance(self.expression, row, &batch)
    }
    pub fn evaluate_batch(
        &self,
        input: &crate::common::vector::DataChunk,
        context: &ExecutionContext<'_>,
    ) -> Result<crate::common::vector::Vector> {
        let output = if self.dependencies.is_empty() {
            context
                .expressions
                .evaluate_batch(self.expression, input, context)?
        } else {
            let mut row = Vec::with_capacity(input.columns().len());
            let mut values = Vec::with_capacity(input.len());
            for index in 0..input.len() {
                input.read_row(index, &mut row)?;
                values.push(self.evaluate_with_provenance(&row, input, context)?);
            }
            super::super::expression_executor::result_column(
                self.expression.data_type.clone(),
                values,
                context.query,
            )?
        };
        if output.len() != input.len() {
            return Err(crate::Error::Internal(
                "expression batch cardinality differs from input".into(),
            ));
        }
        context
            .query
            .types()
            .bind(&self.expression.data_type)?
            .validate_vector(&output, context.query)?;
        Ok(output)
    }
    pub fn select_batch(
        &self,
        input: &crate::common::vector::DataChunk,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<usize>> {
        use crate::common::{DataType, Error};
        if self.expression.data_type != DataType::Boolean {
            return Err(Error::Internal("filter predicate must be Boolean".into()));
        }
        let boolean = context.query.types().bind(&DataType::Boolean)?;
        let selected = if self.dependencies.is_empty() && !boolean.requires_logical_validation() {
            context
                .expressions
                .select_batch(self.expression, input, context)?
        } else {
            super::super::expression_executor::select_boolean(
                &self.evaluate_batch(input, context)?,
                input.len(),
                context.query,
            )?
        };
        if selected.last().is_some_and(|index| *index >= input.len())
            || selected.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(Error::Internal(
                "predicate selection must contain ordered distinct input positions".into(),
            ));
        }
        context.query.check()?;
        Ok(selected)
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn collect<'a>(expression: &'a BoundExpr, output: &mut Vec<&'a BoundExpr>) {
    expression.visit_children(&mut |child| collect(child, output));
    if matches!(expression.kind, ExprKind::Subquery(_)) {
        output.push(expression);
    }
}
