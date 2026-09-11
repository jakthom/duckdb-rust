use std::sync::Arc;

use crate::{
    common::{
        DataType, Result, Value,
        cast::{BoundCast, CastMode, CastRegistry},
    },
    function::ScalarFunction,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    IsNull,
    IsNotNull,
}

#[derive(Clone, Debug)]
pub struct BoundExpr {
    pub kind: ExprKind,
    pub data_type: DataType,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Literal(Value),
    /// A statement-owned typed parameter. Constant for execution, but not a
    /// SQL literal for contextual overload or string coercion rules.
    Parameter(Value),
    Column(usize),
    /// One-based lexical query depth and a typed position in that outer row.
    OuterColumn {
        depth: usize,
        column: usize,
    },
    Subquery(Arc<BoundSubquery>),
    Cast(Box<BoundExpr>, Arc<BoundCast>, bool),
    Unary(UnaryOp, Box<BoundExpr>),
    Binary(
        BinaryOp,
        Box<BoundExpr>,
        Box<BoundExpr>,
        Arc<crate::common::type_registry::BoundType>,
    ),
    Operator(
        Arc<crate::function::operator::BoundOperator>,
        Vec<BoundExpr>,
    ),
    Scalar(Arc<dyn ScalarFunction>, Vec<BoundExpr>),
    Case(Vec<(BoundExpr, BoundExpr)>, Box<BoundExpr>),
    Between(
        Box<BoundExpr>,
        Box<BoundExpr>,
        Box<BoundExpr>,
        Arc<crate::common::type_registry::BoundType>,
    ),
    InList(
        Box<BoundExpr>,
        Vec<BoundExpr>,
        bool,
        Arc<crate::common::type_registry::BoundType>,
    ),
}

/// Nested relational expressions preserve the enclosing transaction snapshot.
/// Scalar requires one column and at most one row (empty produces NULL).
/// EXISTS tests row presence; IN uses SQL three-valued membership semantics.
#[derive(Clone, Debug)]
pub enum SubqueryKind {
    Scalar,
    Exists {
        negated: bool,
    },
    In {
        needle: Box<BoundExpr>,
        negated: bool,
        operand_type: Arc<crate::common::type_registry::BoundType>,
    },
}

/// Immutable nested query identity. Clones share the same bound operation;
/// rebuilding it creates a new identity for statement-local preparation.
#[derive(Clone, Debug)]
pub struct BoundSubquery {
    pub plan: Arc<super::LogicalPlan>,
    pub kind: SubqueryKind,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundExpr {
    /// A conservative proof for a validated scalar tree: evaluation is pure and
    /// has no data-dependent errors for any valid input. Resource exhaustion and
    /// cancellation remain possible. False means unknown, not necessarily unsafe.
    /// Consumers must separately establish the row scope and NULL semantics.
    pub fn is_pure_and_total(&self) -> bool {
        match &self.kind {
            ExprKind::Literal(_)
            | ExprKind::Parameter(_)
            | ExprKind::Column(_)
            | ExprKind::OuterColumn { .. } => true,
            ExprKind::Unary(_, inner) => inner.is_pure_and_total(),
            ExprKind::Cast(inner, cast, false) => inner.is_pure_and_total() && cast.is_total(),
            ExprKind::Case(branches, otherwise) => {
                branches.iter().all(|(condition, value)| {
                    condition.is_pure_and_total() && value.is_pure_and_total()
                }) && otherwise.is_pure_and_total()
            }
            ExprKind::Binary(op, left, right, _) if !matches!(op, BinaryOp::And | BinaryOp::Or) => {
                left.is_pure_and_total() && right.is_pure_and_total()
            }
            ExprKind::Operator(function, arguments) => {
                let effects = function.effects();
                !effects.volatile
                    && !effects.external_access
                    && arguments.iter().all(Self::is_pure_and_total)
                    && function.is_total(
                        &arguments
                            .iter()
                            .map(Self::constant_value)
                            .collect::<Vec<_>>(),
                    )
            }
            _ => false,
        }
    }
    /// Visit immediate scalar children without copying them. Relational inputs
    /// of a subquery have their own row scope and are not scalar children.
    pub fn visit_children<'a>(&'a self, visit: &mut impl FnMut(&'a Self)) {
        match &self.kind {
            ExprKind::Literal(_)
            | ExprKind::Parameter(_)
            | ExprKind::Column(_)
            | ExprKind::OuterColumn { .. } => (),
            ExprKind::Cast(inner, ..) | ExprKind::Unary(_, inner) => visit(inner),
            ExprKind::Binary(_, left, right, _) => {
                visit(left);
                visit(right);
            }
            ExprKind::Operator(_, arguments) | ExprKind::Scalar(_, arguments) => {
                for argument in arguments {
                    visit(argument);
                }
            }
            ExprKind::Case(branches, otherwise) => {
                for (condition, value) in branches {
                    visit(condition);
                    visit(value);
                }
                visit(otherwise);
            }
            ExprKind::Between(input, lower, upper, _) => {
                visit(input);
                visit(lower);
                visit(upper);
            }
            ExprKind::InList(needle, list, ..) => {
                visit(needle);
                for value in list {
                    visit(value);
                }
            }
            ExprKind::Subquery(query) => {
                if let SubqueryKind::In { needle, .. } = &query.kind {
                    visit(needle);
                }
            }
        }
    }
    /// Transform immediate children in evaluation order, retaining this node's
    /// declared type and selected adapters. This does not evaluate expressions.
    pub fn map_children(self, mut map: impl FnMut(Self) -> Result<Self>) -> Result<Self> {
        let kind = match self.kind {
            ExprKind::Subquery(query) => {
                let BoundSubquery { plan, kind } = Arc::unwrap_or_clone(query);
                ExprKind::Subquery(Arc::new(BoundSubquery {
                    plan,
                    kind: match kind {
                        SubqueryKind::In {
                            needle,
                            negated,
                            operand_type,
                        } => SubqueryKind::In {
                            needle: Box::new(map(*needle)?),
                            negated,
                            operand_type,
                        },
                        other => other,
                    },
                }))
            }
            ExprKind::Cast(inner, cast, try_cast) => {
                ExprKind::Cast(Box::new(map(*inner)?), cast, try_cast)
            }
            ExprKind::Unary(op, inner) => ExprKind::Unary(op, Box::new(map(*inner)?)),
            ExprKind::Binary(op, left, right, operand_type) => ExprKind::Binary(
                op,
                Box::new(map(*left)?),
                Box::new(map(*right)?),
                operand_type,
            ),
            ExprKind::Operator(function, arguments) => ExprKind::Operator(
                function,
                arguments.into_iter().map(&mut map).collect::<Result<_>>()?,
            ),
            ExprKind::Scalar(function, arguments) => ExprKind::Scalar(
                function,
                arguments.into_iter().map(&mut map).collect::<Result<_>>()?,
            ),
            ExprKind::Case(branches, otherwise) => ExprKind::Case(
                branches
                    .into_iter()
                    .map(|(condition, value)| Ok((map(condition)?, map(value)?)))
                    .collect::<Result<_>>()?,
                Box::new(map(*otherwise)?),
            ),
            ExprKind::Between(input, lower, upper, operand_type) => ExprKind::Between(
                Box::new(map(*input)?),
                Box::new(map(*lower)?),
                Box::new(map(*upper)?),
                operand_type,
            ),
            ExprKind::InList(needle, list, negated, operand_type) => ExprKind::InList(
                Box::new(map(*needle)?),
                list.into_iter().map(&mut map).collect::<Result<_>>()?,
                negated,
                operand_type,
            ),
            kind @ (ExprKind::Literal(_)
            | ExprKind::Parameter(_)
            | ExprKind::Column(_)
            | ExprKind::OuterColumn { .. }) => kind,
        };
        Ok(Self {
            kind,
            data_type: self.data_type,
        })
    }
    pub fn literal(value: Value) -> Self {
        Self {
            data_type: value.data_type(),
            kind: ExprKind::Literal(value),
        }
    }
    pub fn parameter(value: Value) -> Self {
        Self {
            data_type: value.data_type(),
            kind: ExprKind::Parameter(value),
        }
    }
    /// A retained value without evaluation. This does not establish SQL literal
    /// identity; optimizers and executors may treat typed parameters as constants.
    pub fn constant_value(&self) -> Option<&Value> {
        match &self.kind {
            ExprKind::Literal(value) | ExprKind::Parameter(value) => Some(value),
            _ => None,
        }
    }
    pub fn column(index: usize, data_type: DataType) -> Self {
        Self {
            kind: ExprKind::Column(index),
            data_type,
        }
    }
    pub fn cast(
        self,
        data_type: DataType,
        mode: CastMode,
        registry: &CastRegistry,
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Self> {
        if self.data_type == data_type {
            Ok(self)
        } else {
            let cast = registry.bind(&self.data_type, &data_type, mode, types)?;
            Ok(Self {
                kind: ExprKind::Cast(Box::new(self), Arc::new(cast), false),
                data_type,
            })
        }
    }
}
