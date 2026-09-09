use super::subquery::SubqueryRequest;
use crate::{
    common::{Error, Result, Row, Value},
    function::ArgumentEvaluation,
    parallel::QueryContext,
    planner::{
        BoundExpr, ExprKind,
        expression::{BinaryOp, SubqueryKind, UnaryOp},
    },
};

/// Explicit expression inputs beyond the current row. Resource-only evaluation
/// rejects relational dependencies. Execution supplies lexical outer rows and
/// nested plans through the same contract, without SQL or concrete storage.
pub trait EvaluationContext {
    fn query(&self) -> &QueryContext;
    /// A value already produced by this row's relational dependency stage.
    fn prepared_subquery(
        &self,
        _query: &std::sync::Arc<crate::planner::expression::BoundSubquery>,
    ) -> Option<Value> {
        None
    }
    fn outer_column(&self, _depth: usize, _column: usize) -> Result<Value> {
        Err(Error::Internal(
            "outer row is unavailable in this expression context".into(),
        ))
    }
    fn subquery(
        &self,
        _subquery: &std::sync::Arc<crate::planner::expression::BoundSubquery>,
        _request: SubqueryRequest<'_>,
        _row: &Row,
    ) -> Result<Value> {
        Err(Error::Unsupported(
            "subquery execution in a resource-only expression context".into(),
        ))
    }
}
impl EvaluationContext for QueryContext {
    fn query(&self) -> &QueryContext {
        self
    }
}

/// Evaluate the shared bound-expression semantics using the retained adapters.
/// Literals and column references are pure value access; physical planners may
/// project column vectors directly. Implementations may change evaluation
/// strategy, but must preserve NULLs, short-circuiting, errors and declared
/// function effects, and observe query cancellation during work.
pub trait ExpressionEvaluator: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value>;
}

#[derive(Default)]
pub struct ScalarEvaluator;

impl ExpressionEvaluator for ScalarEvaluator {
    fn name(&self) -> &'static str {
        "scalar-expression"
    }
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        let query = context.query();
        query.check()?;
        let eval = |e: &BoundExpr| self.evaluate(e, row, context);
        let value = match &expression.kind {
            ExprKind::Literal(v) => v.clone(),
            ExprKind::OuterColumn { depth, column } => context.outer_column(*depth, *column)?,
            ExprKind::Subquery(subquery) => {
                if let Some(value) = context.prepared_subquery(subquery) {
                    return checked_value(value, &expression.data_type);
                }
                let kind = &subquery.kind;
                let value = match kind {
                    SubqueryKind::Scalar => {
                        context.subquery(subquery, SubqueryRequest::Scalar, row)?
                    }
                    SubqueryKind::Exists { negated } => context.subquery(
                        subquery,
                        SubqueryRequest::Exists { negated: *negated },
                        row,
                    )?,
                    SubqueryKind::In {
                        needle,
                        negated,
                        operand_type,
                    } => {
                        let needle = eval(needle)?;
                        context.subquery(
                            subquery,
                            SubqueryRequest::In {
                                needle: &needle,
                                negated: *negated,
                                operand_type,
                            },
                            row,
                        )?
                    }
                };
                query.check()?;
                if matches!(kind, SubqueryKind::Exists { .. })
                    && !matches!(value, Value::Boolean(_))
                {
                    return Err(Error::Internal(
                        "EXISTS adapter must return a non-NULL BOOLEAN".into(),
                    ));
                }
                query
                    .types()
                    .bind(&expression.data_type)?
                    .validate(&value, query)
                    .map_err(|error| match error {
                        Error::Conversion(_) => Error::Internal(
                            "subquery adapter returned an invalid logical value".into(),
                        ),
                        other => other,
                    })?;
                value
            }
            ExprKind::Column(i) => row
                .get(*i)
                .cloned()
                .ok_or_else(|| Error::Internal(format!("column {i} outside row")))?,
            ExprKind::Cast(inner, cast, try_cast) => match cast.apply(&eval(inner)?, query) {
                Ok(v) => v,
                Err(Error::Conversion(_)) if *try_cast => Value::Null,
                Err(e) => return Err(e),
            },
            ExprKind::Unary(op, inner) => {
                let value = eval(inner)?;
                match op {
                    UnaryOp::IsNull => Value::Boolean(value.is_null()),
                    UnaryOp::IsNotNull => Value::Boolean(!value.is_null()),
                    _ if value.is_null() => Value::Null,
                    UnaryOp::Not => Value::Boolean(!value.as_bool()?.unwrap_or(false)),
                }
            }
            ExprKind::Binary(op, left, right, operand_type) => {
                let left = eval(left)?;
                if *op == BinaryOp::And && left.as_bool()? == Some(false) {
                    return Ok(Value::Boolean(false));
                }
                if *op == BinaryOp::Or && left.as_bool()? == Some(true) {
                    return Ok(Value::Boolean(true));
                }
                evaluate_binary(*op, left, eval(right)?, operand_type, query)?
            }
            ExprKind::Operator(function, arguments) => match arguments.as_slice() {
                [argument] => function.apply(&[eval(argument)?], query)?,
                [left, right] => function.apply(&[eval(left)?, eval(right)?], query)?,
                _ => return Err(Error::Internal("operator argument count".into())),
            },
            ExprKind::Scalar(function, arguments) => {
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    let value = eval(argument)?;
                    if matches!(
                        function.argument_evaluation(),
                        ArgumentEvaluation::FirstNonNull
                    ) && !value.is_null()
                    {
                        return checked_value(value, &expression.data_type);
                    }
                    values.push(value);
                }
                let value = function.evaluate(&values, query)?;
                query
                    .types()
                    .bind(&expression.data_type)?
                    .validate(&value, query)
                    .map_err(|error| match error {
                        Error::Conversion(_) => Error::Internal(
                            "scalar function returned an invalid logical value".into(),
                        ),
                        other => other,
                    })?;
                value
            }
            ExprKind::Case(branches, otherwise) => {
                let mut chosen = None;
                for (condition, value) in branches {
                    if eval(condition)?.as_bool()? == Some(true) {
                        chosen = Some(eval(value)?);
                        break;
                    }
                }
                match chosen {
                    Some(v) => v,
                    None => eval(otherwise)?,
                }
            }
            ExprKind::InList(needle, list, negated, operand_type) => {
                let needle = eval(needle)?;
                let mut unknown = needle.is_null();
                let mut matched = false;
                for candidate in list {
                    let candidate = eval(candidate)?;
                    if candidate.is_null() {
                        unknown = true;
                    } else if !needle.is_null()
                        && operand_type.compare(&needle, &candidate, query)?.is_eq()
                    {
                        matched = true;
                        break;
                    }
                }
                if matched {
                    Value::Boolean(!negated)
                } else if unknown {
                    Value::Null
                } else {
                    Value::Boolean(*negated)
                }
            }
        };
        checked_value(value, &expression.data_type)
    }
}

fn checked_value(value: Value, data_type: &crate::DataType) -> Result<Value> {
    if !value.fits_type(data_type) {
        return Err(Error::Execution(format!(
            "expression result overflows or differs from {data_type}"
        )));
    }
    Ok(value)
}

fn evaluate_binary(
    op: BinaryOp,
    left: Value,
    right: Value,
    data_type: &crate::common::type_registry::BoundType,
    context: &QueryContext,
) -> Result<Value> {
    use BinaryOp::*;
    if matches!(op, And | Or) {
        let (a, b) = (left.as_bool()?, right.as_bool()?);
        return Ok(match (op, a, b) {
            (And, Some(false), _) | (And, _, Some(false)) => Value::Boolean(false),
            (Or, Some(true), _) | (Or, _, Some(true)) => Value::Boolean(true),
            (_, None, _) | (_, _, None) => Value::Null,
            (And, Some(a), Some(b)) => Value::Boolean(a && b),
            (_, Some(a), Some(b)) => Value::Boolean(a || b),
        });
    }
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let ordering = data_type.compare(&left, &right, context)?;
    Ok(Value::Boolean(match op {
        Equal => ordering.is_eq(),
        NotEqual => !ordering.is_eq(),
        Less => ordering.is_lt(),
        LessEqual => !ordering.is_gt(),
        Greater => ordering.is_gt(),
        GreaterEqual => !ordering.is_lt(),
        And | Or => unreachable!("handled Boolean operators"),
    }))
}
