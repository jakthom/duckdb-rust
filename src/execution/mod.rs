pub mod expression_executor;
pub mod index;
pub mod operator;
pub mod physical_plan;
pub mod stream;
pub mod subquery;

use crate::{
    common::{Result, Row, vector::DataChunk},
    parallel::QueryContext,
    planner::Schema,
    transaction::Transaction,
};
use expression_executor::ExpressionEvaluator;
use physical_plan::PhysicalOperator;

#[derive(Debug, Clone)]
pub struct DataSet {
    pub schema: Schema,
    pub rows: Vec<Row>,
}

pub struct ExecutionContext<'a> {
    pub transaction: &'a dyn Transaction,
    pub expressions: &'a dyn ExpressionEvaluator,
    pub query: &'a QueryContext,
    pub subquery_plans: &'a subquery::PreparedSubqueries<'a>,
    pub subqueries: &'a dyn subquery::SubqueryExecutor,
    pub outer: Option<&'a OuterRow<'a>>,
    pub recursive: Option<&'a operator::recursive::RecursiveFrame<'a>>,
}

/// A borrowed lexical row frame. Nested evaluation completes before its input
/// frame expires; independently opened streams never share mutable row state.
pub struct OuterRow<'a> {
    row: &'a Row,
    parent: Option<&'a OuterRow<'a>>,
}

impl expression_executor::EvaluationContext for ExecutionContext<'_> {
    fn query(&self) -> &QueryContext {
        self.query
    }
    fn outer_column(&self, depth: usize, column: usize) -> Result<crate::Value> {
        let mut frame = self.outer;
        if depth == 0 {
            return Err(crate::Error::Internal(
                "outer query depth must be positive".into(),
            ));
        }
        for _ in 1..depth {
            frame = frame.and_then(|frame| frame.parent);
        }
        frame
            .and_then(|frame| frame.row.get(column))
            .cloned()
            .ok_or_else(|| {
                crate::Error::Internal("correlated column outside its lexical frame".into())
            })
    }
    fn subquery(
        &self,
        subquery: &std::sync::Arc<crate::planner::expression::BoundSubquery>,
        request: subquery::SubqueryRequest<'_>,
        row: &Row,
    ) -> Result<crate::Value> {
        self.query.check()?;
        if let Some(value) = self.subquery_plans.value(subquery) {
            return Ok(value);
        }
        let outer = OuterRow {
            row,
            parent: self.outer,
        };
        let nested = ExecutionContext {
            outer: Some(&outer),
            ..*self
        };
        let physical = self.subquery_plans.prepare(&subquery.plan)?;
        let value = self
            .subqueries
            .evaluate(physical.as_ref(), request, &nested)?;
        self.query.check()?;
        self.subquery_plans.retain(subquery, &value);
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamControl {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub rows_delivered: usize,
    pub stopped_early: bool,
}

/// A sink consumes whole owned chunks. Stop accepts the current chunk and asks
/// for no further work. An error aborts execution; already delivered chunks
/// cannot be retracted and callers must not treat them as a successful query.
pub trait ResultSink {
    fn consume(&mut self, chunk: DataChunk) -> Result<StreamControl>;
}
impl<F: FnMut(DataChunk) -> Result<StreamControl>> ResultSink for F {
    fn consume(&mut self, chunk: DataChunk) -> Result<StreamControl> {
        self(chunk)
    }
}

/// Executes a shared plan with fresh local state. Batches reach the sink in plan
/// order. Both adapters report all errors when fully consumed; an eager adapter
/// may discover later errors before delivery. Neither adapter performs a commit.
pub trait Executor: Send + Sync {
    fn name(&self) -> &'static str;
    fn execute(
        &self,
        plan: &dyn PhysicalOperator,
        context: &ExecutionContext<'_>,
        sink: &mut dyn ResultSink,
    ) -> Result<ExecutionOutcome>;
}

#[derive(Default)]
pub struct PullExecutor;
impl Executor for PullExecutor {
    fn name(&self) -> &'static str {
        "pull"
    }
    fn execute(
        &self,
        plan: &dyn PhysicalOperator,
        context: &ExecutionContext<'_>,
        sink: &mut dyn ResultSink,
    ) -> Result<ExecutionOutcome> {
        let mut input = stream::open(plan, context)?;
        let mut outcome = ExecutionOutcome {
            rows_delivered: 0,
            stopped_early: false,
        };
        while let Some(batch) = input.next(context.query.batch_size())? {
            outcome.rows_delivered = outcome
                .rows_delivered
                .checked_add(batch.len())
                .ok_or_else(|| crate::Error::Resource("result row count overflow".into()))?;
            if sink.consume(batch)? == StreamControl::Stop {
                outcome.stopped_early = true;
                break;
            }
        }
        context.query.check()?;
        Ok(outcome)
    }
}

/// Eager result collection is an ordinary alternative, useful when consumers
/// require evaluation to finish before any result is delivered.
#[derive(Default)]
pub struct MaterializingExecutor;
impl Executor for MaterializingExecutor {
    fn name(&self) -> &'static str {
        "materializing"
    }
    fn execute(
        &self,
        plan: &dyn PhysicalOperator,
        context: &ExecutionContext<'_>,
        sink: &mut dyn ResultSink,
    ) -> Result<ExecutionOutcome> {
        let output = stream::collect(plan, context)?;
        let mut outcome = ExecutionOutcome {
            rows_delivered: 0,
            stopped_early: false,
        };
        for rows in output.rows.chunks(
            context
                .query
                .batch_size()
                .min(context.query.max_intermediate_rows()),
        ) {
            context.query.check()?;
            let batch = stream::chunk(&output.schema, rows)?.expect("nonempty result batch");
            outcome.rows_delivered += batch.len();
            if sink.consume(batch)? == StreamControl::Stop {
                outcome.stopped_early = true;
                break;
            }
        }
        context.query.check()?;
        Ok(outcome)
    }
}

pub(crate) struct CollectingSink<'a> {
    pub rows: crate::common::RowCollection,
    pub query: &'a QueryContext,
}
impl ResultSink for CollectingSink<'_> {
    fn consume(&mut self, chunk: DataChunk) -> Result<StreamControl> {
        self.query
            .check_rows(self.rows.len().saturating_add(chunk.len()))?;
        self.rows.append(&chunk)?;
        Ok(StreamControl::Continue)
    }
}
