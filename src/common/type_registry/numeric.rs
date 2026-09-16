use super::{
    ComparisonPredicate, IntegerLiteral, KeyRepresentation, KeyWriter, TypeAdapter, TypeRegistry,
    ValueValidation,
};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};
use std::cmp::Ordering;

/// Full-width unsigned values and parameterized fixed-point decimals. These
/// expose equality coefficients separately from signed-integer arithmetic.
#[derive(Debug)]
pub struct ExactNumericTypes;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for ExactNumericTypes {
    fn uniform_comparison(
        &self,
        _: &DataType,
        left: &crate::common::vector::Vector,
        right: &crate::common::vector::Vector,
        predicate: ComparisonPredicate,
        query: &QueryContext,
    ) -> Result<Option<bool>> {
        query.check()?;
        if !left.numeric_ascending() || !right.all_valid() {
            return Ok(None);
        }
        let Some(right) = right.constant_value() else {
            return Ok(None);
        };
        if left.is_empty() {
            return Ok(Some(false));
        }
        let (first, last) =
            if let (Some(values), Some(right)) = (left.flat_decimal_i64(), decimal_i64(right)) {
                (
                    values.first().expect("nonempty sorted column").cmp(&right),
                    values.last().expect("nonempty sorted column").cmp(&right),
                )
            } else {
                (
                    physical_compare(&left.get(0).expect("nonempty sorted column"), right),
                    physical_compare(
                        &left.get(left.len() - 1).expect("nonempty sorted column"),
                        right,
                    ),
                )
            };
        Ok((first == last).then(|| predicate.matches(first)))
    }
    fn key_representation(&self, _: &DataType) -> KeyRepresentation {
        KeyRepresentation::NumericCoefficient
    }
    fn name(&self) -> &'static str {
        "exact-numeric-types"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        super::check_metadata(data_type)?;
        if !data_type.is_decimal() && !data_type.is_unsigned_integer() {
            return Err(Error::Bind(
                "exact numeric adapter requires decimal or unsigned metadata".into(),
            ));
        }
        Ok(())
    }
    fn validate_value(&self, _: &DataType, _: &Value, query: &QueryContext) -> Result<()> {
        query.check()
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        Ok(DataType::common(left, right).ok())
    }
    fn common_type_with_integer_literals(
        &self,
        left: &DataType,
        right: &DataType,
        left_literal: Option<i128>,
        right_literal: Option<i128>,
        types: &TypeRegistry,
    ) -> Result<Option<DataType>> {
        self.common_type_with_literals(
            left,
            right,
            left_literal.map(IntegerLiteral::Signed),
            right_literal.map(IntegerLiteral::Signed),
            types,
        )
    }
    fn common_type_with_literals(
        &self,
        left: &DataType,
        right: &DataType,
        left_literal: Option<IntegerLiteral>,
        right_literal: Option<IntegerLiteral>,
        types: &TypeRegistry,
    ) -> Result<Option<DataType>> {
        if let Some(target) =
            super::integer_literal_target(left, right, left_literal, right_literal)
        {
            return Ok(Some(target));
        }
        self.common_type_with_registry(left, right, types)
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        query.check()?;
        left.compare(right)
    }
    fn compare_batch(
        &self,
        _: &DataType,
        left: &crate::common::vector::Vector,
        right: &crate::common::vector::Vector,
        query: &QueryContext,
    ) -> Result<Vec<Option<Ordering>>> {
        // BoundType has validated identical logical metadata, including decimal
        // scale. Compare coefficients directly and check cancellation per block.
        super::batch::compare_values(left, right, query, |a, b| match (a, b) {
            (Value::Unsigned(a), Value::Unsigned(b)) => Ok(a.cmp(b)),
            (Value::Decimal { value: a, .. }, Value::Decimal { value: b, .. }) => Ok(a.cmp(b)),
            _ => Err(Error::Internal("numeric comparison input".into())),
        })
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        value.append_primitive_key(output)
    }
    fn select_comparison(
        &self,
        _: &DataType,
        left: &crate::common::vector::Vector,
        right: &crate::common::vector::Vector,
        predicate: ComparisonPredicate,
        query: &QueryContext,
    ) -> Result<Vec<usize>> {
        if right.all_valid()
            && let (Some(values), Some(value)) = (
                left.flat_decimal_i64(),
                right.constant_value().and_then(decimal_i64),
            )
        {
            query.check()?;
            if left.numeric_ascending() {
                return select_sorted_decimal_i64(values, value, predicate, query);
            }
            let mut selected = Vec::new();
            for (index, left) in values.iter().enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                if predicate.matches(left.cmp(&value)) {
                    selected.push(index);
                }
            }
            query.check()?;
            return Ok(selected);
        }
        if left.numeric_ascending()
            && right.all_valid()
            && let (Some(values), Some(value)) = (left.flat_values(), right.constant_value())
        {
            query.check()?;
            if let Some((first, last)) = values.first().zip(values.last()) {
                let first = physical_compare(first, value);
                if first == physical_compare(last, value) {
                    let mut selected = Vec::with_capacity(if predicate.matches(first) {
                        values.len()
                    } else {
                        0
                    });
                    if predicate.matches(first) {
                        for start in (0..values.len()).step_by(1024) {
                            query.check()?;
                            selected.extend(start..(start + 1024).min(values.len()));
                        }
                    }
                    return Ok(selected);
                }
            }
            let lower = values.partition_point(|a| physical_compare(a, value).is_lt());
            let upper =
                values[lower..].partition_point(|a| physical_compare(a, value).is_eq()) + lower;
            let mut selected = Vec::new();
            for (include, range) in [
                (predicate.less, 0..lower),
                (predicate.equal, lower..upper),
                (predicate.greater, upper..values.len()),
            ] {
                if include {
                    for start in range.clone().step_by(1024) {
                        query.check()?;
                        selected.extend(start..(start.saturating_add(1024)).min(range.end));
                    }
                }
            }
            query.check()?;
            Ok(selected)
        } else {
            super::batch::select_values(left, right, predicate, query, |a, b| {
                Ok(physical_compare(a, b))
            })
        }
    }
}

fn decimal_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Decimal { value, .. } => i64::try_from(*value).ok(),
        _ => None,
    }
}

fn select_sorted_decimal_i64(
    values: &[i64],
    value: i64,
    predicate: ComparisonPredicate,
    query: &QueryContext,
) -> Result<Vec<usize>> {
    if let Some((first, last)) = values.first().zip(values.last()) {
        let first = first.cmp(&value);
        if first == last.cmp(&value) {
            let mut selected = Vec::with_capacity(if predicate.matches(first) {
                values.len()
            } else {
                0
            });
            if predicate.matches(first) {
                for start in (0..values.len()).step_by(1024) {
                    query.check()?;
                    selected.extend(start..(start + 1024).min(values.len()));
                }
            }
            return Ok(selected);
        }
    }
    let lower = values.partition_point(|left| *left < value);
    let upper = values[lower..].partition_point(|left| *left == value) + lower;
    let mut selected = Vec::new();
    for (include, range) in [
        (predicate.less, 0..lower),
        (predicate.equal, lower..upper),
        (predicate.greater, upper..values.len()),
    ] {
        if include {
            for start in range.clone().step_by(1024) {
                query.check()?;
                selected.extend(start..(start.saturating_add(1024)).min(range.end));
            }
        }
    }
    query.check()?;
    Ok(selected)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn physical_compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Unsigned(a), Value::Unsigned(b)) => a.cmp(b),
        (Value::Decimal { value: a, .. }, Value::Decimal { value: b, .. }) => a.cmp(b),
        _ => unreachable!("validated same-type non-NULL numeric comparison"),
    }
}

/// Independent decimal-digit ordering and textual equality keys. Useful for
/// checking interchange and comparison without native-width integer kernels.
#[derive(Debug)]
pub struct LexicalNumericTypes;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for LexicalNumericTypes {
    fn name(&self) -> &'static str {
        "lexical-numeric-types"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        ExactNumericTypes.validate_type(data_type)
    }
    fn validate_value(&self, _: &DataType, _: &Value, query: &QueryContext) -> Result<()> {
        query.check()
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        Ok(DataType::common(left, right).ok())
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        query.check()?;
        let a = coefficient_text(left)?;
        let b = coefficient_text(right)?;
        let an = a.starts_with('-');
        let bn = b.starts_with('-');
        if an != bn {
            return Ok(bn.cmp(&an));
        }
        let a = a.trim_start_matches('-');
        let b = b.trim_start_matches('-');
        let order = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
        Ok(if an { order.reverse() } else { order })
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        output.extend_from_slice(coefficient_text(value)?.as_bytes())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn coefficient_text(value: &Value) -> Result<String> {
    match value {
        Value::Unsigned(n) => Ok(n.to_string()),
        Value::Decimal { value, .. } => Ok(value.to_string()),
        _ => Err(Error::Internal("numeric comparison input".into())),
    }
}
