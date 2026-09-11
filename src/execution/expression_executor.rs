use super::subquery::SubqueryRequest;
mod batch;
mod provenance;
use crate::{
    common::{Error, Result, Row, Value},
    function::{ArgumentEvaluation, ArgumentProvenance, operator::Operator},
    parallel::QueryContext,
    planner::{
        BoundExpr, ExprKind,
        expression::{BinaryOp, SubqueryKind, UnaryOp},
    },
};
pub(crate) use batch::select_boolean;
pub use batch::{BatchedEvaluator, evaluate_expression_rows};
pub(crate) use provenance::{BatchContext, result_column};

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
    /// Evaluate a Boolean expression in predicate mode. Conjunctions can stop
    /// once the current row cannot be selected, unlike projected Boolean
    /// values, whose runtime children retain ordinary value-mode demand.
    fn select(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<bool> {
        selection_matches(self, expression, row, context)
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
        if let Some(selected) = select_conjunction_rows(self, expression, input, context)? {
            return Ok(selected);
        }
        select_boolean(
            &self.evaluate_batch(expression, input, context)?,
            input.len(),
            context.query(),
        )
    }
}

#[derive(Default)]
pub struct ScalarEvaluator;

/// DuckDB's predicate executor short-circuits conjunctions per selected row,
/// even though projecting the same Boolean expression preserves value-mode
/// demand for both runtime children. Keep that distinction at the evaluator
/// boundary so scalar and batched adapters agree without changing expression
/// value semantics.
fn select_conjunction_rows<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &crate::common::vector::DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Option<Vec<usize>>> {
    if !matches!(
        expression.kind,
        ExprKind::Binary(BinaryOp::And | BinaryOp::Or, ..)
    ) {
        return Ok(None);
    }
    let context = BatchContext {
        parent: context,
        input,
    };
    let mut row = Vec::with_capacity(input.columns().len());
    let mut selected = Vec::with_capacity(input.len());
    for index in 0..input.len() {
        context.query().check()?;
        input.read_row(index, &mut row)?;
        if selection_matches(evaluator, expression, &row, &context)? {
            selected.push(index);
        }
    }
    context.query().check()?;
    Ok(Some(selected))
}

fn selection_matches<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    row: &Row,
    context: &dyn EvaluationContext,
) -> Result<bool> {
    match &expression.kind {
        ExprKind::Binary(BinaryOp::And, left, right, _) => {
            if !selection_matches(evaluator, left, row, context)? {
                return Ok(false);
            }
            selection_matches(evaluator, right, row, context)
        }
        ExprKind::Binary(BinaryOp::Or, left, right, _) => {
            if selection_matches(evaluator, left, row, context)? {
                return Ok(true);
            }
            selection_matches(evaluator, right, row, context)
        }
        _ => Ok(evaluator.evaluate(expression, row, context)?.as_bool()? == Some(true)),
    }
}

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
        self.evaluate_with_provenance(expression, row, context)
            .map(|result| result.value)
    }
    fn evaluate_with_provenance(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<EvaluatedValue> {
        let query = context.query();
        query.check()?;
        let constant = std::cell::Cell::new(true);
        let eval = |e: &BoundExpr| {
            let result = self.evaluate_with_provenance(e, row, context)?;
            constant.set(constant.get() && result.provenance == ArgumentProvenance::Constant);
            Ok::<Value, Error>(result.value)
        };
        let value = match &expression.kind {
            ExprKind::Literal(v) | ExprKind::Parameter(v) => v.clone(),
            ExprKind::OuterColumn { depth, column } => {
                constant.set(false);
                context.outer_column(*depth, *column)?
            }
            ExprKind::Subquery(subquery) => {
                if let Some(value) = context.prepared_subquery(subquery) {
                    return checked_evaluated(
                        value,
                        &expression.data_type,
                        context.subquery_provenance(subquery) == ArgumentProvenance::Constant,
                    );
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
                constant.set(context.subquery_provenance(subquery) == ArgumentProvenance::Constant);
                value
            }
            ExprKind::Column(i) => {
                constant.set(context.column_provenance(*i) == ArgumentProvenance::Constant);
                row.get(*i)
                    .cloned()
                    .ok_or_else(|| Error::Internal(format!("column {i} outside row")))?
            }
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
                let comparison = ordinary_comparison(*op);
                if comparison && constant_null_expression(right) {
                    if literal_cast_expression(left) {
                        self.evaluate_with_provenance(left, row, context)?;
                    }
                    if &left.data_type != operand_type.data_type() {
                        return Err(Error::Internal(
                            "comparison operand metadata mismatch".into(),
                        ));
                    }
                    validate_comparison_null(
                        &right.data_type,
                        &expression.data_type,
                        operand_type,
                        query,
                    )?;
                    return checked_evaluated(Value::Null, &expression.data_type, true);
                }
                let mut right_value =
                    if comparison && right.is_pure_and_total() && !row_dependent(right) {
                        let result = self.evaluate_with_provenance(right, row, context)?;
                        constant.set(
                            constant.get() && result.provenance == ArgumentProvenance::Constant,
                        );
                        Some(result)
                    } else {
                        None
                    };
                if right_value.as_ref().is_some_and(|right| {
                    right.value.is_null() && right.provenance == ArgumentProvenance::Constant
                }) {
                    if &left.data_type != operand_type.data_type() {
                        return Err(Error::Internal(
                            "comparison operand metadata mismatch".into(),
                        ));
                    }
                    validate_comparison_null(
                        &right.data_type,
                        &expression.data_type,
                        operand_type,
                        query,
                    )?;
                    return checked_evaluated(Value::Null, &expression.data_type, true);
                }
                let left_result = self.evaluate_with_provenance(left, row, context)?;
                constant
                    .set(constant.get() && left_result.provenance == ArgumentProvenance::Constant);
                let left_value = left_result.value;
                if comparison
                    && left_value.is_null()
                    && left_result.provenance == ArgumentProvenance::Constant
                {
                    validate_comparison_null(
                        &left.data_type,
                        &expression.data_type,
                        operand_type,
                        query,
                    )?;
                    return checked_evaluated(Value::Null, &expression.data_type, true);
                }
                let foldable_left = left.is_pure_and_total() && !row_dependent(left);
                if *op == BinaryOp::And && foldable_left && left_value.as_bool()? == Some(false) {
                    return checked_evaluated(
                        Value::Boolean(false),
                        &expression.data_type,
                        constant.get(),
                    );
                }
                if *op == BinaryOp::Or && foldable_left && left_value.as_bool()? == Some(true) {
                    return checked_evaluated(
                        Value::Boolean(true),
                        &expression.data_type,
                        constant.get(),
                    );
                }
                let right_value = if let Some(right) = right_value.take() {
                    right
                } else {
                    let result = self.evaluate_with_provenance(right, row, context)?;
                    constant
                        .set(constant.get() && result.provenance == ArgumentProvenance::Constant);
                    result
                };
                if comparison
                    && right_value.value.is_null()
                    && right_value.provenance == ArgumentProvenance::Constant
                {
                    if &left.data_type != operand_type.data_type() {
                        return Err(Error::Internal(
                            "comparison operand metadata mismatch".into(),
                        ));
                    }
                    operand_type
                        .validate(&left_value, query)
                        .map_err(comparison_validation_error)?;
                    validate_comparison_null(
                        &right.data_type,
                        &expression.data_type,
                        operand_type,
                        query,
                    )?;
                    return checked_evaluated(Value::Null, &expression.data_type, true);
                }
                evaluate_binary(*op, left_value, right_value.value, operand_type, query)?
            }
            ExprKind::Operator(function, arguments) => {
                let effects = function.effects();
                constant.set(!effects.volatile && !effects.external_access);
                match arguments.as_slice() {
                    [argument] => function.apply(&[eval(argument)?], query)?,
                    [left, right]
                        if matches!(
                            function.signature().operator,
                            Operator::Like | Operator::NotLike
                        ) =>
                    {
                        if constant_null_expression(right) {
                            if literal_cast_expression(left) {
                                self.evaluate_with_provenance(left, row, context)?;
                            }
                            return checked_evaluated(Value::Null, &expression.data_type, true);
                        }
                        let mut right_value = if right.is_pure_and_total() && !row_dependent(right)
                        {
                            Some(self.evaluate_with_provenance(right, row, context)?)
                        } else {
                            None
                        };
                        if let Some(right) = &right_value {
                            constant.set(
                                constant.get() && right.provenance == ArgumentProvenance::Constant,
                            );
                        }
                        if right_value.as_ref().is_some_and(|right| {
                            right.value.is_null()
                                && right.provenance == ArgumentProvenance::Constant
                        }) {
                            Value::Null
                        } else {
                            let left_value = self.evaluate_with_provenance(left, row, context)?;
                            constant.set(
                                constant.get()
                                    && left_value.provenance == ArgumentProvenance::Constant,
                            );
                            if left_value.value.is_null()
                                && left_value.provenance == ArgumentProvenance::Constant
                            {
                                Value::Null
                            } else {
                                let right_value = if let Some(right) = right_value.take() {
                                    right
                                } else {
                                    let result =
                                        self.evaluate_with_provenance(right, row, context)?;
                                    constant.set(
                                        constant.get()
                                            && result.provenance == ArgumentProvenance::Constant,
                                    );
                                    result
                                };
                                function.apply(&[left_value.value, right_value.value], query)?
                            }
                        }
                    }
                    [left, right] => function.apply(&[eval(left)?, eval(right)?], query)?,
                    _ => return Err(Error::Internal("operator argument count".into())),
                }
            }
            ExprKind::Scalar(function, arguments) => {
                let mut values = Vec::with_capacity(arguments.len());
                let mut provenance = Vec::with_capacity(arguments.len());
                let effects = function.effects();
                constant.set(!effects.volatile && !effects.external_access);
                for argument in arguments.iter().take(
                    if matches!(function.argument_evaluation(), ArgumentEvaluation::TypeOnly) {
                        0
                    } else {
                        arguments.len()
                    },
                ) {
                    let result = self.evaluate_with_provenance(argument, row, context)?;
                    constant
                        .set(constant.get() && result.provenance == ArgumentProvenance::Constant);
                    let value = result.value;
                    if matches!(
                        function.argument_evaluation(),
                        ArgumentEvaluation::NullOnConstant
                    ) && result.provenance == ArgumentProvenance::Constant
                        && value.is_null()
                    {
                        // The selected policy permits stopping only after this
                        // child was executed and its value/metadata validated.
                        for data_type in [&argument.data_type, &expression.data_type] {
                            query.types().bind(data_type)?.validate(&value, query)
                                .map_err(|error| match error {
                                    Error::Conversion(_) => Error::Internal("constant NULL argument or result differs from its selected type".into()),
                                    other => other,
                                })?;
                        }
                        return checked_evaluated(value, &expression.data_type, true);
                    }
                    if matches!(
                        function.argument_evaluation(),
                        ArgumentEvaluation::FirstNonNull
                    ) && !value.is_null()
                    {
                        return checked_evaluated(value, &expression.data_type, constant.get());
                    }
                    values.push(value);
                    provenance.push(result.provenance);
                }
                let value = function.evaluate_with_provenance(&values, &provenance, query)?;
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
            ExprKind::Between(input, lower, upper, operand_type, eager_bounds) => {
                let input = self.evaluate_with_provenance(input, row, context)?;
                let input_is_constant = input.provenance == ArgumentProvenance::Constant;
                constant.set(constant.get() && input_is_constant);
                let input = input.value;
                if input.is_null() && (input_is_constant || *eager_bounds) {
                    Value::Null
                } else if *eager_bounds {
                    let lower = eval(lower)?;
                    let upper = eval(upper)?;
                    if input.is_null() || lower.is_null() || upper.is_null() {
                        Value::Null
                    } else {
                        Value::Boolean(
                            !operand_type.compare(&input, &lower, query)?.is_lt()
                                && !operand_type.compare(&input, &upper, query)?.is_gt(),
                        )
                    }
                } else {
                    let mut unknown = input.is_null();
                    let mut failed = false;
                    'bounds: for total in [true, false] {
                        for (bound, lower_bound) in
                            [(lower.as_ref(), true), (upper.as_ref(), false)]
                        {
                            if bound.is_pure_and_total() != total {
                                continue;
                            }
                            let bound = eval(bound)?;
                            if bound.is_null() {
                                unknown = true;
                                continue;
                            }
                            if input.is_null() {
                                continue;
                            }
                            let ordering = operand_type.compare(&input, &bound, query)?;
                            if (lower_bound && ordering.is_lt())
                                || (!lower_bound && ordering.is_gt())
                            {
                                failed = true;
                                break 'bounds;
                            }
                        }
                    }
                    if failed {
                        Value::Boolean(false)
                    } else if unknown {
                        Value::Null
                    } else {
                        Value::Boolean(true)
                    }
                }
            }
            ExprKind::InList(needle, list, negated, operand_type) => {
                let row_independent_needle = !row_dependent(needle);
                let needle = self.evaluate_with_provenance(needle, row, context)?;
                let needle_is_constant = needle.provenance == ArgumentProvenance::Constant;
                constant.set(constant.get() && needle_is_constant);
                let needle = needle.value;
                if needle.is_null() && (needle_is_constant || row_independent_needle) {
                    Value::Null
                } else {
                    let mut unknown = needle.is_null();
                    let mut matched = false;
                    // DuckDB lowers an IN list with a non-foldable RHS to a
                    // conjunction of comparisons instead of its large-list join.
                    // Total branches may therefore establish the result before an
                    // effectful or failing sibling is demanded. Keep source order
                    // within each class and defer only expressions whose evaluation
                    // can have observable behavior.
                    for total in [true, false] {
                        for candidate in list {
                            if candidate.is_pure_and_total() != total {
                                continue;
                            }
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
            }
        };
        checked_evaluated(value, &expression.data_type, constant.get())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ordinary_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_comparison_null(
    child: &crate::DataType,
    result: &crate::DataType,
    operand: &crate::common::type_registry::BoundType,
    query: &QueryContext,
) -> Result<()> {
    query.check()?;
    if child != operand.data_type() || *result != crate::DataType::Boolean {
        return Err(Error::Internal(
            "constant NULL comparison metadata mismatch".into(),
        ));
    }
    // Keep the selected operand adapter, not a fresh built-in validation path.
    operand
        .validate(&Value::Null, query)
        .and_then(|()| query.types().bind(result)?.validate(&Value::Null, query))
        .map_err(comparison_validation_error)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn comparison_validation_error(error: Error) -> Error {
    match error {
        Error::Conversion(_) => {
            Error::Internal("constant NULL comparison failed selected validation".into())
        }
        other => other,
    }
}

fn row_dependent(expression: &BoundExpr) -> bool {
    let mut dependent = matches!(
        expression.kind,
        ExprKind::Column(_) | ExprKind::OuterColumn { .. } | ExprKind::Subquery(_)
    );
    expression.visit_children(&mut |child| dependent |= row_dependent(child));
    dependent
}

fn constant_null_expression(expression: &BoundExpr) -> bool {
    match &expression.kind {
        ExprKind::Literal(Value::Null) | ExprKind::Parameter(Value::Null) => true,
        ExprKind::Cast(inner, _, _) => constant_null_expression(inner),
        _ => false,
    }
}

fn literal_cast_expression(expression: &BoundExpr) -> bool {
    let ExprKind::Cast(inner, _, _) = &expression.kind else {
        return false;
    };
    matches!(inner.kind, ExprKind::Literal(_) | ExprKind::Parameter(_))
        || literal_cast_expression(inner)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn checked_evaluated(
    value: Value,
    data_type: &crate::DataType,
    constant: bool,
) -> Result<EvaluatedValue> {
    Ok(EvaluatedValue {
        value: checked_value(value, data_type)?,
        provenance: if constant {
            ArgumentProvenance::Constant
        } else {
            ArgumentProvenance::Unknown
        },
    })
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
