use std::sync::Arc;

use super::{AggregateFunction, AggregateState, FunctionRegistry};
use crate::common::{DataType, Error, Result, Value};

mod exact;
mod groups;
mod window;
use exact::SumKernel;

#[derive(Debug)]
struct Builtin(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "count", "sum", "avg", "product", "min", "max", "first", "last", "bool_and", "bool_or",
    ] {
        registry
            .register_aggregate(Arc::new(Builtin(name)))
            .expect("unique builtin name");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for Builtin {
    fn name(&self) -> &str {
        self.0
    }
    fn return_type(
        &self,
        args: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if self.0 == "count" && args.len() <= 1 {
            return Ok(DataType::BigInt);
        }
        if args.len() != 1 {
            return Err(Error::Bind(format!("{} requires one argument", self.0)));
        }
        match self.0 {
            "sum" | "avg" if args[0].is_numeric() || args[0] == DataType::Null => {
                Ok(if self.0 == "avg" || args[0].is_floating() {
                    DataType::Double
                } else if args[0] == DataType::Bignum {
                    DataType::Bignum
                } else if let DataType::Decimal { scale, .. } = args[0] {
                    DataType::Decimal { width: 38, scale }
                } else if args[0] == DataType::UHugeInt {
                    DataType::Double
                } else {
                    DataType::HugeInt
                })
            }
            "product" if args[0].is_numeric() || args[0] == DataType::Null => Ok(DataType::Double),
            "min" | "max" | "first" | "last" => Ok(args[0].clone()),
            "bool_and" | "bool_or" if matches!(args[0], DataType::Boolean | DataType::Null) => {
                Ok(DataType::Boolean)
            }
            _ => Err(Error::Bind(format!("no overload for {}({args:?})", self.0))),
        }
    }
    fn create_state(
        &self,
        args: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        if self.0 == "sum" && args == [DataType::Bignum] {
            return Ok(Box::new(super::bignum::BignumSum::default()));
        }
        Ok(Box::new(State {
            name: self.0,
            data_type: self.return_type(args, types)?,
            count: 0,
            value: Value::Null,
            seen: false,
        }))
    }
    fn batch_update_is_total(&self, args: &[DataType]) -> bool {
        self.0 == "count"
            || (self.0 == "sum"
                && args
                    .first()
                    .and_then(SumKernel::bind)
                    .is_some_and(|kernel| kernel.supports_count(usize::MAX)))
    }
    fn evaluate_window(
        &self,
        input: &super::window::WindowInput<'_>,
        query: &crate::parallel::QueryContext,
    ) -> Result<Option<Vec<Value>>> {
        window::evaluate(self, input, query)
    }
    fn create_grouped_state(
        &self,
        args: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Option<Box<dyn super::grouped::GroupedAggregateState>>> {
        self.return_type(args, types)?;
        Ok(groups::create(self.0, args))
    }
}

struct State {
    name: &'static str,
    data_type: DataType,
    count: i128,
    value: Value,
    seen: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateState for State {
    fn update_batch(
        &mut self,
        arguments: &crate::common::vector::DataChunk,
        context: &crate::parallel::QueryContext,
    ) -> Result<()> {
        if !matches!(self.name, "count" | "sum" | "product") || arguments.columns().len() > 1 {
            return super::update_aggregate_rows(self, arguments, context);
        }
        let Some(column) = arguments.columns().first() else {
            if self.name != "count" {
                return super::update_aggregate_rows(self, arguments, context);
            }
            // This is the only total zero-argument fast path, so it cannot
            // rely on a column kernel for its cancellation boundary.
            context.check()?;
            self.count = self
                .count
                .checked_add(arguments.len() as i128)
                .ok_or_else(|| Error::Execution("aggregate count overflow".into()))?;
            return Ok(());
        };
        self.update_column(column, context)
    }
    fn update_column(
        &mut self,
        column: &crate::common::vector::Vector,
        context: &crate::parallel::QueryContext,
    ) -> Result<()> {
        // SUM's column kernels establish their own bounded cancellation
        // boundary (before each 1024-row reduction and before return). Avoid
        // checking twice before the first dense block; the other aggregate
        // paths still need this entry check for their early-return fast paths.
        if self.name != "sum" {
            context.check()?;
        }
        if self.name == "count" && column.all_valid() {
            self.count = self
                .count
                .checked_add(column.len() as i128)
                .ok_or_else(|| Error::Execution("aggregate count overflow".into()))?;
            return Ok(());
        }
        if self.name == "sum" && self.sum_repeated(column, context)? {
            return Ok(());
        }
        if self.name == "sum" && self.sum_selected_bigints(column, context)? {
            return Ok(());
        }
        if self.name == "sum" && self.sum_dense(column, context)? {
            return Ok(());
        }
        if self.name == "product" && self.product_dense(column, context)? {
            return Ok(());
        }
        if self.name == "count"
            || (self.name == "sum"
                && self.data_type == DataType::HugeInt
                && column.data_type().is_signed_integer())
        {
            return self.update_integer_column(column, context);
        }
        for value in column.values() {
            context.check()?;
            self.update(std::slice::from_ref(&value), context)?;
        }
        context.check()
    }
    fn update(&mut self, args: &[Value], context: &crate::parallel::QueryContext) -> Result<()> {
        let value = args.first().cloned().unwrap_or(Value::Integer(1));
        if self.name == "first" {
            if !self.seen {
                self.value = value;
                self.seen = true;
            }
            return Ok(());
        }
        if self.name == "last" {
            self.value = value;
            return Ok(());
        }
        if value.is_null() {
            return Ok(());
        }
        if self.name != "sum" {
            self.count = self
                .count
                .checked_add(1)
                .ok_or_else(|| Error::Execution("aggregate count overflow".into()))?;
        }
        match self.name {
            "count" => {}
            "sum" | "avg" => {
                let value = if self.data_type == DataType::Double {
                    Value::Double(match &value {
                        Value::Bignum(value) => value.to_f64(|| context.check())?,
                        _ => value.as_f64()?,
                    })
                } else if let DataType::Decimal { width, scale } = self.data_type {
                    let Value::Decimal { value, .. } = value else {
                        return Err(Error::Internal("decimal aggregate argument".into()));
                    };
                    Value::Decimal {
                        value,
                        width,
                        scale,
                    }
                } else {
                    Value::Integer(value.as_i128()?)
                };
                self.value = match (&self.value, value) {
                    (Value::Null, v) => v,
                    (Value::Integer(a), Value::Integer(b)) => Value::Integer(
                        a.checked_add(b)
                            .ok_or_else(|| Error::Execution("sum overflow".into()))?,
                    ),
                    (Value::Double(a), Value::Double(b)) => Value::Double(a + b),
                    (
                        Value::Decimal {
                            value: a,
                            width,
                            scale,
                        },
                        Value::Decimal { value: b, .. },
                    ) => {
                        // SUM accumulates in the physical HUGEINT domain.
                        let value = a
                            .checked_add(b)
                            .ok_or_else(|| Error::Execution("sum overflow".into()))?;
                        crate::common::numeric::decimal(value, *width, *scale)
                            .map_err(|_| Error::Execution("sum overflow".into()))?
                    }
                    _ => return Err(Error::Internal("aggregate type mismatch".into())),
                };
            }
            "product" => {
                let right = match &value {
                    Value::Bignum(value) => value.to_f64(|| context.check())?,
                    _ => value.as_f64()?,
                };
                self.value = Value::Double(match self.value {
                    Value::Null => right,
                    Value::Double(left) => left * right,
                    _ => return Err(Error::Internal("product state differs from binding".into())),
                });
            }
            "min" | "max" => {
                // Pinned development's numeric min/max reduction chooses the
                // right representation on a tie. INTERVAL makes this visible:
                // one month and thirty days compare equal but format differently.
                let interval_tie = self.data_type == DataType::Interval
                    && !self.value.is_null()
                    && context
                        .types()
                        .bind(&self.data_type)?
                        .compare(&value, &self.value, context)?
                        .is_eq();
                if self.value.is_null()
                    || interval_tie
                    || (self.name == "min"
                        && context
                            .types()
                            .bind(&self.data_type)?
                            .compare(&value, &self.value, context)?
                            .is_lt())
                    || (self.name == "max"
                        && context
                            .types()
                            .bind(&self.data_type)?
                            .compare(&value, &self.value, context)?
                            .is_gt())
                {
                    self.value = value;
                }
            }
            "bool_and" | "bool_or" => {
                let right = value.as_bool()?.unwrap_or(false);
                self.value = Value::Boolean(match self.value.as_bool()? {
                    None => right,
                    Some(left) if self.name == "bool_and" => left && right,
                    Some(left) => left || right,
                });
            }
            _ => return Err(Error::Internal("unknown aggregate".into())),
        }
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        match self.name {
            "count" => Ok(Value::Integer(self.count)),
            "avg" if self.count > 0 => Ok(Value::Double(self.value.as_f64()? / self.count as f64)),
            _ => Ok(self.value),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State {
    /// Dictionary BIGINT selections can retain their parent physical lane.
    /// Declining any unproved shape preserves the scalar update/error order.
    fn sum_selected_bigints(
        &mut self,
        column: &crate::common::vector::Vector,
        context: &crate::parallel::QueryContext,
    ) -> Result<bool> {
        let Some(kernel @ SumKernel::Signed(64)) = SumKernel::bind(column.data_type()) else {
            return Ok(false);
        };
        if kernel.result_type() != self.data_type || !column.all_valid() {
            return Ok(false);
        }
        let Some((parent, selection)) = column.dictionary() else {
            return Ok(false);
        };
        if !parent.all_valid() {
            return Ok(false);
        }
        let Some(values) = parent.flat_bigints() else {
            return Ok(false);
        };
        let sum = match self.value {
            Value::Null => 0,
            Value::Integer(value) => value,
            _ => return Ok(false),
        };
        let Some(bound) = kernel
            .maximum_magnitude()
            .checked_mul(selection.len() as i128)
        else {
            return Ok(false);
        };
        if sum.checked_add(bound).is_none() || sum.checked_sub(bound).is_none() {
            return Ok(false);
        }
        let Some(contribution) = kernel.selected_bigint_sum(values, selection, context)? else {
            return Ok(false);
        };
        self.value = Value::Integer(
            sum.checked_add(contribution)
                .ok_or_else(|| Error::Execution("sum overflow".into()))?,
        );
        Ok(true)
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    /// A constant column, or a dictionary whose physical entries all carry
    /// one coefficient, has a closed-form exact SUM. Restrict this to domains
    /// whose entire process-sized input population fits the accumulator, so
    /// replacing row-order additions cannot change overflow timing.
    fn sum_repeated(
        &mut self,
        column: &crate::common::vector::Vector,
        context: &crate::parallel::QueryContext,
    ) -> Result<bool> {
        let Some(kernel) = SumKernel::bind(column.data_type())
            .filter(|kernel| kernel.result_type() == self.data_type)
            .filter(|kernel| kernel.supports_count(usize::MAX))
        else {
            return Ok(false);
        };
        let repeated = if let Some(value) = column.constant_value() {
            kernel.coefficient(value)
        } else if column.all_valid()
            && let Some((parent, _)) = column.dictionary()
        {
            let mut values = parent.values();
            let first = values.next().and_then(|value| kernel.coefficient(&value));
            first.filter(|first| {
                values.all(|value| kernel.coefficient(&value).as_ref() == Some(first))
            })
        } else {
            None
        };
        let Some(repeated) = repeated else {
            return Ok(false);
        };
        let contribution = repeated
            .checked_mul(column.len() as i128)
            .ok_or_else(|| Error::Execution("sum overflow".into()))?;
        let sum = match self.value {
            Value::Null => contribution,
            Value::Integer(value) => value
                .checked_add(contribution)
                .ok_or_else(|| Error::Execution("sum overflow".into()))?,
            Value::Decimal { value, .. } => value
                .checked_add(contribution)
                .filter(|value| kernel.valid_sum(*value))
                .ok_or_else(|| Error::Execution("sum overflow".into()))?,
            _ => return Ok(false),
        };
        if !column.is_empty() {
            self.value = kernel.value(sum);
        }
        context.check()?;
        Ok(true)
    }

    /// PRODUCT's selected DOUBLE overload can consume a validated dense column
    /// without rebuilding one-value argument rows or dispatching the aggregate
    /// callback for every input. Iteration remains in source order so IEEE
    /// overflow, signed zero and NaN behavior stay unchanged.
    fn product_dense(
        &mut self,
        column: &crate::common::vector::Vector,
        context: &crate::parallel::QueryContext,
    ) -> Result<bool> {
        if column.data_type() != &DataType::Double || !column.all_valid() {
            return Ok(false);
        }
        let mut product = match self.value {
            Value::Null => 1.0,
            Value::Double(value) => value,
            _ => return Err(Error::Internal("product state differs from binding".into())),
        };
        let count = column.len();
        if (product == -1.0 || product == 1.0)
            && let Some((parent, selection)) = column.dictionary()
        {
            let signs = parent
                .values()
                .map(|value| match value {
                    Value::Double(1.0) => Some(false),
                    Value::Double(-1.0) => Some(true),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            if let Some(signs) = signs {
                let mut negative = false;
                for (index, &source) in selection.iter().enumerate() {
                    if index % 1024 == 0 {
                        context.check()?;
                    }
                    negative ^= signs[source];
                }
                if negative {
                    product = -product;
                }
                self.count = self
                    .count
                    .checked_add(count as i128)
                    .ok_or_else(|| Error::Execution("aggregate count overflow".into()))?;
                if count != 0 {
                    self.value = Value::Double(product);
                }
                context.check()?;
                return Ok(true);
            }
        }
        if let Some(values) = column.flat_values() {
            for block in values.chunks(1024) {
                context.check()?;
                for value in block {
                    let Value::Double(value) = value else {
                        return Err(Error::Internal(
                            "product argument differs from binding".into(),
                        ));
                    };
                    product *= value;
                }
            }
        } else {
            for (index, value) in column.values().enumerate() {
                if index % 1024 == 0 {
                    context.check()?;
                }
                let Value::Double(value) = value else {
                    return Err(Error::Internal(
                        "product argument differs from binding".into(),
                    ));
                };
                product *= value;
            }
        }
        self.count = self
            .count
            .checked_add(count as i128)
            .ok_or_else(|| Error::Execution("aggregate count overflow".into()))?;
        if count != 0 {
            self.value = Value::Double(product);
        }
        context.check()?;
        Ok(true)
    }

    /// A narrow, non-NULL column admits a range proof for every accumulator
    /// prefix. The fallback preserves exact overflow timing near either bound,
    /// for HUGEINT input and for columns without the required physical views.
    fn sum_dense(
        &mut self,
        column: &crate::common::vector::Vector,
        context: &crate::parallel::QueryContext,
    ) -> Result<bool> {
        let Some(kernel) = SumKernel::bind(column.data_type()) else {
            return Ok(false);
        };
        if kernel.result_type() != self.data_type {
            return Ok(false);
        }
        if !column.all_valid() {
            return Ok(false);
        }
        let mut sum = match self.value {
            Value::Null => 0,
            Value::Integer(value) => value,
            Value::Decimal { value, .. } => value,
            _ => return Ok(false),
        };
        let Some(bound) = kernel.maximum_magnitude().checked_mul(column.len() as i128) else {
            return Ok(false);
        };
        if !sum.checked_add(bound).is_some_and(|v| kernel.valid_sum(v))
            || !sum.checked_sub(bound).is_some_and(|v| kernel.valid_sum(v))
        {
            return Ok(false);
        }
        if let Some(values) = column.flat_decimal_i64() {
            sum += kernel.column_sum_decimal_i64(values, context)?;
        } else if let Some(values) = column.flat_bigints() {
            sum += kernel.column_sum_signed_i64(values, context)?;
        } else if let Some(values) = column.flat_values() {
            sum += kernel.column_sum(values, context)?;
        } else {
            return Ok(false);
        }
        if !column.is_empty() {
            self.value = kernel.value(sum);
        }
        context.check()?;
        Ok(true)
    }
    fn update_integer_column(
        &mut self,
        column: &crate::common::vector::Vector,
        context: &crate::parallel::QueryContext,
    ) -> Result<()> {
        if let Some(values) = column.flat_values() {
            self.update_values(values.iter().cloned(), context)
        } else {
            self.update_values(column.values(), context)
        }
    }
    fn update_values(
        &mut self,
        values: impl Iterator<Item = Value>,
        context: &crate::parallel::QueryContext,
    ) -> Result<()> {
        if self.name == "count" {
            for (index, value) in values.enumerate() {
                if index % 1024 == 0 {
                    context.check()?;
                }
                if !value.is_null() {
                    self.count = self
                        .count
                        .checked_add(1)
                        .ok_or_else(|| Error::Execution("aggregate count overflow".into()))?;
                }
            }
        } else {
            let mut seen = !self.value.is_null();
            let mut sum = match self.value {
                Value::Null => 0,
                Value::Integer(value) => value,
                _ => {
                    return Err(Error::Internal(
                        "integer sum state differs from binding".into(),
                    ));
                }
            };
            for (index, value) in values.enumerate() {
                if index % 1024 == 0 {
                    context.check()?;
                }
                let value = match value {
                    Value::Null => continue,
                    Value::Integer(value) => value,
                    _ => {
                        return Err(Error::Internal(
                            "integer sum argument differs from binding".into(),
                        ));
                    }
                };
                sum = sum
                    .checked_add(value)
                    .ok_or_else(|| Error::Execution("sum overflow".into()))?;
                seen = true;
            }
            if seen {
                self.value = Value::Integer(sum);
            }
        }
        context.check()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Independent machine-width lanes avoid a carry dependency across every row.
/// Failure requests the wide kernel; it is not a SQL overflow. The caller must
/// separately prove that every logical prefix fits the SQL accumulator.
fn sum_narrow(values: &[Value], integer: impl Fn(&Value) -> Option<i64>) -> Option<i128> {
    let mut lanes = [0_i64; 4];
    // A machine-width carry is not a SQL overflow: the caller recomputes the
    // whole block in i128. Retain its fact and branch only once after the
    // independent lanes have finished, rather than making each coefficient
    // choose the fallback path.
    let mut overflowed = false;
    let mut blocks = values.chunks_exact(4);
    for block in &mut blocks {
        for (lane, value) in lanes.iter_mut().zip(block) {
            let (sum, overflow) = lane.overflowing_add(integer(value)?);
            *lane = sum;
            overflowed |= overflow;
        }
    }
    if overflowed {
        return None;
    }
    let mut sum: i128 = lanes.into_iter().map(i128::from).sum();
    for value in blocks.remainder() {
        sum += i128::from(integer(value)?);
    }
    Some(sum)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Caller proves the sum of all input magnitudes fits i64, so every lane does
/// too. Physical dispatch is monomorphic for this block, outside the hot loop.
fn sum_proven_narrow(values: &[Value], integer: impl Fn(&Value) -> Option<i64>) -> i128 {
    // The caller's column construction proves the physical kind. Keep a
    // checked load so an internal misuse still fails, without carrying extra
    // validity accumulators and conditional-zero coefficients through the hot
    // loop. Nothing is published until this entire private reduction returns.
    let mut lanes = [0_i64; 4];
    let mut blocks = values.chunks_exact(4);
    for block in &mut blocks {
        for (lane, value) in lanes.iter_mut().zip(block) {
            *lane += integer(value).expect("validated narrow SUM input");
        }
    }
    // The magnitude proof covers the lane reduction and tail as well as the
    // independent lanes. Widen only the completed block, not each lane.
    let mut sum: i64 = lanes.into_iter().sum();
    for value in blocks.remainder() {
        sum += integer(value).expect("validated narrow SUM input");
    }
    i128::from(sum)
}

#[cfg(test)]
mod reduction_tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn coefficient(value: &Value) -> Option<i64> {
        match value {
            Value::Decimal { value, .. } => Some(*value as i64),
            _ => None,
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn proven_reduction_lanes_and_tails_match_wide_signed_arithmetic() {
        for count in (0..34).chain([63, 64, 65, 1023, 1024, 1025]) {
            let maximum = i64::MAX / count.max(1) as i64;
            for sign in [-1_i64, 1] {
                let values = (0..count)
                    .map(|index| Value::Decimal {
                        value: i128::from(if index % 3 == 0 { -sign } else { sign })
                            * i128::from(maximum - index as i64),
                        width: 38,
                        scale: 2,
                    })
                    .collect::<Vec<_>>();
                // The helper's arithmetic precondition is the magnitude
                // bound, independent of a particular SQL declaration width.
                let expected: i128 = values
                    .iter()
                    .map(|v| i128::from(coefficient(v).unwrap()))
                    .sum();
                assert_eq!(sum_proven_narrow(&values, coefficient), expected);
            }
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn proven_reduction_never_discards_wrong_physical_kinds_in_any_lane_or_tail() {
        for count in 1..34 {
            for position in 0..count {
                let mut values = vec![
                    Value::Decimal {
                        value: 1,
                        width: 2,
                        scale: 0
                    };
                    count
                ];
                values[position] = Value::Null;
                assert!(
                    std::panic::catch_unwind(|| sum_proven_narrow(&values, coefficient)).is_err(),
                    "count={count}, position={position}"
                );
            }
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn checked_reduction_defers_machine_carries_to_the_wide_fallback() {
        let values = [
            Value::Integer(i128::from(i64::MAX)),
            Value::Integer(i128::from(i64::MAX)),
            Value::Integer(i128::from(i64::MAX)),
            Value::Integer(i128::from(i64::MAX)),
            Value::Integer(1),
            Value::Integer(1),
            Value::Integer(1),
            Value::Integer(1),
        ];
        assert!(
            sum_narrow(&values, |value| match value {
                Value::Integer(value) => Some(*value as i64),
                _ => None,
            })
            .is_none()
        );
    }
}
