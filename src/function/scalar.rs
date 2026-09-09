use std::sync::Arc;

use super::{ArgumentEvaluation, FunctionRegistry, ScalarFunction};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
struct Builtin(&'static str);

pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "abs",
        "lower",
        "upper",
        "length",
        "char_length",
        "coalesce",
        "nullif",
        "concat",
        "sqrt",
        "round",
    ] {
        registry
            .register_scalar(Arc::new(Builtin(name)))
            .expect("unique builtin name");
    }
}

impl ScalarFunction for Builtin {
    fn name(&self) -> &str {
        self.0
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self.0 == "coalesce" {
            ArgumentEvaluation::FirstNonNull
        } else {
            ArgumentEvaluation::Eager
        }
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let count = arguments.len();
        match self.0 {
            "coalesce" if count > 0 => arguments
                .iter()
                .try_fold(DataType::Null, |t, a| types.common_type(&t, a)),
            "nullif" if count == 2 => types.common_type(&arguments[0], &arguments[1]),
            "concat" => Ok(DataType::Varchar),
            "lower" | "upper"
                if count == 1 && matches!(arguments[0], DataType::Varchar | DataType::Null) =>
            {
                Ok(DataType::Varchar)
            }
            "length" | "char_length"
                if count == 1 && matches!(arguments[0], DataType::Varchar | DataType::Null) =>
            {
                Ok(DataType::BigInt)
            }
            "abs" | "round"
                if count == 1 && (arguments[0].is_numeric() || arguments[0] == DataType::Null) =>
            {
                Ok(arguments[0].clone())
            }
            "sqrt"
                if count == 1 && (arguments[0].is_numeric() || arguments[0] == DataType::Null) =>
            {
                Ok(DataType::Double)
            }
            _ => Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.0
            ))),
        }
    }
    fn evaluate(&self, args: &[Value], context: &QueryContext) -> Result<Value> {
        context.check()?;
        match self.0 {
            "coalesce" => {
                return Ok(args
                    .iter()
                    .find(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(Value::Null));
            }
            "concat" => {
                return Ok(Value::Varchar(
                    args.iter()
                        .filter(|v| !v.is_null())
                        .map(ToString::to_string)
                        .collect(),
                ));
            }
            "nullif" => {
                return if args[0].is_null()
                    || (!args[1].is_null()
                        && context
                            .types()
                            .bind(
                                &context
                                    .types()
                                    .common_type(&args[0].data_type(), &args[1].data_type())?,
                            )?
                            .compare(&args[0], &args[1], context)?
                            .is_eq())
                {
                    Ok(Value::Null)
                } else {
                    Ok(args[0].clone())
                };
            }
            _ => {}
        }
        if args[0].is_null() {
            return Ok(Value::Null);
        }
        Ok(match self.0 {
            "abs" => match &args[0] {
                Value::Integer(v) => Value::Integer(
                    v.checked_abs()
                        .ok_or_else(|| Error::Execution("integer overflow".into()))?,
                ),
                Value::Float(v) => Value::Float(v.abs()),
                _ => Value::Double(args[0].as_f64()?.abs()),
            },
            "lower" => Value::Varchar(args[0].to_string().to_lowercase()),
            "upper" => Value::Varchar(args[0].to_string().to_uppercase()),
            "length" | "char_length" => Value::Integer(args[0].to_string().chars().count() as i128),
            "sqrt" => {
                let v = args[0].as_f64()?;
                if v < 0.0 {
                    return Err(Error::Execution(
                        "cannot take square root of a negative number".into(),
                    ));
                }
                Value::Double(v.sqrt())
            }
            "round" => match &args[0] {
                Value::Integer(_) => args[0].clone(),
                Value::Float(v) => Value::Float(v.round()),
                _ => Value::Double(args[0].as_f64()?.round()),
            },
            _ => return Err(Error::Internal("unregistered builtin".into())),
        })
    }
}
