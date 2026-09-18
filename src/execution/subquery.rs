//! Relational expression reduction, independent of binding and physical planning.
mod frame;
use super::{
    ExecutionContext,
    physical_plan::{PhysicalOperator, PhysicalPlanner},
    stream,
};
use crate::{
    common::{Error, Result, Value, type_registry::BoundType},
    parallel::QueryContext,
    planner::LogicalPlan,
};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

pub use frame::PreparedExpression;

/// Statement-local compilation state bound to one selected planner. Keys retain
/// the immutable logical Arc, preventing pointer reuse. Only physical plans are
/// shared: correlated evaluations open fresh streams with current outer rows.
/// Uncorrelated scalar/EXISTS values initialize once per statement. Row frames,
/// failures and values from earlier statements are never retained.
pub struct PreparedSubqueries<'a> {
    planner: &'a dyn PhysicalPlanner,
    plans: RefCell<BTreeMap<usize, PreparedPlan>>,
    values: RefCell<BTreeMap<usize, PreparedValue>>,
}
struct PreparedPlan {
    _logical: Arc<LogicalPlan>,
    physical: Arc<dyn PhysicalOperator>,
    correlated: bool,
}
struct PreparedValue {
    _query: Arc<crate::planner::expression::BoundSubquery>,
    value: Value,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> PreparedSubqueries<'a> {
    pub fn new(planner: &'a dyn PhysicalPlanner) -> Self {
        Self {
            planner,
            plans: RefCell::new(BTreeMap::new()),
            values: RefCell::new(BTreeMap::new()),
        }
    }
    pub(super) fn prepare(&self, plan: &Arc<LogicalPlan>) -> Result<Arc<dyn PhysicalOperator>> {
        let key = Arc::as_ptr(plan) as usize;
        if let Some(cached) = self.plans.borrow().get(&key) {
            return Ok(cached.physical.clone());
        }
        let physical = self.planner.plan(plan)?;
        if !physical
            .schema()
            .iter()
            .map(|f| &f.data_type)
            .eq(plan.schema.iter().map(|f| &f.data_type))
        {
            return Err(Error::Internal(
                "subquery physical schema differs from its logical plan".into(),
            ));
        }
        self.plans.borrow_mut().insert(
            key,
            PreparedPlan {
                _logical: plan.clone(),
                physical: physical.clone(),
                correlated: correlated(plan, 0),
            },
        );
        Ok(physical)
    }
    pub(super) fn value(
        &self,
        query: &Arc<crate::planner::expression::BoundSubquery>,
    ) -> Option<Value> {
        self.values
            .borrow()
            .get(&(Arc::as_ptr(query) as usize))
            .map(|entry| entry.value.clone())
    }
    pub(super) fn has_value(&self, query: &Arc<crate::planner::expression::BoundSubquery>) -> bool {
        self.values
            .borrow()
            .contains_key(&(Arc::as_ptr(query) as usize))
    }
    pub(super) fn retain(
        &self,
        query: &Arc<crate::planner::expression::BoundSubquery>,
        value: &Value,
    ) {
        use crate::planner::expression::SubqueryKind;
        if matches!(query.kind, SubqueryKind::In { .. }) {
            return;
        }
        if self
            .plans
            .borrow()
            .get(&(Arc::as_ptr(&query.plan) as usize))
            .is_some_and(|plan| !plan.correlated)
        {
            self.values.borrow_mut().insert(
                Arc::as_ptr(query) as usize,
                PreparedValue {
                    _query: query.clone(),
                    value: value.clone(),
                },
            );
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn correlated(plan: &LogicalPlan, local_depth: usize) -> bool {
    // Iteration inputs change without a lexical outer-row reference. Their
    // scalar reductions must never be cached across recursive generations.
    let mut found = matches!(plan.node, crate::planner::PlanNode::RecursiveInput(_));
    plan.visit_inputs(&mut |input| found |= correlated(input, local_depth));
    plan.visit_expressions(&mut |expr| found |= correlated_expression(expr, local_depth));
    found
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn correlated_expression(expr: &crate::planner::BoundExpr, local_depth: usize) -> bool {
    use crate::planner::ExprKind;
    let mut found = match &expr.kind {
        ExprKind::OuterColumn { depth, .. } => *depth > local_depth,
        ExprKind::Subquery(query) => correlated(&query.plan, local_depth + 1),
        _ => false,
    };
    expr.visit_children(&mut |child| found |= correlated_expression(child, local_depth));
    found
}

/// Borrowed, typed request. Values and type adapters remain valid for the call.
pub enum SubqueryRequest<'a> {
    Scalar,
    Exists {
        negated: bool,
    },
    In {
        needle: &'a Value,
        negated: bool,
        operand_type: &'a BoundType,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Evaluate a nested physical plan with fresh local state and the supplied
/// transaction, outer rows and cancellation context. Never publish mutations.
/// Scalar cardinality errors are execution errors, never TRY_CAST NULLs.
/// IN over an empty relation is false even for NULL; otherwise a match wins
/// over NULL and a nonmatch with a NULL operand/candidate is unknown.
/// Streaming adapters may stop at a decisive result; eager adapters can observe
/// later errors and consume more resources. No result is cached across calls.
pub trait SubqueryExecutor: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(
        &self,
        plan: &dyn PhysicalOperator,
        request: SubqueryRequest<'_>,
        context: &ExecutionContext<'_>,
    ) -> Result<Value>;
}

#[derive(Default)]
pub struct StreamingSubqueries;
#[derive(Default)]
pub struct MaterializingSubqueries;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SubqueryExecutor for StreamingSubqueries {
    fn name(&self) -> &'static str {
        "streaming-subqueries"
    }
    fn evaluate(
        &self,
        plan: &dyn PhysicalOperator,
        request: SubqueryRequest<'_>,
        context: &ExecutionContext<'_>,
    ) -> Result<Value> {
        let mut reduction = Reduction::new(request, plan.schema().len())?;
        let mut input = stream::open(plan, context)?;
        // Scalar needs a second row only to reject it; EXISTS needs one.
        let demand = if matches!(reduction.request, SubqueryRequest::In { .. }) {
            context.query.batch_size()
        } else {
            1
        };
        while let Some(batch) = input.next(demand)? {
            for row in batch.rows() {
                if let Some(result) = reduction.push(&row, context.query)? {
                    return Ok(result);
                }
            }
        }
        context.query.check()?;
        Ok(reduction.finish())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SubqueryExecutor for MaterializingSubqueries {
    fn name(&self) -> &'static str {
        "materializing-subqueries"
    }
    fn evaluate(
        &self,
        plan: &dyn PhysicalOperator,
        request: SubqueryRequest<'_>,
        context: &ExecutionContext<'_>,
    ) -> Result<Value> {
        let mut reduction = Reduction::new(request, plan.schema().len())?;
        let rows = stream::collect(plan, context)?.rows;
        for row in rows {
            if let Some(result) = reduction.push(&row, context.query)? {
                return Ok(result);
            }
        }
        context.query.check()?;
        Ok(reduction.finish())
    }
}

struct Reduction<'a> {
    request: SubqueryRequest<'a>,
    value: Option<Value>,
    unknown: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> Reduction<'a> {
    fn new(request: SubqueryRequest<'a>, width: usize) -> Result<Self> {
        if !matches!(request, SubqueryRequest::Exists { .. }) && width != 1 {
            return Err(Error::Internal(
                "scalar/membership subquery requires one column".into(),
            ));
        }
        Ok(Self {
            request,
            value: None,
            unknown: false,
        })
    }
    fn push(&mut self, row: &[Value], query: &QueryContext) -> Result<Option<Value>> {
        query.check()?;
        match self.request {
            SubqueryRequest::Scalar => {
                if self.value.is_some() {
                    return Err(Error::Execution(
                        "scalar subquery returned more than one row".into(),
                    ));
                }
                self.value = Some(row[0].clone());
            }
            SubqueryRequest::Exists { negated } => return Ok(Some(Value::Boolean(!negated))),
            SubqueryRequest::In {
                needle,
                negated,
                operand_type,
            } => {
                let candidate = &row[0];
                if needle.is_null() || candidate.is_null() {
                    self.unknown = true;
                } else if operand_type.compare(needle, candidate, query)?.is_eq() {
                    return Ok(Some(Value::Boolean(!negated)));
                }
            }
        }
        Ok(None)
    }
    fn finish(self) -> Value {
        match self.request {
            SubqueryRequest::Scalar => self.value.unwrap_or(Value::Null),
            SubqueryRequest::Exists { negated } => Value::Boolean(negated),
            SubqueryRequest::In { negated, .. } => {
                if self.unknown {
                    Value::Null
                } else {
                    Value::Boolean(negated)
                }
            }
        }
    }
}
