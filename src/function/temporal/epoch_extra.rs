use std::sync::Arc;

use crate::{
    common::{DataType, Error, Result, TemporalValue, Value, temporal::MICROS_PER_DAY},
    function::{FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
};

const DAYS_PER_MONTH: i64 = 30;

#[derive(Debug)]
struct EpochExtraFunction(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["to_timestamp", "normalized_interval"] {
        registry
            .register_scalar(Arc::new(EpochExtraFunction(name)))
            .expect("unique extra epoch function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for EpochExtraFunction {
    fn name(&self) -> &str {
        self.0
    }

    fn argument_types(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if arguments.len() != 1 {
            return Ok(arguments.to_vec());
        }
        Ok(vec![match self.0 {
            "to_timestamp" => DataType::Double,
            "normalized_interval" => DataType::Interval,
            _ => return Err(Error::Internal("unregistered extra epoch function".into())),
        }])
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        match (self.0, arguments) {
            ("to_timestamp", [DataType::Double]) => Ok(DataType::TimestampTz),
            ("normalized_interval", [DataType::Interval]) => Ok(DataType::Interval),
            _ => Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.0
            ))),
        }
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let value = arguments
            .first()
            .ok_or_else(|| Error::Internal("extra epoch argument missing".into()))?;
        if value.is_null() {
            return Ok(Value::Null);
        }
        match (self.0, value) {
            ("to_timestamp", Value::Double(seconds)) => to_timestamp(*seconds),
            (
                "normalized_interval",
                Value::Temporal(TemporalValue::Interval {
                    months,
                    days,
                    micros,
                }),
            ) => Ok(Value::Temporal(normalized_interval(
                *months, *days, *micros,
            ))),
            _ => Err(Error::Internal(format!(
                "{} received an unbound argument",
                self.0
            ))),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn to_timestamp(seconds: f64) -> Result<Value> {
    let micros = seconds * 1_000_000.0;
    // DuckDB's numeric TryCast rejects the half-open binary64 range first,
    // then applies the current default round-to-nearest, ties-to-even mode.
    if !micros.is_finite() || micros < i64::MIN as f64 || micros >= -(i64::MIN as f64) {
        return Err(Error::Conversion(
            "Epoch seconds out of range for TIMESTAMP WITH TIME ZONE".into(),
        ));
    }
    let micros = micros.round_ties_even() as i64;
    Ok(Value::Temporal(TemporalValue::TimestampTz(micros)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn normalized_interval(months: i32, days: i32, micros: i64) -> TemporalValue {
    // Development DuckDB uses Euclidean carries so the two lower fields are
    // non-negative before it borrows overflow back into their right neighbor.
    let carry_days = micros.div_euclid(MICROS_PER_DAY);
    let mut normalized_micros = micros.rem_euclid(MICROS_PER_DAY);
    let days = i64::from(days) + carry_days;
    let carry_months = days.div_euclid(DAYS_PER_MONTH);
    let mut normalized_days = days.rem_euclid(DAYS_PER_MONTH);
    let months = i64::from(months) + carry_months;

    let normalized_months = borrow(
        months,
        &mut normalized_days,
        DAYS_PER_MONTH,
        i32::MIN,
        i32::MAX,
    );
    let normalized_days = borrow(
        normalized_days,
        &mut normalized_micros,
        MICROS_PER_DAY,
        i32::MIN,
        i32::MAX,
    );
    TemporalValue::Interval {
        months: normalized_months,
        days: normalized_days,
        micros: normalized_micros,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn borrow(msf: i64, lsf: &mut i64, scale: i64, minimum: i32, maximum: i32) -> i32 {
    if let Ok(value) = i32::try_from(msf) {
        return value;
    }
    let value = if msf > i64::from(maximum) {
        maximum
    } else {
        minimum
    };
    let remainder = msf - i64::from(value);
    // Keep development's explicit saturation guard. `lsf` is in [0, scale),
    // so every non-saturated addition below is representable.
    let max_units = i64::MAX / scale - 1;
    if remainder > max_units {
        *lsf = i64::MAX;
    } else if remainder < -max_units {
        *lsf = i64::MIN;
    } else {
        *lsf += remainder * scale;
    }
    value
}
