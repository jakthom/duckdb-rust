use std::sync::Arc;

use super::{
    BoundExpr, RecursiveId,
    aggregation::{AggregateOutput, Aggregation},
};
use crate::{
    catalog::{TableBinding, TableDefinition},
    common::{DataType, Result, Row},
    function::AggregateFunction,
};

#[derive(Clone, Debug)]
pub struct Field {
    pub qualifier: Option<String>,
    pub name: String,
    pub data_type: DataType,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Field {
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            qualifier: None,
            name: name.into(),
            data_type,
        }
    }
}

pub type Schema = Vec<Field>;

#[derive(Clone, Debug)]
pub struct LogicalPlan {
    pub schema: Schema,
    pub node: PlanNode,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl LogicalPlan {
    /// Borrow immediate relational inputs without copying the plan.
    pub fn visit_inputs(&self, visit: &mut impl FnMut(&Self)) {
        match &self.node {
            PlanNode::Filter { input, .. }
            | PlanNode::Projection { input, .. }
            | PlanNode::Aggregate { input, .. }
            | PlanNode::Window { input, .. }
            | PlanNode::Sort { input, .. }
            | PlanNode::Limit { input, .. }
            | PlanNode::Distinct(input) => visit(input),
            PlanNode::Join { left, right, .. }
            | PlanNode::SetOperation { left, right, .. }
            | PlanNode::Recursive {
                seed: left,
                step: right,
                ..
            } => {
                visit(left);
                visit(right);
            }
            PlanNode::Values(_)
            | PlanNode::RecursiveInput(_)
            | PlanNode::Scan(_)
            | PlanNode::KeyLookup { .. }
            | PlanNode::Range { .. } => (),
        }
    }
    /// Borrow this node's expression roots; each caller owns recursion and scope.
    pub fn visit_expressions(&self, visit: &mut impl FnMut(&BoundExpr)) {
        match &self.node {
            PlanNode::Values(rows) => {
                for row in rows {
                    for expr in row {
                        visit(expr);
                    }
                }
            }
            PlanNode::Filter { predicate, .. } => visit(predicate),
            PlanNode::Projection { expressions, .. } => {
                for expr in expressions {
                    visit(expr);
                }
            }
            PlanNode::Join { condition, .. } => visit(condition),
            PlanNode::Aggregate { aggregation, .. } => {
                for expr in &aggregation.groups {
                    visit(expr);
                }
                for aggregate in aggregation.functions() {
                    for expr in &aggregate.arguments {
                        visit(expr);
                    }
                    if let Some(expr) = &aggregate.filter {
                        visit(expr);
                    }
                }
            }
            PlanNode::Window { expressions, .. } => {
                for expression in expressions {
                    expression.visit_expressions(visit);
                }
            }
            PlanNode::Sort { order, .. } => {
                for key in order {
                    visit(&key.expression);
                }
            }
            PlanNode::Scan(_)
            | PlanNode::RecursiveInput(_)
            | PlanNode::Recursive { .. }
            | PlanNode::KeyLookup { .. }
            | PlanNode::Range { .. }
            | PlanNode::Limit { .. }
            | PlanNode::Distinct(_)
            | PlanNode::SetOperation { .. } => (),
        }
    }
    /// Transform this node's expression roots without visiting input plans.
    /// The caller owns recursion and must preserve schema and expression types.
    pub fn map_expressions(
        mut self,
        mut map: impl FnMut(BoundExpr) -> Result<BoundExpr>,
    ) -> Result<Self> {
        self.node = match self.node {
            PlanNode::Values(rows) => PlanNode::Values(
                rows.into_iter()
                    .map(|row| row.into_iter().map(&mut map).collect())
                    .collect::<Result<_>>()?,
            ),
            PlanNode::Filter { input, predicate } => PlanNode::Filter {
                input,
                predicate: map(predicate)?,
            },
            PlanNode::Projection { input, expressions } => PlanNode::Projection {
                input,
                expressions: expressions
                    .into_iter()
                    .map(&mut map)
                    .collect::<Result<_>>()?,
            },
            PlanNode::Join {
                left,
                right,
                kind,
                condition,
            } => PlanNode::Join {
                left,
                right,
                kind,
                condition: map(condition)?,
            },
            PlanNode::Aggregate {
                input,
                mut aggregation,
            } => {
                aggregation.groups = aggregation
                    .groups
                    .into_iter()
                    .map(&mut map)
                    .collect::<Result<_>>()?;
                for output in &mut aggregation.outputs {
                    if let AggregateOutput::Function(aggregate) = output {
                        aggregate.arguments = aggregate
                            .arguments
                            .drain(..)
                            .map(&mut map)
                            .collect::<Result<_>>()?;
                        aggregate.filter = aggregate.filter.take().map(&mut map).transpose()?;
                    }
                }
                PlanNode::Aggregate { input, aggregation }
            }
            PlanNode::Window { input, expressions } => PlanNode::Window {
                input,
                expressions: expressions
                    .into_iter()
                    .map(|expression| expression.map_expressions(&mut map))
                    .collect::<Result<_>>()?,
            },
            PlanNode::Sort { input, order } => PlanNode::Sort {
                input,
                order: order
                    .into_iter()
                    .map(|mut order| {
                        order.expression = map(order.expression)?;
                        Ok(order)
                    })
                    .collect::<Result<_>>()?,
            },
            node @ (PlanNode::Scan(_)
            | PlanNode::RecursiveInput(_)
            | PlanNode::Recursive { .. }
            | PlanNode::KeyLookup { .. }
            | PlanNode::Range { .. }
            | PlanNode::Limit { .. }
            | PlanNode::Distinct(_)
            | PlanNode::SetOperation { .. }) => node,
        };
        Ok(self)
    }
    /// Transforms immediate inputs in ownership order without copying subtrees.
    /// Operator semantics and output fields remain owned by this node.
    pub fn map_inputs(self, mut map: impl FnMut(Self) -> Result<Self>) -> Result<Self> {
        let node = match self.node {
            PlanNode::Filter { input, predicate } => PlanNode::Filter {
                input: Box::new(map(*input)?),
                predicate,
            },
            PlanNode::Projection { input, expressions } => PlanNode::Projection {
                input: Box::new(map(*input)?),
                expressions,
            },
            PlanNode::Join {
                left,
                right,
                kind,
                condition,
            } => PlanNode::Join {
                left: Box::new(map(*left)?),
                right: Box::new(map(*right)?),
                kind,
                condition,
            },
            PlanNode::Aggregate { input, aggregation } => PlanNode::Aggregate {
                input: Box::new(map(*input)?),
                aggregation,
            },
            PlanNode::Window { input, expressions } => PlanNode::Window {
                input: Box::new(map(*input)?),
                expressions,
            },
            PlanNode::Sort { input, order } => PlanNode::Sort {
                input: Box::new(map(*input)?),
                order,
            },
            PlanNode::Limit {
                input,
                limit,
                offset,
            } => PlanNode::Limit {
                input: Box::new(map(*input)?),
                limit,
                offset,
            },
            PlanNode::Distinct(input) => PlanNode::Distinct(Box::new(map(*input)?)),
            PlanNode::SetOperation {
                left,
                right,
                kind,
                all,
            } => PlanNode::SetOperation {
                left: Box::new(map(*left)?),
                right: Box::new(map(*right)?),
                all,
                kind,
            },
            PlanNode::Recursive {
                id,
                seed,
                step,
                all,
            } => PlanNode::Recursive {
                id,
                seed: Box::new(map(*seed)?),
                step: Box::new(map(*step)?),
                all,
            },
            node @ (PlanNode::Values(_)
            | PlanNode::RecursiveInput(_)
            | PlanNode::Scan(_)
            | PlanNode::KeyLookup { .. }
            | PlanNode::Range { .. }) => node,
        };
        Ok(Self {
            schema: self.schema,
            node,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Semi,
    Anti,
}

/// SQL multiset operations compare complete typed rows; NULLs compare equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetOperation {
    Union,
    Intersect,
    Except,
}

#[derive(Clone, Debug)]
pub struct AggregateExpr {
    pub function: Arc<dyn AggregateFunction>,
    pub arguments: Vec<BoundExpr>,
    pub distinct: bool,
    pub filter: Option<BoundExpr>,
    pub data_type: DataType,
}

#[derive(Clone, Debug)]
pub struct OrderExpr {
    pub expression: BoundExpr,
    pub descending: bool,
    pub nulls_first: bool,
}

#[derive(Clone, Debug)]
pub enum PlanNode {
    Values(Vec<Vec<BoundExpr>>),
    Scan(TableBinding),
    /// Reads the previous iteration in the nearest enclosing matching binding.
    RecursiveInput(RecursiveId),
    /// Seed rows followed by fixed-point iterations. UNION removes duplicates
    /// across all generations; UNION ALL preserves them. The step sees only
    /// the previous generation, and has the seed's explicitly coerced types.
    Recursive {
        id: RecursiveId,
        seed: Box<LogicalPlan>,
        step: Box<LogicalPlan>,
        all: bool,
    },
    /// Exact typed equality through an advertised transaction-visible index.
    /// The result has the full table schema; NULL keys produce no matches.
    KeyLookup {
        table: TableBinding,
        columns: Vec<usize>,
        key: Row,
    },
    Range {
        start: i64,
        end: i64,
        step: i64,
    },
    Filter {
        input: Box<LogicalPlan>,
        predicate: BoundExpr,
    },
    Projection {
        input: Box<LogicalPlan>,
        expressions: Vec<BoundExpr>,
    },
    Join {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        kind: JoinKind,
        condition: BoundExpr,
    },
    Aggregate {
        input: Box<LogicalPlan>,
        aggregation: Aggregation,
    },
    /// Append one result per input row for each independently bound window.
    Window {
        input: Box<LogicalPlan>,
        expressions: Vec<super::window::WindowExpression>,
    },
    Sort {
        input: Box<LogicalPlan>,
        order: Vec<OrderExpr>,
    },
    Limit {
        input: Box<LogicalPlan>,
        limit: Option<usize>,
        offset: usize,
    },
    Distinct(Box<LogicalPlan>),
    SetOperation {
        kind: SetOperation,
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        all: bool,
    },
}

#[derive(Clone, Debug)]
pub enum BoundStatement {
    Noop,
    Configure(crate::main::settings::SettingChange),
    Checkpoint,
    Query(LogicalPlan),
    CreateSchema {
        name: String,
        if_not_exists: bool,
    },
    DropSchema {
        names: Vec<String>,
        if_exists: bool,
    },
    CreateTable {
        definition: TableDefinition,
        if_not_exists: bool,
        source: Option<LogicalPlan>,
    },
    DropTable {
        tables: Vec<TableBinding>,
        if_exists: bool,
    },
    AlterTable {
        table: TableBinding,
        alteration: crate::catalog::TableAlteration,
    },
    Insert {
        table: TableBinding,
        columns: Vec<usize>,
        source: LogicalPlan,
    },
    Update {
        table: TableBinding,
        assignments: Vec<(usize, BoundExpr)>,
        metadata: crate::storage::UpdateMetadata,
        predicate: Option<BoundExpr>,
    },
    Delete {
        table: TableBinding,
        predicate: Option<BoundExpr>,
    },
    Begin,
    Commit,
    Rollback,
    Explain(Box<BoundStatement>),
}
