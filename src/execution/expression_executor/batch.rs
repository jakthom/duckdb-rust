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
        let Some(left) = input.columns().get(*index) else {
            return Err(Error::Internal("comparison column outside input".into()));
        };
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
        if input.len() > 1 && expression.is_pure_and_total() {
            evaluate_columns(expression, input, context)
        } else {
            if input.len() > 1
                && let Some(output) = dictionary_expression(expression, input, context)?
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
                if let Some(selected) = cast.select_integer_comparison(
                    &inner,
                    value,
                    predicate,
                    data_type,
                    context.query(),
                )? {
                    return Ok(selected);
                }
                cast.apply_batch(&inner, context.query())?
            } else {
                evaluate_columns(left, input, context)?
            };
            let right = evaluate_columns(right, input, context)?;
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
    for (offset, &index) in selection.iter().enumerate() {
        if offset % 1024 == 0 {
            context.query().check()?;
        }
        if !initialized[index] {
            row[column] = dictionary
                .get(index)
                .expect("checked dictionary index")
                .clone();
            values[index] = ScalarEvaluator.evaluate(expression, &row, context)?;
            initialized[index] = true;
        }
    }
    context.query().check()?;
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
            let right = eval(right)?;
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
