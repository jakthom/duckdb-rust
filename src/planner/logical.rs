use std::sync::Arc;

use super::BoundExpr;
use crate::{
    catalog::{TableDefinition, TableName},
    common::{DataType, Result, Row},
    function::AggregateFunction,
};

#[derive(Clone, Debug)]
pub struct Field {
    pub qualifier: Option<String>,
    pub name: String,
    pub data_type: DataType,
}

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

impl LogicalPlan {
    /// Borrow immediate relational inputs without copying the plan.
    pub fn visit_inputs(&self, visit: &mut impl FnMut(&Self)) {
        match &self.node {
            PlanNode::Filter { input, .. }
            | PlanNode::Projection { input, .. }
            | PlanNode::Aggregate { input, .. }
            | PlanNode::Sort { input, .. }
            | PlanNode::Limit { input, .. }
            | PlanNode::Distinct(input) => visit(input),
            PlanNode::Join { left, right, .. } | PlanNode::Union { left, right, .. } => {
                visit(left);
                visit(right);
            }
            PlanNode::Values(_)
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
            PlanNode::Aggregate {
                groups, aggregates, ..
            } => {
                for expr in groups {
                    visit(expr);
                }
                for aggregate in aggregates {
                    for expr in &aggregate.arguments {
                        visit(expr);
                    }
                    if let Some(expr) = &aggregate.filter {
                        visit(expr);
                    }
                }
            }
            PlanNode::Sort { order, .. } => {
                for key in order {
                    visit(&key.expression);
                }
            }
            PlanNode::Scan(_)
            | PlanNode::KeyLookup { .. }
            | PlanNode::Range { .. }
            | PlanNode::Limit { .. }
            | PlanNode::Distinct(_)
            | PlanNode::Union { .. } => (),
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
                groups,
                aggregates,
            } => PlanNode::Aggregate {
                input,
                groups: groups.into_iter().map(&mut map).collect::<Result<_>>()?,
                aggregates: aggregates
                    .into_iter()
                    .map(|mut aggregate| {
                        aggregate.arguments = aggregate
                            .arguments
                            .into_iter()
                            .map(&mut map)
                            .collect::<Result<_>>()?;
                        aggregate.filter = aggregate.filter.map(&mut map).transpose()?;
                        Ok(aggregate)
                    })
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
            | PlanNode::KeyLookup { .. }
            | PlanNode::Range { .. }
            | PlanNode::Limit { .. }
            | PlanNode::Distinct(_)
            | PlanNode::Union { .. }) => node,
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
            PlanNode::Aggregate {
                input,
                groups,
                aggregates,
            } => PlanNode::Aggregate {
                input: Box::new(map(*input)?),
                groups,
                aggregates,
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
            PlanNode::Union { left, right, all } => PlanNode::Union {
                left: Box::new(map(*left)?),
                right: Box::new(map(*right)?),
                all,
            },
            node @ (PlanNode::Values(_)
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
    Scan(TableName),
    /// Exact typed equality through an advertised transaction-visible index.
    /// The result has the full table schema; NULL keys produce no matches.
    KeyLookup {
        table: TableName,
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
        groups: Vec<BoundExpr>,
        aggregates: Vec<AggregateExpr>,
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
    Union {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        all: bool,
    },
}

#[derive(Clone, Debug)]
pub enum BoundStatement {
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
        names: Vec<TableName>,
        if_exists: bool,
    },
    Insert {
        table: TableName,
        columns: Vec<usize>,
        source: LogicalPlan,
    },
    Update {
        table: TableName,
        assignments: Vec<(usize, BoundExpr)>,
        predicate: Option<BoundExpr>,
    },
    Delete {
        table: TableName,
        predicate: Option<BoundExpr>,
    },
    Begin,
    Commit,
    Rollback,
    Explain(Box<BoundStatement>),
}
