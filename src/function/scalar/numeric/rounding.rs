//! Precision-aware numeric rounding. Logical overloads retain selected casts;
//! decimal result metadata is determined by a selected typed constant request.
use std::sync::Arc;

use crate::{
    common::{
        DataType, Error, Result, Value,
        cast::CastMode,
        numeric::{DECIMAL_POWERS, decimal},
        type_registry::TypeRegistry,
    },
    function::{ArgumentEvaluation, FunctionRegistry, ScalarBindArguments, ScalarFunction},
    parallel::QueryContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    Truncate,
    Away,
    Even,
}

#[derive(Debug, Clone)]
struct Signature {
    arguments: Vec<DataType>,
    result: DataType,
    decimal_precision: Option<i32>,
    decimal_zero: bool,
    known_null: bool,
}

#[derive(Debug)]
struct Rounding {
    name: &'static str,
    policy: Policy,
    signature: Option<Signature>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, policy) in [
        ("round", Policy::Away),
        ("trunc", Policy::Truncate),
        ("round_even", Policy::Even),
        ("roundbankers", Policy::Even),
    ] {
        registry
            .register_scalar(Arc::new(Rounding {
                name,
                policy,
                signature: None,
            }))
            .expect("unique rounding scalar function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Rounding {
    fn targets(&self, arguments: &[DataType]) -> Result<Vec<DataType>> {
        if !(arguments.len() == 2 || (arguments.len() == 1 && self.policy != Policy::Even)) {
            return Err(Error::Bind(format!(
                "invalid argument count for {}",
                self.name
            )));
        }
        let target = match &arguments[0] {
            DataType::Null => DataType::BigInt,
            DataType::Bignum => DataType::Double,
            input if self.policy != Policy::Truncate && input.is_unsigned_integer() => {
                match input {
                    DataType::UBigInt => DataType::HugeInt,
                    DataType::UHugeInt => DataType::Double,
                    _ => DataType::BigInt,
                }
            }
            input if input.is_numeric() => input.clone(),
            input => {
                return Err(Error::Bind(format!(
                    "no overload for {}({input})",
                    self.name
                )));
            }
        };
        let mut targets = vec![target];
        if arguments.len() == 2 {
            targets.push(DataType::Integer);
        }
        Ok(targets)
    }

    fn make_signature(
        &self,
        mut arguments: Vec<DataType>,
        precision: Option<i32>,
    ) -> Result<Signature> {
        let mut result = arguments[0].clone();
        let mut decimal_zero = false;
        if let DataType::Decimal { width, scale } = arguments[0] {
            let precision = precision.ok_or_else(|| {
                Error::Bind("decimal rounding requires constant precision".into())
            })?;
            let target_scale = precision.clamp(0, i32::from(scale)) as u8;
            let carries = self.policy != Policy::Truncate
                && scale == 0
                && precision < 0
                && precision >= -i32::from(width);
            let result_width = if carries { (width + 1).min(38) } else { width };
            result = DataType::Decimal {
                width: result_width,
                scale: target_scale,
            };
            // The native implementation widens the input only when carrying
            // crosses a physical coefficient-width transition. Keep that cast
            // visible to the selected cast registry rather than recasting here.
            if decimal_storage_width(width) != decimal_storage_width(result_width) {
                arguments[0] = DataType::Decimal {
                    width: result_width,
                    scale,
                };
            }
            decimal_zero = if self.policy == Policy::Truncate {
                precision < 0 && precision <= -i32::from(width - scale)
            } else {
                precision < -i32::from(width - scale)
            };
        }
        Ok(Signature {
            arguments,
            result,
            decimal_precision: precision,
            decimal_zero,
            known_null: false,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Rounding {
    fn name(&self) -> &str {
        self.name
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let source = (0..arguments.len())
            .map(|i| arguments.data_type(i))
            .collect::<Result<Vec<_>>>()?;
        let targets = self.targets(&source)?;
        if targets[0].is_decimal()
            && arguments
                .constant_if_closed(0)?
                .is_some_and(|value| value.is_null())
        {
            return Ok(Some(Arc::new(Self {
                name: self.name,
                policy: self.policy,
                signature: Some(Signature {
                    arguments: targets,
                    result: DataType::Null,
                    decimal_precision: None,
                    decimal_zero: false,
                    known_null: true,
                }),
            })));
        }
        let precision = if targets[0].is_decimal() {
            if arguments.len() == 1 {
                Some(0)
            } else {
                match arguments.constant_as(1, &DataType::Integer, CastMode::Implicit)? {
                    Value::Integer(value) => {
                        Some(i32::try_from(value).map_err(|_| {
                            Error::Internal("rounding constant is not INTEGER".into())
                        })?)
                    }
                    Value::Null => {
                        return Ok(Some(Arc::new(Self {
                            name: self.name,
                            policy: self.policy,
                            signature: Some(Signature {
                                arguments: targets,
                                result: DataType::Null,
                                decimal_precision: None,
                                decimal_zero: false,
                                known_null: true,
                            }),
                        })));
                    }
                    _ => return Err(Error::Internal("rounding constant is not INTEGER".into())),
                }
            }
        } else {
            None
        };
        Ok(Some(Arc::new(Self {
            name: self.name,
            policy: self.policy,
            signature: Some(self.make_signature(targets, precision)?),
        })))
    }

    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self
            .signature
            .as_ref()
            .is_some_and(|signature| signature.known_null)
        {
            ArgumentEvaluation::TypeOnly
        } else {
            ArgumentEvaluation::Eager
        }
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if let Some(signature) = &self.signature {
            if arguments.len() != signature.arguments.len() {
                return Err(Error::Bind("rounding argument count changed".into()));
            }
            Ok(signature.arguments.clone())
        } else {
            self.targets(arguments)
        }
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if let Some(signature) = &self.signature {
            if arguments != signature.arguments {
                return Err(Error::Bind("rounding argument metadata changed".into()));
            }
            Ok(signature.result.clone())
        } else {
            self.make_signature(
                self.targets(arguments)?,
                (arguments.len() == 1).then_some(0),
            )
            .map(|s| s.result)
        }
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let signature = self
            .signature
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound rounding function".into()))?;
        if signature.known_null {
            if !arguments.is_empty() {
                return Err(Error::Internal(
                    "constant NULL rounding evaluated arguments".into(),
                ));
            }
            return Ok(Value::Null);
        }
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Internal("rounding argument count changed".into()));
        }
        for (value, ty) in arguments.iter().zip(&signature.arguments) {
            if !value.fits_type(ty) {
                return Err(Error::Internal("rounding input was not coerced".into()));
            }
        }
        let precision = if arguments.len() == 1 {
            0
        } else {
            match arguments[1] {
                Value::Null => return Ok(Value::Null),
                Value::Integer(value) => i32::try_from(value)
                    .map_err(|_| Error::Internal("rounding precision is not INTEGER".into()))?,
                _ => return Err(Error::Internal("rounding precision is not INTEGER".into())),
            }
        };
        if let Some(retained) = signature.decimal_precision {
            if retained != precision {
                return Err(Error::Internal(
                    "rounding constant changed after binding".into(),
                ));
            }
            if signature.decimal_zero {
                let DataType::Decimal { width, scale } = signature.result else {
                    unreachable!()
                };
                return decimal(0, width, scale);
            }
        }
        if arguments[0].is_null() {
            return Ok(Value::Null);
        }
        let value = match &arguments[0] {
            Value::Integer(value) => {
                let result = integer(*value, precision, self.policy, &signature.arguments[0])?;
                let result = Value::Integer(result);
                if !result.fits_type(&signature.result) {
                    return Err(overflow(self.policy));
                }
                result
            }
            Value::Unsigned(value) if self.policy == Policy::Truncate => {
                let result = if precision >= 0 {
                    *value
                } else {
                    trunc_power(precision, &signature.arguments[0])
                        .map_or(0, |power| value / power * power)
                };
                Value::Unsigned(result)
            }
            Value::Float(value) => Value::Float(floating(
                f64::from(*value),
                precision,
                self.policy,
                true,
                arguments.len() == 1,
            ) as f32),
            Value::Double(value) => Value::Double(floating(
                *value,
                precision,
                self.policy,
                false,
                arguments.len() == 1,
            )),
            Value::Decimal { value, scale, .. } => {
                let DataType::Decimal {
                    width,
                    scale: target_scale,
                } = signature.result
                else {
                    return Err(Error::Internal("rounding decimal output metadata".into()));
                };
                let drop = i32::from(*scale) - precision.min(i32::from(*scale));
                let result = if drop == 0 {
                    *value
                } else {
                    let power = DECIMAL_POWERS
                        .get(drop as usize)
                        .ok_or_else(|| Error::Internal("rounding decimal power".into()))?;
                    let divided = divide(*value, *power as i128, self.policy);
                    if precision < 0 {
                        divided
                            .checked_mul(DECIMAL_POWERS[precision.unsigned_abs() as usize] as i128)
                            .ok_or_else(|| overflow(self.policy))?
                    } else {
                        divided
                    }
                };
                if result.unsigned_abs() >= DECIMAL_POWERS[usize::from(width)] {
                    return Err(overflow(self.policy));
                }
                decimal(result, width, target_scale)?
            }
            _ => return Err(Error::Internal("rounding input was not coerced".into())),
        };
        Ok(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decimal_storage_width(width: u8) -> u8 {
    match width {
        0..=4 => 16,
        5..=9 => 32,
        10..=18 => 64,
        _ => 128,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn overflow(policy: Policy) -> Error {
    Error::OutOfRange(format!(
        "Overflow in {} of numeric value",
        if policy == Policy::Even {
            "ROUND_EVEN"
        } else {
            "ROUND"
        }
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn divide(value: i128, power: i128, policy: Policy) -> i128 {
    let quotient = value / power;
    let remainder = (value % power).unsigned_abs();
    let half = (power / 2) as u128;
    quotient
        + if policy != Policy::Truncate
            && (remainder > half
                || (remainder == half && (policy == Policy::Away || quotient % 2 != 0)))
        {
            value.signum()
        } else {
            0
        }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn trunc_power(precision: i32, ty: &DataType) -> Option<u128> {
    // Development's smaller integer overloads deliberately use NumericHelper's
    // 19 cached powers, even UBIGINT, whose domain extends above 10^19.
    let count = if matches!(ty, DataType::HugeInt | DataType::UHugeInt) {
        39
    } else {
        19
    };
    (precision.unsigned_abs() < count).then(|| DECIMAL_POWERS[precision.unsigned_abs() as usize])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn integer(value: i128, precision: i32, policy: Policy, ty: &DataType) -> Result<i128> {
    if precision >= 0 {
        return Ok(value);
    }
    let power = if policy == Policy::Truncate {
        trunc_power(precision, ty)
    } else {
        DECIMAL_POWERS
            .get(precision.unsigned_abs() as usize)
            .copied()
    };
    let Some(power) = power else { return Ok(0) };
    divide(value, power as i128, policy)
        .checked_mul(power as i128)
        .ok_or_else(|| overflow(policy))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn floating(value: f64, precision: i32, policy: Policy, single: bool, unary: bool) -> f64 {
    let nearest = |value: f64| match policy {
        Policy::Truncate => value.trunc(),
        Policy::Away => value.round(),
        Policy::Even => value.round_ties_even(),
    };
    if unary {
        return nearest(value);
    }
    // FLOAT precision is converted to FLOAT before pow's DOUBLE arithmetic.
    let exponent = if single {
        f64::from(precision as f32)
    } else {
        f64::from(precision)
    };
    let modifier = 10_f64.powf(exponent.abs());
    let rounded = if precision < 0 {
        nearest(value / modifier) * modifier
    } else {
        nearest(value * modifier) / modifier
    };
    if rounded.is_finite() {
        rounded
    } else if policy != Policy::Truncate && precision < 0 {
        0.0
    } else {
        value
    }
}
