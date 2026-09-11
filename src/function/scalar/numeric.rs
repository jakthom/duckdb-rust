//! Numeric scalar overloads. Input coercion belongs to the selected cast
//! registry, not to implicit numeric conversions inside these evaluators.
use std::sync::Arc;
mod absolute;
mod discrete;
mod ieee;
mod rounding;

use crate::{
    common::{
        DataType, Error, Result, Value, numeric::DECIMAL_POWERS, type_registry::TypeRegistry,
    },
    function::{FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
};

#[derive(Debug)]
struct IntegralDirection(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    absolute::register(registry);
    discrete::register(registry);
    ieee::register(registry);
    rounding::register(registry);
    for name in ["ceil", "ceiling", "floor", "sign"] {
        registry
            .register_scalar(Arc::new(IntegralDirection(name)))
            .expect("unique numeric scalar function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for IntegralDirection {
    fn name(&self) -> &str {
        self.0
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        let [input] = arguments else {
            return Err(Error::Bind(format!("{} requires one argument", self.0)));
        };
        let target = if self.0 == "sign" {
            match input {
                DataType::Null => DataType::BigInt,
                DataType::Decimal { .. } | DataType::Bignum => DataType::Double,
                input
                    if input.is_integer()
                        || matches!(input, DataType::Float | DataType::Double) =>
                {
                    input.clone()
                }
                _ => return Err(Error::Bind(format!("no overload for sign({input})"))),
            }
        } else {
            match input {
                DataType::Float | DataType::Double | DataType::Decimal { .. } => input.clone(),
                DataType::Null | DataType::Bignum => DataType::Double,
                input if input.is_integer() => DataType::Double,
                _ => return Err(Error::Bind(format!("no overload for {}({input})", self.0))),
            }
        };
        Ok(vec![target])
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        let [input] = arguments else {
            return Err(Error::Bind(format!("{} requires one argument", self.0)));
        };
        if self.0 == "sign"
            && (input.is_integer() || matches!(input, DataType::Float | DataType::Double))
        {
            return Ok(DataType::TinyInt);
        }
        if self.0 != "sign" {
            match input {
                DataType::Float | DataType::Double => return Ok(input.clone()),
                DataType::Decimal { width, .. } => {
                    return Ok(DataType::Decimal {
                        width: *width,
                        scale: 0,
                    });
                }
                _ => (),
            }
        }
        Err(Error::Bind(format!("no overload for {}({input})", self.0)))
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [input] = arguments else {
            return Err(Error::Internal("numeric function argument count".into()));
        };
        if input.is_null() {
            return Ok(Value::Null);
        }
        if self.0 == "sign" {
            let sign = match input {
                Value::Integer(value) => value.signum(),
                Value::Unsigned(value) => i128::from(*value != 0),
                // NaN, +0 and -0 all have sign zero in the reference.
                Value::Float(value) => i128::from(*value > 0.0) - i128::from(*value < 0.0),
                Value::Double(value) => i128::from(*value > 0.0) - i128::from(*value < 0.0),
                _ => return Err(Error::Internal("sign input was not coerced".into())),
            };
            return Ok(Value::Integer(sign));
        }
        let ceiling = self.0 != "floor";
        Ok(match input {
            Value::Float(value) => Value::Float(if ceiling { value.ceil() } else { value.floor() }),
            Value::Double(value) => {
                Value::Double(if ceiling { value.ceil() } else { value.floor() })
            }
            Value::Decimal {
                value,
                width,
                scale,
            } => {
                let power = DECIMAL_POWERS
                    .get(usize::from(*scale))
                    .copied()
                    .ok_or_else(|| Error::Internal("decimal direction input scale".into()))?
                    as i128;
                // Avoid adding an entire power to a possibly maximal
                // coefficient. Valid DECIMAL coefficients are strictly inside
                // +/-10^38, so these one-unit corrections cannot overflow.
                let rounded = if ceiling && *value > 0 {
                    (*value - 1) / power + 1
                } else if !ceiling && *value < 0 {
                    (*value + 1) / power - 1
                } else {
                    *value / power
                };
                crate::common::numeric::decimal(rounded, *width, 0)?
            }
            _ => {
                return Err(Error::Internal(
                    "rounding direction input was not coerced".into(),
                ));
            }
        })
    }
}
