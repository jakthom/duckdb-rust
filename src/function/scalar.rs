use std::sync::Arc;

use super::{ArgumentEvaluation, FunctionRegistry, ScalarFunction};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
struct Builtin(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    registry
        .register_scalar(Arc::new(TypeOf(None)))
        .expect("unique typeof");
    for name in [
        "abs",
        "lower",
        "upper",
        "length",
        "char_length",
        "character_length",
        "len",
        "coalesce",
        "nullif",
        "concat",
        "sqrt",
        "round",
        "trunc",
    ] {
        registry
            .register_scalar(Arc::new(Builtin(name)))
            .expect("unique builtin name");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
    fn argument_types(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if matches!(self.0, "coalesce" | "nullif") {
            return Ok(vec![self.return_type(arguments, types)?; arguments.len()]);
        }
        if matches!(
            self.0,
            "lower" | "upper" | "length" | "char_length" | "character_length" | "len"
        ) && arguments.len() == 1
            && matches!(arguments[0], DataType::Enum(_))
        {
            return Ok(vec![DataType::Varchar]);
        }
        if self.0 == "trunc" && arguments == [DataType::Null] {
            return Ok(vec![DataType::BigInt]);
        }
        if self.0 == "round" && arguments.len() == 1 && arguments[0].is_unsigned_integer() {
            return Ok(vec![match arguments[0] {
                DataType::UBigInt => DataType::HugeInt,
                DataType::UHugeInt => DataType::Double,
                _ => DataType::BigInt,
            }]);
        }
        Ok(arguments.to_vec())
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
            "length" | "char_length" | "character_length" | "len"
                if count == 1
                    && matches!(
                        arguments[0],
                        DataType::Varchar | DataType::Bit | DataType::Null
                    ) =>
            {
                Ok(DataType::BigInt)
            }
            "abs"
                if count == 1 && (arguments[0].is_numeric() || arguments[0] == DataType::Null) =>
            {
                Ok(arguments[0].clone())
            }
            "round" | "trunc"
                if count == 1 && (arguments[0].is_numeric() || arguments[0] == DataType::Null) =>
            {
                Ok(match arguments[0] {
                    DataType::Decimal { width, .. } => DataType::Decimal { width, scale: 0 },
                    _ => arguments[0].clone(),
                })
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
                Value::Unsigned(_) => args[0].clone(),
                Value::Decimal {
                    value,
                    width,
                    scale,
                } => crate::common::numeric::decimal(value.abs(), *width, *scale)?,
                _ => Value::Double(args[0].as_f64()?.abs()),
            },
            "lower" => Value::Varchar(args[0].to_string().to_lowercase()),
            "upper" => Value::Varchar(args[0].to_string().to_uppercase()),
            "length" | "char_length" | "character_length" | "len" => {
                Value::Integer(match &args[0] {
                    Value::Bit(value) => value.length() as i128,
                    value => value.to_string().chars().count() as i128,
                })
            }
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
                Value::Integer(_) | Value::Unsigned(_) => args[0].clone(),
                Value::Decimal {
                    value,
                    width,
                    scale,
                } => crate::common::numeric::decimal(
                    crate::common::numeric::rescale(*value, *scale, 0)?,
                    *width,
                    0,
                )?,
                Value::Float(v) => Value::Float(v.round()),
                _ => Value::Double(args[0].as_f64()?.round()),
            },
            "trunc" => match &args[0] {
                Value::Integer(_) | Value::Unsigned(_) => args[0].clone(),
                Value::Decimal {
                    value,
                    width,
                    scale,
                } => crate::common::numeric::decimal(
                    *value / crate::common::numeric::DECIMAL_POWERS[usize::from(*scale)] as i128,
                    *width,
                    0,
                )?,
                Value::Float(value) => Value::Float(value.trunc()),
                _ => Value::Double(args[0].as_f64()?.trunc()),
            },
            _ => return Err(Error::Internal("unregistered builtin".into())),
        })
    }
}

#[derive(Debug)]
struct TypeOf(Option<DataType>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for TypeOf {
    fn name(&self) -> &str {
        "typeof"
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::TypeOnly
    }
    fn bind(
        &self,
        arguments: &dyn super::ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if arguments.len() != 1 {
            return Err(Error::Bind("typeof requires one argument".into()));
        }
        Ok(Some(Arc::new(Self(Some(arguments.data_type(0)?)))))
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if self.0.as_ref().is_some_and(|t| arguments == [t.clone()]) {
            Ok(DataType::Varchar)
        } else {
            Err(Error::Bind(
                "typeof requires contextual type binding".into(),
            ))
        }
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if !arguments.is_empty() {
            return Err(Error::Internal("typeof evaluated its argument".into()));
        }
        Ok(Value::Varchar(
            self.0
                .as_ref()
                .ok_or_else(|| Error::Internal("unbound typeof".into()))?
                .to_string(),
        ))
    }
}
