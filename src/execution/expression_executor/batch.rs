use super::*;
use crate::common::{
    DataType,
    vector::{DataChunk, Vector},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn evaluate_expression_rows<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Vector> {
    let mut row = Vec::with_capacity(input.columns().len());
    let mut values = Vec::with_capacity(input.len());
    let context = BatchContext {
        parent: context,
        input,
    };
    for index in 0..input.len() {
        context.query().check()?;
        input.read_row(index, &mut row)?;
        values.push(evaluator.evaluate_with_provenance(expression, &row, &context)?);
    }
    result_column(expression.data_type.clone(), values, context.query())
}

/// Materialize source-defined physical-batch subexpressions before preserving
/// ordinary row evaluation for their enclosing tree. A physical-batch callback
/// owns the evaluation boundary of its arguments: it receives each child as a
/// complete vector, in source argument order, before reading its row zero.
/// Its enclosing tree still uses ordinary row evaluation, which keeps lazy
/// branches and fallible/effectful parents in row order.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn evaluate_with_physical_batches<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Option<Vector>> {
    if input.is_empty() {
        return Ok(None);
    }
    let mut rewritten = expression.clone();
    let mut columns = input.columns().to_vec();
    if !materialize_physical_batches(evaluator, &mut rewritten, input, context, &mut columns)? {
        return Ok(None);
    }
    let input = DataChunk::new(columns, input.len())?;
    evaluate_expression_rows(evaluator, &rewritten, &input, context).map(Some)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn materialize_physical_batches<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &mut BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
    columns: &mut Vec<Vector>,
) -> Result<bool> {
    if matches!(expression.kind, ExprKind::Case(..)) && expression.uses_physical_batch() {
        let (output, _) =
            evaluate_case_with_semantic_provenance(evaluator, expression, input, context)?;
        let column = columns.len();
        columns.push(output);
        expression.kind = ExprKind::Column(column);
        return Ok(true);
    }
    if let ExprKind::Scalar(function, _) = &expression.kind
        && matches!(
            function.argument_evaluation(),
            ArgumentEvaluation::FirstNonNull
        )
        && expression.uses_physical_batch()
    {
        let output =
            evaluate_first_non_null_with_physical_batches(evaluator, expression, input, context)?;
        let column = columns.len();
        columns.push(output);
        expression.kind = ExprKind::Column(column);
        return Ok(true);
    }
    if let ExprKind::Scalar(function, _) = &expression.kind
        && matches!(
            function.argument_evaluation(),
            ArgumentEvaluation::NullOnConstant
        )
        && expression.uses_physical_batch()
    {
        let output =
            evaluate_null_on_constant_with_physical_batches(evaluator, expression, input, context)?;
        let column = columns.len();
        columns.push(output);
        expression.kind = ExprKind::Column(column);
        return Ok(true);
    }
    let physical = matches!(
        &expression.kind,
        ExprKind::Scalar(function, _) if function.uses_physical_batch()
    );
    if physical {
        let output = evaluate_physical_scalar(evaluator, expression, input, context)?;
        let column = columns.len();
        columns.push(output);
        expression.kind = ExprKind::Column(column);
        return Ok(true);
    }

    let contains_physical_batch = expression.uses_physical_batch();
    let mut found = false;
    let mut visit = |child: &mut BoundExpr| -> Result<()> {
        found |= materialize_physical_batches(evaluator, child, input, context, columns)?;
        Ok(())
    };
    match &mut expression.kind {
        ExprKind::Literal(_)
        | ExprKind::Parameter(_)
        | ExprKind::Column(_)
        | ExprKind::OuterColumn { .. }
        | ExprKind::Subquery(_) => {}
        ExprKind::Cast(inner, ..) | ExprKind::Unary(_, inner) => visit(inner)?,
        ExprKind::Binary(_, left, right, _) => {
            if contains_physical_batch {
                materialize_source_child(evaluator, left, input, context, columns)?;
                materialize_source_child(evaluator, right, input, context, columns)?;
                found = true;
            } else {
                visit(left)?;
                visit(right)?;
            }
        }
        ExprKind::Operator(_, arguments) => {
            if contains_physical_batch {
                for argument in arguments {
                    materialize_source_child(evaluator, argument, input, context, columns)?;
                }
                found = true;
            } else {
                for argument in arguments {
                    visit(argument)?;
                }
            }
        }
        ExprKind::Scalar(function, arguments) => {
            if !matches!(function.argument_evaluation(), ArgumentEvaluation::TypeOnly) {
                if contains_physical_batch
                    && matches!(function.argument_evaluation(), ArgumentEvaluation::Eager)
                {
                    for argument in arguments {
                        materialize_source_child(evaluator, argument, input, context, columns)?;
                    }
                    found = true;
                } else {
                    for argument in arguments {
                        visit(argument)?;
                    }
                }
            }
        }
        ExprKind::Case(branches, otherwise) => {
            for (condition, value) in branches {
                visit(condition)?;
                visit(value)?;
            }
            visit(otherwise)?;
        }
        ExprKind::Between(value, lower, upper, ..) => {
            if contains_physical_batch {
                materialize_source_child(evaluator, value, input, context, columns)?;
                materialize_source_child(evaluator, lower, input, context, columns)?;
                materialize_source_child(evaluator, upper, input, context, columns)?;
                found = true;
            } else {
                visit(value)?;
                visit(lower)?;
                visit(upper)?;
            }
        }
        ExprKind::InList(value, list, ..) => {
            if contains_physical_batch {
                materialize_source_child(evaluator, value, input, context, columns)?;
                for candidate in list {
                    materialize_source_child(evaluator, candidate, input, context, columns)?;
                }
                found = true;
            } else {
                visit(value)?;
                for candidate in list {
                    visit(candidate)?;
                }
            }
        }
    }
    Ok(found)
}

/// A generic parent with a physical-batch descendant evaluates its immediate
/// children as complete vectors in source order before ordinary row evaluation
/// resumes for the parent. This keeps an earlier fallible sibling from being
/// overtaken by a later physical callback.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn materialize_source_child<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    child: &mut BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
    columns: &mut Vec<Vector>,
) -> Result<()> {
    let output = evaluate_selected_with_physical_batches(evaluator, child, input, context)?;
    let column = columns.len();
    columns.push(output);
    child.kind = ExprKind::Column(column);
    Ok(())
}

/// Evaluate a physical-batch scalar's children through the selected evaluator
/// rather than through the total-only column evaluator. This intentionally
/// allows a valid fallible child (for example VARCHAR -> ENUM) while retaining
/// its vector error order. CASE/COALESCE children retain their own selected
/// subsets through `evaluate_batch`.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn evaluate_physical_scalar<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Vector> {
    let ExprKind::Scalar(function, arguments) = &expression.kind else {
        unreachable!("caller selected physical scalar")
    };
    let columns = arguments
        .iter()
        .map(|argument| evaluator.evaluate_batch(argument, input, context))
        .collect::<Result<Vec<_>>>()?;
    let arguments = DataChunk::new(columns, input.len())?;
    let output = function
        .evaluate_batch(&arguments, context.query())?
        .ok_or_else(|| Error::Internal("physical-batch scalar lacks a batch callback".into()))?;
    if output.data_type() != &expression.data_type || output.len() != input.len() {
        return Err(Error::Internal(
            "physical-batch scalar differs from its bound type or cardinality".into(),
        ));
    }
    context.query().check()?;
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn evaluate_selected_with_physical_batches<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Vector> {
    if let Some(output) = evaluate_with_physical_batches(evaluator, expression, input, context)? {
        Ok(output)
    } else {
        evaluate_expression_rows(evaluator, expression, input, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn evaluate_first_non_null_with_physical_batches<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Vector> {
    let ExprKind::Scalar(_, arguments) = &expression.kind else {
        unreachable!("caller selected scalar")
    };
    let mut active = (0..input.len()).collect::<Vec<_>>();
    let mut values = vec![Value::Null; input.len()];
    for argument in arguments {
        if active.is_empty() {
            break;
        }
        let selected = input.select(&active)?;
        let results =
            evaluate_selected_with_physical_batches(evaluator, argument, &selected, context)?;
        let mut remaining = Vec::new();
        for (offset, &index) in active.iter().enumerate() {
            let value = results.get(offset).expect("validated scalar argument");
            if value.is_null() {
                remaining.push(index);
            } else {
                values[index] = value.clone();
            }
        }
        active = remaining;
    }
    let output = Vector::flat(expression.data_type.clone(), values)?;
    context
        .query()
        .types()
        .bind(&expression.data_type)?
        .validate_vector(&output, context.query())?;
    Ok(output)
}

/// NullOnConstant is a semantic provenance contract, not an encoding test.
/// Evaluate each direct child once in source order, retaining a physical child
/// as Unknown even if its callback happens to return a constant vector.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn evaluate_null_on_constant_with_physical_batches<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Vector> {
    let ExprKind::Scalar(function, arguments) = &expression.kind else {
        unreachable!("caller selected NullOnConstant scalar")
    };
    let mut columns = Vec::with_capacity(arguments.len());
    let mut provenance = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let (column, child_provenance) =
            evaluate_child_with_semantic_provenance(evaluator, argument, input, context)?;
        let constant_null = child_provenance == ArgumentProvenance::Constant
            && column.constant_value().is_some_and(Value::is_null);
        columns.push(column);
        provenance.push(child_provenance);
        if constant_null {
            for data_type in [&argument.data_type, &expression.data_type] {
                context
                    .query()
                    .types()
                    .bind(data_type)?
                    .validate(&Value::Null, context.query())
                    .map_err(|error| match error {
                        Error::Conversion(_) => Error::Internal(
                            "constant NULL argument or result differs from its selected type"
                                .into(),
                        ),
                        other => other,
                    })?;
            }
            context.query().check()?;
            return Vector::constant(expression.data_type.clone(), Value::Null, input.len());
        }
    }
    let arguments = DataChunk::new(columns, input.len())?;
    let mut row = Vec::with_capacity(arguments.columns().len());
    let mut values = Vec::with_capacity(input.len());
    for index in 0..input.len() {
        if index % 1024 == 0 {
            context.query().check()?;
        }
        arguments.read_row(index, &mut row)?;
        let value = function.evaluate_with_provenance(&row, &provenance, context.query())?;
        context
            .query()
            .types()
            .bind(&expression.data_type)?
            .validate(&value, context.query())?;
        values.push(value);
    }
    // This path exists because it contains a physical descendant. Do not
    // upgrade its output merely because every observed value is equal.
    Vector::flat(expression.data_type.clone(), values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn evaluate_child_with_semantic_provenance<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<(Vector, ArgumentProvenance)> {
    // NULLIF's expanded CASE is commonly wrapped in a combination cast. Keep
    // the selected CASE vector and its runtime encoding across that wrapper
    // instead of sending the whole cast tree through generic materialization.
    if let ExprKind::Cast(inner, cast, try_cast) = &expression.kind {
        let (column, _) =
            evaluate_child_with_semantic_provenance(evaluator, inner, input, context)?;
        let column = if let Some(value) = column.constant_value() {
            Vector::constant(
                expression.data_type.clone(),
                if *try_cast {
                    cast.apply_try(value, context.query())?
                } else {
                    cast.apply(value, context.query())?
                },
                input.len(),
            )?
        } else {
            if *try_cast {
                Vector::flat(
                    expression.data_type.clone(),
                    column
                        .values()
                        .map(|value| cast.apply_try(value, context.query()))
                        .collect::<Result<Vec<_>>>()?,
                )?
            } else {
                cast.apply_batch(&column, context.query())?
            }
        };
        return Ok((
            column.clone(),
            if column.constant_value().is_some() {
                ArgumentProvenance::Constant
            } else {
                ArgumentProvenance::Unknown
            },
        ));
    }
    if let ExprKind::Scalar(function, arguments) = &expression.kind
        && arguments.len() == 1
        && !matches!(function.argument_evaluation(), ArgumentEvaluation::TypeOnly)
    {
        let (column, provenance) =
            evaluate_child_with_semantic_provenance(evaluator, &arguments[0], input, context)?;
        if let Some(value) = column.constant_value()
            && !function.effects().volatile
            && !function.effects().external_access
        {
            let value = function.evaluate_with_provenance(
                std::slice::from_ref(value),
                std::slice::from_ref(&provenance),
                context.query(),
            )?;
            return Ok((
                Vector::constant(expression.data_type.clone(), value, input.len())?,
                ArgumentProvenance::Constant,
            ));
        }
        let mut values = Vec::with_capacity(input.len());
        for value in column.values() {
            values.push(function.evaluate_with_provenance(
                std::slice::from_ref(value),
                std::slice::from_ref(&provenance),
                context.query(),
            )?);
        }
        return Ok((
            Vector::flat(expression.data_type.clone(), values)?,
            ArgumentProvenance::Unknown,
        ));
    }
    if let ExprKind::Operator(function, arguments) = &expression.kind
        && arguments.len() == 1
    {
        let (column, _) =
            evaluate_child_with_semantic_provenance(evaluator, &arguments[0], input, context)?;
        if let Some(value) = column.constant_value()
            && !function.effects().volatile
            && !function.effects().external_access
        {
            return Ok((
                Vector::constant(
                    expression.data_type.clone(),
                    function.apply(std::slice::from_ref(value), context.query())?,
                    input.len(),
                )?,
                ArgumentProvenance::Constant,
            ));
        }
        return Ok((
            Vector::flat(
                expression.data_type.clone(),
                column
                    .values()
                    .map(|value| function.apply(std::slice::from_ref(value), context.query()))
                    .collect::<Result<Vec<_>>>()?,
            )?,
            ArgumentProvenance::Unknown,
        ));
    }
    if matches!(expression.kind, ExprKind::Case(..)) {
        return evaluate_case_with_semantic_provenance(evaluator, expression, input, context);
    }
    if expression.uses_physical_batch() {
        let column =
            evaluate_selected_with_physical_batches(evaluator, expression, input, context)?;
        return Ok((
            column.clone(),
            if column.constant_value().is_some() {
                ArgumentProvenance::Constant
            } else {
                ArgumentProvenance::Unknown
            },
        ));
    }
    let batch_context = BatchContext {
        parent: context,
        input,
    };
    let mut row = Vec::with_capacity(input.columns().len());
    let mut values = Vec::with_capacity(input.len());
    let mut constant = true;
    for index in 0..input.len() {
        batch_context.query().check()?;
        input.read_row(index, &mut row)?;
        let value = evaluator.evaluate_with_provenance(expression, &row, &batch_context)?;
        constant &= value.provenance == ArgumentProvenance::Constant;
        values.push(value);
    }
    let column = result_column(expression.data_type.clone(), values, batch_context.query())?;
    Ok((
        column,
        if constant {
            ArgumentProvenance::Constant
        } else {
            ArgumentProvenance::Unknown
        },
    ))
}

/// CASE owns selected branch demand. Its result provenance is the provenance
/// of the branches that actually supplied rows, never the encoding of a
/// physical condition or an unselected physical branch.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn evaluate_case_with_semantic_provenance<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<(Vector, ArgumentProvenance)> {
    let ExprKind::Case(branches, otherwise) = &expression.kind else {
        unreachable!("caller selected CASE")
    };
    let mut active = (0..input.len()).collect::<Vec<_>>();
    let mut values = vec![Value::Null; input.len()];
    for (condition, value) in branches {
        if active.is_empty() {
            break;
        }
        let selected = input.select(&active)?;
        let selected_offsets = super::select_predicate_with_physical_batches(
            evaluator, condition, &selected, context,
        )?;
        let mut matched = Vec::with_capacity(selected_offsets.len());
        let mut remaining = Vec::new();
        let mut selected_offset = 0;
        for (offset, &index) in active.iter().enumerate() {
            if selected_offsets.get(selected_offset) == Some(&offset) {
                matched.push(index);
                selected_offset += 1;
            } else {
                remaining.push(index);
            }
        }
        if !matched.is_empty() {
            let selected = input.select(&matched)?;
            let (results, provenance) =
                evaluate_child_with_semantic_provenance(evaluator, value, &selected, context)?;
            if matched.len() == input.len() {
                return Ok((results, provenance));
            }
            for (offset, &index) in matched.iter().enumerate() {
                values[index] = results.get(offset).expect("validated CASE result").clone();
            }
        }
        active = remaining;
    }
    if !active.is_empty() {
        let selected = input.select(&active)?;
        let (results, provenance) =
            evaluate_child_with_semantic_provenance(evaluator, otherwise, &selected, context)?;
        if active.len() == input.len() {
            return Ok((results, provenance));
        }
        for (offset, &index) in active.iter().enumerate() {
            values[index] = results
                .get(offset)
                .expect("validated CASE otherwise")
                .clone();
        }
    }
    let output = Vector::flat(expression.data_type.clone(), values)?;
    Ok((output, ArgumentProvenance::Unknown))
}

/// Evaluates total scalar trees by columns, using retained type and operator
/// adapters. Potential errors, lazy branches, effects and relational stages
/// keep scalar row order. ScalarEvaluator remains independently selectable.
#[derive(Default)]
pub struct BatchedEvaluator;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for BatchedEvaluator {
    fn uniform_selection(
        &self,
        expression: &BoundExpr,
        input: &DataChunk,
        context: &dyn EvaluationContext,
    ) -> Result<Option<bool>> {
        let ExprKind::Binary(op, left, right, data_type) = &expression.kind else {
            return Ok(None);
        };
        let (ExprKind::Column(index), Some(value)) = (&left.kind, right.constant_value()) else {
            return Ok(None);
        };
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            return Ok(None);
        }
        let declared_left = &left.data_type;
        let Some(left) = input.columns().get(*index) else {
            return Err(Error::Internal("comparison column outside input".into()));
        };
        if matches!(left.constant_value(), Some(Value::Null)) {
            if left.data_type() != declared_left || left.len() != input.len() {
                return Err(Error::Internal(
                    "comparison input differs from its bound metadata".into(),
                ));
            }
            validate_comparison_null(
                left.data_type(),
                &expression.data_type,
                data_type,
                context.query(),
            )?;
            return Ok(Some(false));
        }
        let right = Vector::constant(right.data_type.clone(), value.clone(), input.len())?;
        use std::cmp::Ordering;
        let predicate = crate::common::type_registry::ComparisonPredicate {
            less: comparison_matches(*op, Ordering::Less),
            equal: comparison_matches(*op, Ordering::Equal),
            greater: comparison_matches(*op, Ordering::Greater),
        };
        data_type.uniform_comparison(left, &right, predicate, context.query())
    }
    fn name(&self) -> &'static str {
        "batched-expression"
    }
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        ScalarEvaluator.evaluate(expression, row, context)
    }
    fn evaluate_with_provenance(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<EvaluatedValue> {
        ScalarEvaluator.evaluate_with_provenance(expression, row, context)
    }
    fn evaluate_batch(
        &self,
        expression: &BoundExpr,
        input: &DataChunk,
        context: &dyn EvaluationContext,
    ) -> Result<Vector> {
        if input.len() > 1
            && !expression.uses_physical_batch()
            && let Some(output) = dictionary_expression(expression, input, context)?
        {
            Ok(output)
        } else if input.len() > 1 && expression.is_pure_and_total() {
            evaluate_columns(expression, input, context)
        } else {
            if input.len() > 1
                && let Some(output) =
                    evaluate_speculative_expression(self, expression, input, context)?
            {
                return Ok(output);
            }
            if let Some(output) = evaluate_with_physical_batches(self, expression, input, context)?
            {
                return Ok(output);
            }
            evaluate_expression_rows(self, expression, input, context)
        }
    }
    fn select_batch(
        &self,
        expression: &BoundExpr,
        input: &DataChunk,
        context: &dyn EvaluationContext,
    ) -> Result<Vec<usize>> {
        if let Some(selected) = select_conjunction_rows(self, expression, input, context)? {
            return Ok(selected);
        }
        if input.len() > 1
            && expression.is_pure_and_total()
            && let ExprKind::Binary(op, left, right, data_type) = &expression.kind
        {
            use std::cmp::Ordering;
            let predicate = crate::common::type_registry::ComparisonPredicate {
                less: comparison_matches(*op, Ordering::Less),
                equal: comparison_matches(*op, Ordering::Equal),
                greater: comparison_matches(*op, Ordering::Greater),
            };
            let left = if let (ExprKind::Cast(inner, cast, false), Some(value)) =
                (&left.kind, right.constant_value())
            {
                let inner = evaluate_columns(inner, input, context)?;
                if !matches!(inner.constant_value(), Some(Value::Null))
                    && !value.is_null()
                    && let Some(selected) = cast.select_integer_comparison(
                        &inner,
                        value,
                        predicate,
                        data_type,
                        context.query(),
                    )?
                {
                    return Ok(selected);
                }
                cast.apply_batch(&inner, context.query())?
            } else {
                evaluate_columns(left, input, context)?
            };
            if constant_null_comparison(&left, expression, data_type, context.query())? {
                return Ok(Vec::new());
            }
            let right = evaluate_columns(right, input, context)?;
            if constant_null_comparison(&right, expression, data_type, context.query())? {
                data_type
                    .validate_vector(&left, context.query())
                    .map_err(comparison_validation_error)?;
                return Ok(Vec::new());
            }
            return data_type.select_comparison(&left, &right, predicate, context.query());
        }
        select_boolean(
            &self.evaluate_batch(expression, input, context)?,
            input.len(),
            context.query(),
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// A pure eager function may expose a vector callback even when one child is
/// not proved total. Evaluate into temporary columns first; a data error
/// discards them and returns to scalar row order, while successful work can
/// retain the callback's compact encoding.
fn evaluate_speculative_expression<T: ExpressionEvaluator + ?Sized>(
    evaluator: &T,
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Option<Vector>> {
    let arguments: &[BoundExpr] = match &expression.kind {
        ExprKind::Scalar(function, arguments) => {
            let effects = function.effects();
            if effects.volatile
                || effects.external_access
                || !matches!(function.argument_evaluation(), ArgumentEvaluation::Eager)
                || !function.supports_batch_evaluation(
                    &arguments
                        .iter()
                        .map(|argument| argument.data_type.clone())
                        .collect::<Vec<_>>(),
                )
            {
                return Ok(None);
            }
            arguments
        }
        ExprKind::Operator(function, arguments) => {
            let effects = function.effects();
            if effects.volatile || effects.external_access {
                return Ok(None);
            }
            arguments
        }
        ExprKind::Cast(inner, _, false) => std::slice::from_ref(inner.as_ref()),
        _ => return Ok(None),
    };
    if arguments.iter().any(|argument| !argument.is_effect_free()) {
        return Ok(None);
    }
    let mut columns = Vec::with_capacity(arguments.len());
    for argument in arguments {
        match evaluator.evaluate_batch(argument, input, context) {
            Ok(column) => columns.push(column),
            Err(error) if speculative_data_error(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    let arguments = DataChunk::new(columns, input.len())?;
    let output = match &expression.kind {
        ExprKind::Scalar(function, _) => match function.evaluate_batch(&arguments, context.query())
        {
            Ok(Some(output)) => output,
            Ok(None)
            | Err(
                Error::Conversion(_)
                | Error::Execution(_)
                | Error::OutOfRange(_)
                | Error::InvalidInput(_)
                | Error::InvalidType(_),
            ) => return Ok(None),
            Err(error) => return Err(error),
        },
        ExprKind::Operator(function, _) => {
            match function.apply_batch(&arguments, context.query()) {
                Ok(output) => output,
                Err(
                    Error::Conversion(_)
                    | Error::Execution(_)
                    | Error::OutOfRange(_)
                    | Error::InvalidInput(_)
                    | Error::InvalidType(_),
                ) => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        ExprKind::Cast(_, cast, false) => match cast.apply_batch(
            arguments
                .columns()
                .first()
                .expect("speculative cast argument"),
            context.query(),
        ) {
            Ok(output) => output,
            Err(error) if speculative_data_error(&error) => return Ok(None),
            Err(error) => return Err(error),
        },
        _ => unreachable!("matched speculative expression"),
    };
    if output.len() != input.len() || output.data_type() != &expression.data_type {
        return Err(Error::Internal(
            "speculative expression batch differs from binding".into(),
        ));
    }
    context
        .query()
        .bind_type(&expression.data_type)?
        .validate_vector(&output, context.query())?;
    Ok(Some(output))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn speculative_data_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Conversion(_)
            | Error::Execution(_)
            | Error::OutOfRange(_)
            | Error::InvalidInput(_)
            | Error::InvalidType(_)
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn dictionary_expression(
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Option<Vector>> {
    let mut column = None;
    if !single_pure_input(expression, &mut column) {
        return Ok(None);
    }
    let Some(column) = column else {
        return Ok(None);
    };
    let Some((dictionary, selection)) = input.columns().get(column).and_then(Vector::dictionary)
    else {
        return Ok(None);
    };
    if dictionary.len() > input.len() / 4 {
        return Ok(None);
    }
    // Cache only complete root results, at their first logical occurrence.
    // Fallible descendants are never moved across another row's parent. This
    // preserves the first error while reusing identical, effect-free inputs.
    let mut initialized = vec![false; dictionary.len()];
    let mut values = vec![Value::Null; dictionary.len()];
    let mut row = vec![Value::Null; input.columns().len()];
    let context = BatchContext {
        parent: context,
        input,
    };
    let mut all_constant = true;
    for (offset, &index) in selection.iter().enumerate() {
        if offset % 1024 == 0 {
            context.query().check()?;
        }
        if !initialized[index] {
            row[column] = dictionary
                .get(index)
                .expect("checked dictionary index")
                .clone();
            let result = ScalarEvaluator.evaluate_with_provenance(expression, &row, &context)?;
            all_constant &= result.provenance == ArgumentProvenance::Constant;
            values[index] = result.value;
            initialized[index] = true;
        }
    }
    context.query().check()?;
    if all_constant && !selection.is_empty() {
        return result_column(
            expression.data_type.clone(),
            selection
                .iter()
                .map(|&index| EvaluatedValue {
                    value: values[index].clone(),
                    provenance: ArgumentProvenance::Constant,
                })
                .collect(),
            context.query(),
        )
        .map(Some);
    }
    Ok(Some(
        std::sync::Arc::new(Vector::flat(expression.data_type.clone(), values)?)
            .select(selection.to_vec())?,
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn single_pure_input(expression: &BoundExpr, column: &mut Option<usize>) -> bool {
    match &expression.kind {
        ExprKind::Literal(_) | ExprKind::Parameter(_) => true,
        ExprKind::Column(index) => {
            if column.is_some_and(|column| column != *index) {
                return false;
            }
            *column = Some(*index);
            true
        }
        ExprKind::Cast(inner, ..) | ExprKind::Unary(_, inner) => single_pure_input(inner, column),
        ExprKind::Operator(function, arguments)
            if !function.effects().volatile && !function.effects().external_access =>
        {
            arguments
                .iter()
                .all(|argument| single_pure_input(argument, column))
        }
        ExprKind::Scalar(function, arguments)
            if !function.effects().volatile
                && !function.effects().external_access
                && matches!(function.argument_evaluation(), ArgumentEvaluation::Eager) =>
        {
            arguments
                .iter()
                .all(|argument| single_pure_input(argument, column))
        }
        ExprKind::Binary(_, left, right, _) => {
            single_pure_input(left, column) && single_pure_input(right, column)
        }
        // Scalar callbacks, relational dependencies, and other forms retain
        // the selected evaluator's ordinary execution and effect ordering.
        _ => false,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn select_boolean(
    column: &Vector,
    count: usize,
    context: &QueryContext,
) -> Result<Vec<usize>> {
    if column.len() != count || column.data_type() != &DataType::Boolean {
        return Err(Error::Internal(
            "predicate batch differs from Boolean input cardinality".into(),
        ));
    }
    let mut selected = Vec::new();
    for (index, value) in column.values().enumerate() {
        if index % 1024 == 0 {
            context.check()?;
        }
        if matches!(value, Value::Boolean(true)) {
            selected.push(index);
        }
    }
    context.check()?;
    Ok(selected)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn comparison_matches(op: BinaryOp, ordering: std::cmp::Ordering) -> bool {
    match op {
        BinaryOp::Equal => ordering.is_eq(),
        BinaryOp::NotEqual => !ordering.is_eq(),
        BinaryOp::Less => ordering.is_lt(),
        BinaryOp::LessEqual => !ordering.is_gt(),
        BinaryOp::Greater => ordering.is_gt(),
        BinaryOp::GreaterEqual => !ordering.is_lt(),
        BinaryOp::And | BinaryOp::Or => unreachable!("lazy Boolean evaluation uses scalar rows"),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn evaluate_columns(
    expression: &BoundExpr,
    input: &DataChunk,
    context: &dyn EvaluationContext,
) -> Result<Vector> {
    context.query().check()?;
    let eval = |child| evaluate_columns(child, input, context);
    let output = match &expression.kind {
        ExprKind::Literal(value) | ExprKind::Parameter(value) => {
            Vector::constant(expression.data_type.clone(), value.clone(), input.len())?
        }
        ExprKind::Column(index) => input
            .columns()
            .get(*index)
            .cloned()
            .ok_or_else(|| Error::Internal("expression column outside batch".into()))?,
        ExprKind::OuterColumn { depth, column } => Vector::constant(
            expression.data_type.clone(),
            context.outer_column(*depth, *column)?,
            input.len(),
        )?,
        ExprKind::Operator(function, arguments) => {
            let columns = arguments.iter().map(eval).collect::<Result<_>>()?;
            function.apply_batch(&DataChunk::new(columns, input.len())?, context.query())?
        }
        ExprKind::Scalar(function, arguments) => {
            let columns = arguments.iter().map(eval).collect::<Result<Vec<_>>>()?;
            let provenance = columns
                .iter()
                .map(|column| {
                    if column.constant_value().is_some() {
                        ArgumentProvenance::Constant
                    } else {
                        ArgumentProvenance::Unknown
                    }
                })
                .collect::<Vec<_>>();
            let arguments = DataChunk::new(columns, input.len())?;
            if let Some(output) = function.evaluate_batch(&arguments, context.query())? {
                output
            } else {
                let mut row = Vec::with_capacity(arguments.columns().len());
                let mut values = Vec::with_capacity(input.len());
                for index in 0..input.len() {
                    if index % 1024 == 0 {
                        context.query().check()?;
                    }
                    arguments.read_row(index, &mut row)?;
                    values.push(function.evaluate_with_provenance(
                        &row,
                        &provenance,
                        context.query(),
                    )?);
                }
                Vector::flat(expression.data_type.clone(), values)?
            }
        }
        ExprKind::Cast(inner, cast, false) => cast.apply_batch(&eval(inner)?, context.query())?,
        ExprKind::Unary(op, inner) => {
            let inner = eval(inner)?;
            if inner.all_valid() && matches!(op, UnaryOp::IsNull | UnaryOp::IsNotNull) {
                return Vector::constant(
                    DataType::Boolean,
                    Value::Boolean(*op == UnaryOp::IsNotNull),
                    input.len(),
                );
            }
            let values = inner
                .values()
                .map(|value| {
                    Ok(match op {
                        UnaryOp::IsNull => Value::Boolean(value.is_null()),
                        UnaryOp::IsNotNull => Value::Boolean(!value.is_null()),
                        UnaryOp::Not => match value.as_bool()? {
                            Some(value) => Value::Boolean(!value),
                            None => Value::Null,
                        },
                    })
                })
                .collect::<Result<_>>()?;
            Vector::flat(DataType::Boolean, values)?
        }
        ExprKind::Case(branches, otherwise) => {
            // A CASE with retained constant branches is a selection over a
            // tiny value vector, not a freshly materialized value per row.
            // The condition still uses the ordinary total-expression selector
            // and therefore retains SQL NULL/non-match behavior.
            if let [(condition, value)] = branches.as_slice()
                && let (Some(value), Some(otherwise)) =
                    (value.constant_value(), otherwise.constant_value())
            {
                let selected = BatchedEvaluator.select_batch(condition, input, context)?;
                let mut indices = vec![1; input.len()];
                for index in selected {
                    indices[index] = 0;
                }
                let dictionary = std::sync::Arc::new(Vector::flat(
                    expression.data_type.clone(),
                    vec![value.clone(), otherwise.clone()],
                )?);
                return dictionary.select(indices);
            }
            let mut active = Vec::new();
            let mut fallback = otherwise.as_ref();
            for (condition, value) in branches {
                let condition = eval(condition)?;
                match condition.constant_value() {
                    Some(Value::Boolean(true)) => {
                        fallback = value;
                        break;
                    }
                    Some(Value::Boolean(false) | Value::Null) => continue,
                    _ => active.push((condition, eval(value)?)),
                }
            }
            let otherwise = eval(fallback)?;
            if active.is_empty() {
                return Ok(otherwise);
            }
            let mut values = Vec::with_capacity(input.len());
            for index in 0..input.len() {
                if index % 1024 == 0 {
                    context.query().check()?;
                }
                let column = active
                    .iter()
                    .find(|(condition, _)| {
                        matches!(condition.get(index), Some(Value::Boolean(true)))
                    })
                    .map(|(_, value)| value)
                    .unwrap_or(&otherwise);
                values.push(column.get(index).expect("validated CASE column").clone());
            }
            Vector::flat(expression.data_type.clone(), values)?
        }
        ExprKind::Binary(op, left, right, data_type) => {
            let left = eval(left)?;
            if ordinary_comparison(*op)
                && constant_null_comparison(&left, expression, data_type, context.query())?
            {
                return Vector::constant(expression.data_type.clone(), Value::Null, input.len());
            }
            let right = eval(right)?;
            if ordinary_comparison(*op)
                && constant_null_comparison(&right, expression, data_type, context.query())?
            {
                data_type
                    .validate_vector(&left, context.query())
                    .map_err(comparison_validation_error)?;
                return Vector::constant(expression.data_type.clone(), Value::Null, input.len());
            }
            let values = data_type
                .compare_batch(&left, &right, context.query())?
                .into_iter()
                .map(|ordering| match ordering {
                    None => Value::Null,
                    Some(ordering) => Value::Boolean(comparison_matches(*op, ordering)),
                })
                .collect();
            Vector::flat(DataType::Boolean, values)?
        }
        _ => {
            return Err(Error::Internal(
                "expression requires scalar row evaluation".into(),
            ));
        }
    };
    if output.data_type() != &expression.data_type || output.len() != input.len() {
        return Err(Error::Internal(
            "expression batch differs from its bound type or cardinality".into(),
        ));
    }
    context.query().check()?;
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn constant_null_comparison(
    column: &Vector,
    expression: &BoundExpr,
    operand: &crate::common::type_registry::BoundType,
    query: &QueryContext,
) -> Result<bool> {
    if !matches!(column.constant_value(), Some(Value::Null)) {
        return Ok(false);
    }
    validate_comparison_null(column.data_type(), &expression.data_type, operand, query)?;
    Ok(true)
}
