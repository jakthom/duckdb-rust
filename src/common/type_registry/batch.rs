use super::*;
use crate::common::vector::Vector;
use std::collections::HashSet;

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
            if !a.is_null() && !b.is_null() && predicate.matches(compare(&a, &b)?) {
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
            Some(compare(&a, &b)?)
        });
    }
    context.check()?;
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundType {
    pub(crate) fn compare_vector_at_validated(
        &self,
        left: &Vector,
        left_index: usize,
        right: &Vector,
        right_index: usize,
        context: &QueryContext,
    ) -> Result<Ordering> {
        let left_null = left
            .is_null_at(left_index)
            .ok_or_else(|| Error::Internal("comparison vector index".into()))?;
        let right_null = right
            .is_null_at(right_index)
            .ok_or_else(|| Error::Internal("comparison vector index".into()))?;
        match (left_null, right_null) {
            (true, true) => return Ok(Ordering::Equal),
            (true, false) => return Ok(Ordering::Greater),
            (false, true) => return Ok(Ordering::Less),
            (false, false) => {}
        }
        if self.ordering_representation() == OrderingRepresentation::VarcharBytes {
            let left = left
                .varchar_at_validated(left_index)
                .flatten()
                .ok_or_else(|| Error::Internal("validated VARCHAR comparison input".into()))?;
            let right = right
                .varchar_at_validated(right_index)
                .flatten()
                .ok_or_else(|| Error::Internal("validated VARCHAR comparison input".into()))?;
            return Ok(left.cmp(right));
        }
        if let Some(result) = self.adapter.compare_validated_vector_at(
            super::TypeAdapterAccess(()),
            &self.data_type,
            left,
            left_index,
            right,
            right_index,
            context,
        ) {
            let result = result?;
            context.check()?;
            return Ok(result);
        }
        let left = left
            .value(left_index)
            .ok_or_else(|| Error::Internal("comparison vector index".into()))?;
        let right = right
            .value(right_index)
            .ok_or_else(|| Error::Internal("comparison vector index".into()))?;
        compare_nullable_validated(self, &left, &right, context)
    }

    pub(crate) fn compare_vector_value_at_validated(
        &self,
        column: &Vector,
        index: usize,
        value: &Value,
        column_is_left: bool,
        context: &QueryContext,
    ) -> Result<Ordering> {
        let column_null = column
            .is_null_at(index)
            .ok_or_else(|| Error::Internal("comparison vector index".into()))?;
        let value_null = value.is_null();
        let order = match (column_null, value_null) {
            (true, true) => Some(Ordering::Equal),
            (true, false) => Some(Ordering::Greater),
            (false, true) => Some(Ordering::Less),
            (false, false) => None,
        };
        if let Some(order) = order {
            return Ok(if column_is_left {
                order
            } else {
                order.reverse()
            });
        }
        if self.ordering_representation() == OrderingRepresentation::VarcharBytes {
            let column = column
                .varchar_at_validated(index)
                .flatten()
                .ok_or_else(|| Error::Internal("validated VARCHAR comparison input".into()))?;
            let Value::Varchar(value) = value else {
                return Err(Error::Internal(
                    "validated VARCHAR scalar comparison input".into(),
                ));
            };
            let order = column.cmp(value);
            return Ok(if column_is_left {
                order
            } else {
                order.reverse()
            });
        }
        if let Some(result) = self.adapter.compare_validated_vector_value_at(
            super::TypeAdapterAccess(()),
            &self.data_type,
            column,
            index,
            value,
            column_is_left,
            context,
        ) {
            let result = result?;
            context.check()?;
            return Ok(result);
        }
        let column = column
            .value(index)
            .ok_or_else(|| Error::Internal("comparison vector index".into()))?;
        if column_is_left {
            compare_nullable_validated(self, &column, value, context)
        } else {
            compare_nullable_validated(self, value, &column, context)
        }
    }

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
        let selected = if let Some(compared) = self.adapter.compare_validated_batch(
            super::TypeAdapterAccess(()),
            &self.data_type,
            left,
            right,
            context,
        ) {
            compared.and_then(|compared| {
                if compared.len() != left.len() {
                    return Err(Error::Internal(
                        "comparison output cardinality differs".into(),
                    ));
                }
                let mut selected = Vec::new();
                selected.try_reserve_exact(compared.len()).map_err(|_| {
                    Error::Resource("comparison selection allocation failed".into())
                })?;
                for (index, order) in compared.into_iter().enumerate() {
                    if index % 1024 == 0 {
                        context.check()?;
                    }
                    if order.is_some_and(|order| predicate.matches(order)) {
                        selected.push(index);
                    }
                }
                Ok(selected)
            })
        } else {
            self.adapter
                .select_comparison(&self.data_type, left, right, predicate, context)
        };
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
                && (left.is_null_at(index) == Some(true) || right.is_null_at(index) == Some(true))
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
            if let Some(value) = column.constant_value() {
                self.validate(value, context)?;
            } else if let Some((parent, selection)) = column.dictionary() {
                // A dictionary position denotes one exact physical value, so
                // repeated logical rows share the same logical-validation
                // result. Validate only positions used by this view; an
                // unselected parent value grants no proof.
                if parent.len() <= selection.len().saturating_mul(4).max(256) {
                    let mut validated = vec![false; parent.len()];
                    for (offset, &index) in selection.iter().enumerate() {
                        if offset % 1024 == 0 {
                            context.check()?;
                        }
                        let validated = validated.get_mut(index).ok_or_else(|| {
                            Error::Internal("dictionary selection outside parent".into())
                        })?;
                        if !*validated {
                            self.validate(
                                &parent.get(index).expect("checked parent index"),
                                context,
                            )?;
                            *validated = true;
                        }
                    }
                } else {
                    let mut validated = HashSet::with_capacity(selection.len().min(parent.len()));
                    for (offset, &index) in selection.iter().enumerate() {
                        if offset % 1024 == 0 {
                            context.check()?;
                        }
                        if validated.insert(index) {
                            self.validate(
                                &parent.get(index).ok_or_else(|| {
                                    Error::Internal("dictionary selection outside parent".into())
                                })?,
                                context,
                            )?;
                        }
                    }
                }
            } else {
                for value in column.values() {
                    self.validate(&value, context)?;
                }
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
            .compare_validated_batch(
                super::TypeAdapterAccess(()),
                &self.data_type,
                left,
                right,
                context,
            )
            .unwrap_or_else(|| {
                self.adapter
                    .compare_batch(&self.data_type, left, right, context)
            });
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
                != (left.is_null_at(index) == Some(true) || right.is_null_at(index) == Some(true))
            {
                return Err(Error::Internal(
                    "comparison batch violated NULL semantics".into(),
                ));
            }
        }
        Ok(result)
    }
}

fn compare_nullable_validated(
    bound: &BoundType,
    left: &Value,
    right: &Value,
    context: &QueryContext,
) -> Result<Ordering> {
    match (left.is_null(), right.is_null()) {
        (true, true) => Ok(Ordering::Equal),
        (true, false) => Ok(Ordering::Greater),
        (false, true) => Ok(Ordering::Less),
        (false, false) => bound.compare_validated(left, right, context),
    }
}
