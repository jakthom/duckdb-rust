use super::subquery::SubqueryRequest;
mod batch;
use crate::{
    common::{Error, Result, Row, Value},
    function::{ArgumentEvaluation, ArgumentProvenance},
    parallel::QueryContext,
    planner::{
        BoundExpr, ExprKind,
        expression::{BinaryOp, SubqueryKind, UnaryOp},
    },
};
pub(crate) use batch::select_boolean;
pub use batch::{BatchedEvaluator, evaluate_expression_rows};

/// An evaluated value and its encoding in the actual current execution scope.
/// Unknown metadata must never be upgraded by comparing result values. An
/// evaluator claiming Constant promises batch invariance without discarding
/// required evaluation, effects, validation, or errors.
pub struct EvaluatedValue {
    pub value: Value,
    pub provenance: ArgumentProvenance,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Explicit expression inputs beyond the current row. Resource-only evaluation
/// rejects relational dependencies. Execution supplies lexical outer rows and
/// nested plans through the same contract, without SQL or concrete storage.
pub trait EvaluationContext {
    fn query(&self) -> &QueryContext;
    /// Encoding supplied by the current physical input, not its cardinality.
    fn column_provenance(&self, _column: usize) -> ArgumentProvenance {
        ArgumentProvenance::Unknown
    }
    /// A successfully materialized statement-local scalar reduction may be
    /// shared across this scope. Per-row/correlated results remain Unknown.
    fn subquery_provenance(
        &self,
        _query: &std::sync::Arc<crate::planner::expression::BoundSubquery>,
    ) -> ArgumentProvenance {
        ArgumentProvenance::Unknown
    }
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl EvaluationContext for QueryContext {
    fn query(&self) -> &QueryContext {
        self
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Evaluate the shared bound-expression semantics using the retained adapters.
/// Literals and column references are pure value access; physical planners may
/// project column vectors directly. Implementations may change evaluation
/// strategy, but must preserve NULLs, short-circuiting, errors and declared
/// function effects, and observe query cancellation during work.
pub trait ExpressionEvaluator: Send + Sync {
    /// Optional whole-batch selection proof. Some(true) selects all rows;
    /// Some(false) selects none. It must preserve required validation, errors,
    /// and effects. The default declines and keeps ordinary predicate work.
    fn uniform_selection(
        &self,
        _expression: &BoundExpr,
        _input: &crate::common::vector::DataChunk,
        _context: &dyn EvaluationContext,
    ) -> Result<Option<bool>> {
        Ok(None)
    }
    fn name(&self) -> &'static str;
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value>;
    /// Preserve selected ordinary evaluation unless this evaluator explicitly
    /// owns physical provenance. Default Unknown is important for replacement
    /// adapters: delegating a value does not delegate an encoding assertion.
    fn evaluate_with_provenance(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<EvaluatedValue> {
        self.evaluate(expression, row, context)
            .map(|value| EvaluatedValue {
                value,
                provenance: ArgumentProvenance::Unknown,
            })
    }
    /// Evaluate an owned result column with exactly the input cardinality.
    /// Preserve row order, lazy branches, the first error and declared effects.
    /// Batch evaluation may reorder only expressions proved total and without
    /// effects. Demand is limited to the supplied rows; no input is retained.
    /// The default preserves scalar behavior through the selected evaluator.
    fn evaluate_batch(
        &self,
        expression: &BoundExpr,
        input: &crate::common::vector::DataChunk,
        context: &dyn EvaluationContext,
    ) -> Result<crate::common::vector::Vector> {
        evaluate_expression_rows(self, expression, input, context)
    }
    /// Select rows whose Boolean predicate is true, in strictly increasing
    /// input order. NULL is not selected. The same error/effect rules apply as
    /// batch evaluation. Consumers validate all returned positions. Adapters
    /// may avoid materializing a Boolean value for each row.
    fn select_batch(
        &self,
        expression: &BoundExpr,
        input: &crate::common::vector::DataChunk,
        context: &dyn EvaluationContext,
    ) -> Result<Vec<usize>> {
        select_boolean(
            &self.evaluate_batch(expression, input, context)?,
            input.len(),
            context.query(),
        )
    }
}

#[derive(Default)]
pub struct ScalarEvaluator;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
            ExprKind::Literal(v) | ExprKind::Parameter(v) => v.clone(),
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
            ExprKind::Cast(inner, cast, try_cast) => {
                let value = eval(inner)?;
                if *try_cast {
                    cast.apply_try(&value, query)?
                } else {
                    cast.apply(&value, query)?
                }
            }
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
                for argument in arguments.iter().take(
                    if matches!(function.argument_evaluation(), ArgumentEvaluation::TypeOnly) {
                        0
                    } else {
                        arguments.len()
                    },
                ) {
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn checked_value(value: Value, data_type: &crate::DataType) -> Result<Value> {
    if !value.fits_type(data_type) {
        return Err(Error::Execution(format!(
            "expression result overflows or differs from {data_type}"
        )));
    }
    Ok(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
