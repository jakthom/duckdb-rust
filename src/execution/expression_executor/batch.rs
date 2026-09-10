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
    for index in 0..input.len() {
        context.query().check()?;
        input.read_row(index, &mut row)?;
        values.push(evaluator.evaluate(expression, &row, context)?);
    }
    Vector::flat(expression.data_type.clone(), values)
}

/// Evaluates total scalar trees by columns, using retained type and operator
/// adapters. Potential errors, lazy branches, effects and relational stages
/// keep scalar row order. ScalarEvaluator remains independently selectable.
#[derive(Default)]
pub struct BatchedEvaluator;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for BatchedEvaluator {
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
    fn evaluate_batch(
        &self,
        expression: &BoundExpr,
        input: &DataChunk,
        context: &dyn EvaluationContext,
    ) -> Result<Vector> {
        if input.len() > 1 && expression.is_pure_and_total() {
            evaluate_columns(expression, input, context)
        } else {
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
            let left = evaluate_columns(left, input, context)?;
            let right = evaluate_columns(right, input, context)?;
            let comparisons = data_type.compare_batch(&left, &right, context.query())?;
            let mut selected = Vec::new();
            for (index, ordering) in comparisons.into_iter().enumerate() {
                if index % 1024 == 0 {
                    context.query().check()?;
                }
                if ordering.is_some_and(|ordering| comparison_matches(*op, ordering)) {
                    selected.push(index);
                }
            }
            return Ok(selected);
        }
        select_boolean(
            &self.evaluate_batch(expression, input, context)?,
            input.len(),
            context.query(),
        )
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
        ExprKind::Literal(value) => {
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
        ExprKind::Unary(op, inner) => {
            let values = eval(inner)?
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
