//! Checked integer factorial, greatest-common-divisor and least-common-multiple
//! overloads. Logical width remains part of execution even though signed values
//! share the engine's i128 scalar payload.

use std::sync::Arc;

use crate::{
    common::{DataType, Error, Result, Value, type_registry::TypeRegistry},
    function::{FunctionRegistry, ScalarBindArguments, ScalarFunction, ScalarSignature},
    parallel::QueryContext,
};

#[derive(Clone, Copy, Debug)]
enum Operation {
    Factorial,
    Gcd,
    Lcm,
}

#[derive(Debug)]
struct Discrete {
    name: &'static str,
    operation: Operation,
    signature: Option<ScalarSignature>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, operation) in [
        ("factorial", Operation::Factorial),
        ("gcd", Operation::Gcd),
        ("greatest_common_divisor", Operation::Gcd),
        ("lcm", Operation::Lcm),
        ("least_common_multiple", Operation::Lcm),
    ] {
        registry
            .register_scalar(Arc::new(Discrete {
                name,
                operation,
                signature: None,
            }))
            .expect("unique discrete numeric function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Discrete {
    fn candidates(&self) -> Vec<ScalarSignature> {
        match self.operation {
            Operation::Factorial => vec![ScalarSignature {
                arguments: vec![DataType::Integer],
                return_type: DataType::HugeInt,
                argument_names: None,
            }],
            Operation::Gcd | Operation::Lcm => [DataType::BigInt, DataType::HugeInt]
                .into_iter()
                .map(|data_type| ScalarSignature {
                    arguments: vec![data_type.clone(), data_type.clone()],
                    return_type: data_type,
                    argument_names: None,
                })
                .collect(),
        }
    }

    fn signature(&self) -> Result<&ScalarSignature> {
        self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported(format!(
                "{} requires selected statement-local binding",
                self.name
            ))
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Discrete {
    fn name(&self) -> &str {
        self.name
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
        if signature.arguments.len() != arguments.len() {
            return Err(Error::Internal(
                "discrete numeric overload changed argument count".into(),
            ));
        }
        Ok(Some(Arc::new(Self {
            name: self.name,
            operation: self.operation,
            signature: Some(signature),
        })))
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        let signature = self.signature()?;
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
        let signature = self.signature()?;
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
        let signature = self.signature()?;
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Internal(
                "discrete numeric argument count changed after binding".into(),
            ));
        }
        for (value, data_type) in arguments.iter().zip(&signature.arguments) {
            if !value.is_null() && !value.fits_type(data_type) {
                return Err(Error::Internal(
                    "discrete numeric input was not coerced".into(),
                ));
            }
        }
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let result = match self.operation {
            Operation::Factorial => factorial(arguments[0].as_i128()?, query)?,
            Operation::Gcd => gcd(
                arguments[0].as_i128()?,
                arguments[1].as_i128()?,
                &signature.return_type,
                query,
            )?,
            Operation::Lcm => lcm(
                arguments[0].as_i128()?,
                arguments[1].as_i128()?,
                &signature.return_type,
                query,
            )?,
        };
        let result = Value::Integer(result);
        if !result.fits_type(&signature.return_type) {
            return Err(Error::OutOfRange("Value out of range".into()));
        }
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn factorial(input: i128, query: &QueryContext) -> Result<i128> {
    if input < 0 {
        return Err(Error::OutOfRange(
            "factorial of a negative number is undefined".into(),
        ));
    }
    let mut result = 1_i128;
    for factor in 2..=input {
        query.check()?;
        result = result
            .checked_mul(factor)
            .ok_or_else(|| Error::OutOfRange("Value out of range".into()))?;
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn absolute(value: i128, data_type: &DataType) -> Result<i128> {
    let result = value
        .checked_abs()
        .ok_or_else(|| Error::OutOfRange(format!("Overflow on abs({value})")))?;
    if !Value::Integer(result).fits_type(data_type) {
        return Err(Error::OutOfRange(format!("Overflow on abs({value})")));
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn remainder(dividend: i128, divisor: i128) -> i128 {
    // The sole signed remainder overflow has mathematical remainder zero.
    dividend.checked_rem(divisor).unwrap_or(0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn gcd(
    mut left: i128,
    mut right: i128,
    data_type: &DataType,
    query: &QueryContext,
) -> Result<i128> {
    // Avoid signed-min modulo -1 before entering the ordinary Euclidean loop.
    if (left == i128::MIN && right == -1) || (left == -1 && right == i128::MIN) {
        return Ok(1);
    }
    loop {
        query.check()?;
        if left == 0 {
            return absolute(right, data_type);
        }
        right = remainder(right, left);
        if right == 0 {
            return absolute(left, data_type);
        }
        left = remainder(left, right);
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn lcm(left: i128, right: i128, data_type: &DataType, query: &QueryContext) -> Result<i128> {
    if left == 0 || right == 0 {
        return Ok(0);
    }
    let divisor = gcd(left, right, data_type, query)?;
    let result = left
        .checked_mul(right / divisor)
        .ok_or_else(|| Error::OutOfRange("lcm value is out of range".into()))?;
    if !Value::Integer(result).fits_type(data_type) {
        return Err(Error::OutOfRange("lcm value is out of range".into()));
    }
    absolute(result, data_type)
}
