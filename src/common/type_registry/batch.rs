use super::*;
use crate::common::vector::Vector;

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

impl BoundType {
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
