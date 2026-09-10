//! ABS retains its selected logical domain: an i128 payload is not permission
//! to return 128 from abs(TINYINT(-128)). Input coercion remains a bound cast.
use std::sync::Arc;

use crate::{
    common::{DataType, Error, Result, Value, numeric::decimal, type_registry::TypeRegistry},
    function::{ArgumentEvaluation, FunctionRegistry, ScalarBindArguments, ScalarFunction},
    parallel::QueryContext,
};

#[derive(Debug)]
struct Absolute {
    input: Option<DataType>,
    known_null: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    registry
        .register_scalar(Arc::new(Absolute {
            input: None,
            known_null: false,
        }))
        .expect("unique absolute scalar function");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn target(arguments: &[DataType]) -> Result<DataType> {
    match arguments {
        [DataType::Null] => Ok(DataType::BigInt),
        [DataType::Bignum] => Ok(DataType::Double),
        [input] if input.is_numeric() => Ok(input.clone()),
        _ => Err(Error::Bind(format!("no overload for abs({arguments:?})"))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Absolute {
    fn name(&self) -> &str {
        "abs"
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if arguments.len() != 1 {
            return Err(Error::Bind("abs requires one argument".into()));
        }
        let input = target(&[arguments.data_type(0)?])?;
        // The native decimal template resolves a provably NULL child to SQL
        // NULL. Failed/effectful probes must retain their ordinary lazy path.
        let known_null = input.is_decimal() && arguments.is_provably_null(0)?;
        Ok(Some(Arc::new(Self {
            input: Some(input),
            known_null,
        })))
    }

    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self.known_null {
            ArgumentEvaluation::TypeOnly
        } else {
            ArgumentEvaluation::Eager
        }
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        Ok(vec![target(arguments)?])
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        let target = target(arguments)?;
        if self.input.as_ref().is_some_and(|input| *input != target) {
            return Err(Error::Bind("abs input type changed after binding".into()));
        }
        Ok(if self.known_null {
            DataType::Null
        } else {
            target
        })
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let input = self
            .input
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound abs function".into()))?;
        if self.known_null {
            return if arguments.is_empty() {
                Ok(Value::Null)
            } else {
                Err(Error::Internal("abs evaluated a type-only argument".into()))
            };
        }
        let [value] = arguments else {
            return Err(Error::Internal("abs argument count".into()));
        };
        if !value.fits_type(input) {
            return Err(Error::Internal("abs input was not coerced".into()));
        }
        let result = match value {
            Value::Null => Value::Null,
            Value::Integer(value) => Value::Integer(
                value
                    .checked_abs()
                    .ok_or_else(|| Error::OutOfRange(format!("Overflow on abs({value})")))?,
            ),
            Value::Unsigned(_) => value.clone(),
            Value::Float(value) => Value::Float(value.abs()),
            Value::Double(value) => Value::Double(value.abs()),
            Value::Decimal {
                value,
                width,
                scale,
            } => decimal(value.abs(), *width, *scale)?,
            _ => return Err(Error::Internal("abs input was not coerced".into())),
        };
        // Validate against the retained signed width, not the physical i128
        // container. Only a valid input reaching this boundary can overflow.
        if !result.fits_type(input) {
            return Err(Error::OutOfRange(format!("Overflow on abs({value})")));
        }
        Ok(result)
    }
}
