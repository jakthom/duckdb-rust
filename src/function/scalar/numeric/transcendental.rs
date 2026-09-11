//! Selected floating math overloads shared by both pinned core builds.
//!
//! The development pin makes historically fallible trigonometric domains
//! conditional on `ieee_floating_point_ops`; binding retains that statement
//! setting just like the existing logarithm/power family.

use std::{f64::consts::PI, sync::Arc};

use crate::{
    common::{DataType, Error, Result, Value, type_registry::TypeRegistry},
    function::{
        ArgumentEvaluation, FunctionRegistry, ScalarBindArguments, ScalarFunction, ScalarSignature,
    },
    parallel::QueryContext,
};

#[derive(Clone, Copy, Debug)]
enum Operation {
    Acos,
    Asin,
    Atan,
    Atan2,
    Cbrt,
    Cos,
    Cot,
    Sin,
    Tan,
    Cosh,
    Sinh,
    Tanh,
    Acosh,
    Asinh,
    Atanh,
    Degrees,
    Radians,
    Exp,
    Pi,
    SignBit,
    Even,
    NextAfter,
}

#[derive(Clone, Debug)]
struct Binding {
    signature: ScalarSignature,
    ieee: bool,
    known_null: bool,
}

#[derive(Debug)]
struct FloatingMath {
    name: &'static str,
    operation: Operation,
    binding: Option<Binding>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, operation) in [
        ("acos", Operation::Acos),
        ("asin", Operation::Asin),
        ("atan", Operation::Atan),
        ("atan2", Operation::Atan2),
        ("cbrt", Operation::Cbrt),
        ("cos", Operation::Cos),
        ("cot", Operation::Cot),
        ("sin", Operation::Sin),
        ("tan", Operation::Tan),
        ("cosh", Operation::Cosh),
        ("sinh", Operation::Sinh),
        ("tanh", Operation::Tanh),
        ("acosh", Operation::Acosh),
        ("asinh", Operation::Asinh),
        ("atanh", Operation::Atanh),
        ("degrees", Operation::Degrees),
        ("radians", Operation::Radians),
        ("exp", Operation::Exp),
        ("pi", Operation::Pi),
        ("signbit", Operation::SignBit),
        ("even", Operation::Even),
        ("nextafter", Operation::NextAfter),
    ] {
        registry
            .register_scalar(Arc::new(FloatingMath {
                name,
                operation,
                binding: None,
            }))
            .expect("unique floating math function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FloatingMath {
    fn candidates(&self) -> Vec<ScalarSignature> {
        let signature = |arguments, return_type| ScalarSignature {
            arguments,
            return_type,
            argument_names: None,
        };
        match self.operation {
            Operation::Pi => vec![signature(Vec::new(), DataType::Double)],
            Operation::SignBit => [DataType::Float, DataType::Double]
                .into_iter()
                .map(|input| signature(vec![input], DataType::Boolean))
                .collect(),
            Operation::NextAfter => [DataType::Double, DataType::Float]
                .into_iter()
                .map(|input| signature(vec![input.clone(), input.clone()], input))
                .collect(),
            Operation::Atan2 => vec![signature(
                vec![DataType::Double, DataType::Double],
                DataType::Double,
            )],
            _ => vec![signature(vec![DataType::Double], DataType::Double)],
        }
    }

    fn uses_ieee_setting(&self) -> bool {
        matches!(
            self.operation,
            Operation::Acos
                | Operation::Asin
                | Operation::Cos
                | Operation::Cot
                | Operation::Sin
                | Operation::Tan
                | Operation::Atanh
        )
    }

    fn binding(&self) -> Result<&Binding> {
        self.binding.as_ref().ok_or_else(|| {
            Error::Unsupported(format!(
                "{} requires selected statement-local binding",
                self.name
            ))
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for FloatingMath {
    fn name(&self) -> &str {
        self.name
    }

    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self
            .binding
            .as_ref()
            .is_some_and(|binding| binding.known_null)
        {
            ArgumentEvaluation::TypeOnly
        } else {
            ArgumentEvaluation::NullOnConstant
        }
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let candidates = self.candidates();
        ScalarSignature::validate_candidates(self.name, &candidates, query)?;
        let selected = arguments.select_overload(self.name, &candidates)?;
        let signature = ScalarSignature::selected(&candidates, selected)?.clone();
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Internal(
                "floating math overload changed argument count".into(),
            ));
        }
        let mut known_null = false;
        for index in 0..arguments.len() {
            if arguments.is_provably_null(index)? {
                known_null = true;
                break;
            }
        }
        // Native binding resolves a provable default-NULL before consulting a
        // bind callback. A NULL statement setting means its declared default.
        let ieee = if known_null || !self.uses_ieee_setting() {
            true
        } else {
            match query.settings().get("ieee_floating_point_ops", query)? {
                Value::Boolean(value) => *value,
                Value::Null => true,
                _ => return Err(Error::Internal("IEEE setting is not BOOLEAN".into())),
            }
        };
        query.check()?;
        Ok(Some(Arc::new(Self {
            name: self.name,
            operation: self.operation,
            binding: Some(Binding {
                signature,
                ieee,
                known_null,
            }),
        })))
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        let signature = &self.binding()?.signature;
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Bind(format!(
                "{} requires {} argument{}",
                self.name,
                signature.arguments.len(),
                if signature.arguments.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )));
        }
        Ok(signature.arguments.clone())
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        let signature = &self.binding()?.signature;
        if arguments != signature.arguments {
            return Err(Error::Bind(format!(
                "{} arguments differ from the selected overload",
                self.name
            )));
        }
        Ok(signature.return_type.clone())
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let binding = self.binding()?;
        if binding.known_null {
            if !arguments.is_empty() {
                return Err(Error::Internal(
                    "constant NULL floating math received arguments".into(),
                ));
            }
            return Ok(Value::Null);
        }
        if arguments.len() != binding.signature.arguments.len() {
            return Err(Error::Internal(
                "floating math argument count changed after binding".into(),
            ));
        }
        for (value, data_type) in arguments.iter().zip(&binding.signature.arguments) {
            if !value.is_null() && !value.fits_type(data_type) {
                return Err(Error::Internal(
                    "floating math input was not coerced".into(),
                ));
            }
        }
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }

        let result = match self.operation {
            Operation::Pi => Value::Double(PI),
            Operation::SignBit => match arguments[0] {
                Value::Float(value) => Value::Boolean(value.is_sign_negative()),
                Value::Double(value) => Value::Boolean(value.is_sign_negative()),
                _ => unreachable!("selected signbit input"),
            },
            Operation::NextAfter => match (&arguments[0], &arguments[1]) {
                (Value::Float(input), Value::Float(toward)) => {
                    Value::Float(next_after_f32(*input, *toward))
                }
                (Value::Double(input), Value::Double(toward)) => {
                    Value::Double(next_after_f64(*input, *toward))
                }
                _ => unreachable!("selected nextafter inputs"),
            },
            operation => {
                let Value::Double(input) = arguments[0] else {
                    unreachable!("selected DOUBLE input")
                };
                let second = arguments.get(1).map(|value| match value {
                    Value::Double(value) => *value,
                    _ => unreachable!("selected DOUBLE input"),
                });
                Value::Double(evaluate_double(operation, input, second, binding.ieee)?)
            }
        };
        query.check()?;
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn evaluate_double(
    operation: Operation,
    input: f64,
    second: Option<f64>,
    ieee: bool,
) -> Result<f64> {
    // The strict core wrappers pass NaNs through before calling the platform
    // trigonometric functions. Besides avoiding a domain error, this preserves
    // the sign and payload rather than accepting a libm-specific NaN result.
    if !ieee
        && input.is_nan()
        && matches!(
            operation,
            Operation::Acos
                | Operation::Asin
                | Operation::Cos
                | Operation::Cot
                | Operation::Sin
                | Operation::Tan
        )
    {
        return Ok(input);
    }
    Ok(match operation {
        Operation::Acos => {
            if !ieee {
                reject_infinite(input)?;
                if input.abs() > 1.0 {
                    return Err(Error::InvalidInput(
                        "ACOS is undefined outside [-1,1]".into(),
                    ));
                }
            }
            input.acos()
        }
        Operation::Asin => {
            if !ieee {
                reject_infinite(input)?;
                if input.abs() > 1.0 {
                    return Err(Error::InvalidInput(
                        "ASIN is undefined outside [-1,1]".into(),
                    ));
                }
            }
            input.asin()
        }
        Operation::Atan => input.atan(),
        Operation::Atan2 => input
            .atan2(second.ok_or_else(|| Error::Internal("atan2 missing second argument".into()))?),
        Operation::Cbrt => input.cbrt(),
        Operation::Cos => {
            if !ieee {
                reject_infinite(input)?;
            }
            input.cos()
        }
        Operation::Cot => {
            if !ieee {
                reject_infinite(input)?;
                if input == 0.0 {
                    return Err(Error::OutOfRange(
                        "input is out of range for numeric function cotangent".into(),
                    ));
                }
            }
            1.0 / input.tan()
        }
        Operation::Sin => {
            if !ieee {
                reject_infinite(input)?;
            }
            input.sin()
        }
        Operation::Tan => {
            if !ieee {
                reject_infinite(input)?;
            }
            input.tan()
        }
        Operation::Cosh => input.cosh(),
        Operation::Sinh => input.sinh(),
        Operation::Tanh => input.tanh(),
        Operation::Acosh => input.acosh(),
        Operation::Asinh => input.asinh(),
        Operation::Atanh => {
            if !ieee && input.abs() > 1.0 {
                return Err(Error::InvalidInput(
                    "ATANH is undefined outside [-1,1]".into(),
                ));
            }
            input.atanh()
        }
        Operation::Degrees => input * (180.0 / PI),
        Operation::Radians => input * (PI / 180.0),
        Operation::Exp => input.exp(),
        Operation::Even => even(input),
        Operation::Pi | Operation::SignBit | Operation::NextAfter => {
            return Err(Error::Internal(
                "non-DOUBLE operation reached DOUBLE evaluator".into(),
            ));
        }
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn reject_infinite(input: f64) -> Result<()> {
    if input.is_infinite() {
        return Err(Error::OutOfRange(format!(
            "input value {input} is out of range for numeric function"
        )));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn even(input: f64) -> f64 {
    if input.is_nan() {
        return f64::from_bits(input.to_bits() ^ (1_u64 << 63));
    }
    let mut value = if input >= 0.0 {
        input.ceil()
    } else {
        -(-input).ceil()
    };
    if (value / 2.0).floor() * 2.0 != value {
        if input >= 0.0 {
            value += 1.0;
        } else {
            value -= 1.0;
        }
    }
    value
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn next_after_f64(input: f64, toward: f64) -> f64 {
    if input.is_nan() || toward.is_nan() {
        return input + toward;
    }
    if input == toward {
        return toward;
    }
    if input == 0.0 {
        return f64::from_bits(1 | (toward.to_bits() & (1_u64 << 63)));
    }
    let bits = input.to_bits();
    if (toward > input) == (input > 0.0) {
        f64::from_bits(bits + 1)
    } else {
        f64::from_bits(bits - 1)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn next_after_f32(input: f32, toward: f32) -> f32 {
    if input.is_nan() || toward.is_nan() {
        return input + toward;
    }
    if input == toward {
        return toward;
    }
    if input == 0.0 {
        return f32::from_bits(1 | (toward.to_bits() & (1_u32 << 31)));
    }
    let bits = input.to_bits();
    if (toward > input) == (input > 0.0) {
        f32::from_bits(bits + 1)
    } else {
        f32::from_bits(bits - 1)
    }
}
