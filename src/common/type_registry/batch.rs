use super::*;
use crate::common::vector::Vector;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn select_values(
    left: &Vector,
    right: &Vector,
    predicate: ComparisonPredicate,
    context: &QueryContext,
    compare: impl Fn(&Value, &Value) -> Result<Ordering>,
) -> Result<Vec<usize>> {
    let mut selected = Vec::with_capacity(left.len());
    if left.all_valid()
        && right.all_valid()
        && let (Some(left), Some(right)) = (left.flat_values(), right.constant_value())
    {
        for (index, value) in left.iter().enumerate() {
            if index % 1024 == 0 {
                context.check()?;
            }
            if predicate.matches(compare(value, right)?) {
                selected.push(index);
            }
        }
    } else {
        for (index, (a, b)) in left.values().zip(right.values()).enumerate() {
            if index % 1024 == 0 {
                context.check()?;
            }
            if !a.is_null() && !b.is_null() && predicate.matches(compare(a, b)?) {
                selected.push(index);
            }
        }
    }
    context.check()?;
    Ok(selected)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn compare_values(
    left: &Vector,
    right: &Vector,
    context: &QueryContext,
    compare: impl Fn(&Value, &Value) -> Result<Ordering>,
) -> Result<Vec<Option<Ordering>>> {
    let mut values = Vec::with_capacity(left.len());
    if left.all_valid()
        && right.all_valid()
        && let (Some(left), Some(right)) = (left.flat_values(), right.constant_value())
    {
        for (index, a) in left.iter().enumerate() {
            if index % 1024 == 0 {
                context.check()?;
            }
            values.push(Some(compare(a, right)?));
        }
        context.check()?;
        return Ok(values);
    }
    for (index, (a, b)) in left.values().zip(right.values()).enumerate() {
        if index % 1024 == 0 {
            context.check()?;
        }
        values.push(if a.is_null() || b.is_null() {
            None
        } else {
            Some(compare(a, b)?)
        });
    }
    context.check()?;
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundType {
    pub fn uniform_comparison(
        &self,
        left: &Vector,
        right: &Vector,
        predicate: ComparisonPredicate,
        context: &QueryContext,
    ) -> Result<Option<bool>> {
        if left.len() != right.len() {
            return Err(Error::Internal(
                "comparison input cardinality differs".into(),
            ));
        }
        self.validate_vector(left, context)?;
        self.validate_vector(right, context)?;
        let result =
            self.adapter
                .uniform_comparison(&self.data_type, left, right, predicate, context);
        context.check()?;
        let result = result?;
        if result == Some(true) && !(left.all_valid() && right.all_valid()) {
            for (index, (a, b)) in left.values().zip(right.values()).enumerate() {
                if index % 1024 == 0 {
                    context.check()?;
                }
                if a.is_null() || b.is_null() {
                    return Err(Error::Internal(
                        "uniform comparison selected a NULL input".into(),
                    ));
                }
            }
        }
        Ok(result)
    }
    pub fn select_comparison(
        &self,
        left: &Vector,
        right: &Vector,
        predicate: ComparisonPredicate,
        context: &QueryContext,
    ) -> Result<Vec<usize>> {
        if left.len() != right.len() {
            return Err(Error::Internal(
                "comparison input cardinality differs".into(),
            ));
        }
        self.validate_vector(left, context)?;
        self.validate_vector(right, context)?;
        let selected =
            self.adapter
                .select_comparison(&self.data_type, left, right, predicate, context);
        context.check()?;
        let selected = selected?;
        if left.all_valid() && right.all_valid() {
            if selected.last().is_some_and(|index| *index >= left.len()) {
                return Err(Error::Internal("comparison selection outside input".into()));
            }
            for start in (0..selected.len()).step_by(1024) {
                context.check()?;
                if selected[start..start.saturating_add(1025).min(selected.len())]
                    .windows(2)
                    .any(|pair| pair[0] >= pair[1])
                {
                    return Err(Error::Internal(
                        "comparison selection must be ordered unique input rows".into(),
                    ));
                }
            }
            return Ok(selected);
        }
        let mut previous = None;
        for (offset, &index) in selected.iter().enumerate() {
            if offset % 1024 == 0 {
                context.check()?;
            }
            if index >= left.len() || previous.is_some_and(|previous| index <= previous) {
                return Err(Error::Internal(
                    "comparison selection must be ordered unique input rows".into(),
                ));
            }
            if !(left.all_valid() && right.all_valid())
                && (left.get(index).is_some_and(Value::is_null)
                    || right.get(index).is_some_and(Value::is_null))
            {
                return Err(Error::Internal("comparison selected a NULL input".into()));
            }
            previous = Some(index);
        }
        Ok(selected)
    }
    /// Vector construction establishes physical validity. This boundary checks
    /// the bound type and any additional invariants of the selected adapter.
    pub fn validate_vector(&self, column: &Vector, context: &QueryContext) -> Result<()> {
        context.check()?;
        if column.data_type() != &self.data_type {
            return Err(Error::Internal("vector differs from its bound type".into()));
        }
        if self.requires_logical_validation() {
            for value in column.values() {
                self.validate(value, context)?;
            }
        }
        context.check()
    }
    pub fn compare_batch(
        &self,
        left: &Vector,
        right: &Vector,
        context: &QueryContext,
    ) -> Result<Vec<Option<Ordering>>> {
        if left.len() != right.len() {
            return Err(Error::Internal(
                "comparison input cardinality differs".into(),
            ));
        }
        self.validate_vector(left, context)?;
        self.validate_vector(right, context)?;
        let result = self
            .adapter
            .compare_batch(&self.data_type, left, right, context);
        context.check()?;
        let result = result?;
        if result.len() != left.len() {
            return Err(Error::Internal(
                "comparison output cardinality differs".into(),
            ));
        }
        if left.all_valid() && right.all_valid() {
            if result.iter().any(Option::is_none) {
                return Err(Error::Internal(
                    "comparison batch violated NULL semantics".into(),
                ));
            }
            return Ok(result);
        }
        for (index, value) in result.iter().enumerate() {
            if index % 1024 == 0 {
                context.check()?;
            }
            if value.is_none()
                != (left.get(index).is_some_and(Value::is_null)
                    || right.get(index).is_some_and(Value::is_null))
            {
                return Err(Error::Internal(
                    "comparison batch violated NULL semantics".into(),
                ));
            }
        }
        Ok(result)
    }
}
