use std::sync::Arc;
mod conditional;
mod numeric;
mod text;

use super::{ArgumentEvaluation, FunctionRegistry, ScalarFunction};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
struct Builtin(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    numeric::register(registry);
    conditional::register(registry);
    text::register(registry);
    registry
        .register_scalar(Arc::new(TypeOf(None)))
        .expect("unique typeof");
    for name in [
        "lower",
        "upper",
        "lcase",
        "ucase",
        "length",
        "char_length",
        "character_length",
        "len",
        "concat",
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
    fn bind(
        &self,
        arguments: &dyn super::ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        if self.0 == "concat" {
            super::nested::bind_concat(arguments, query)
        } else {
            Ok(None)
        }
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if self.0 == "concat" {
            if arguments.is_empty() {
                return Err(Error::Bind("concat requires at least one argument".into()));
            }
            return Ok(vec![DataType::Varchar; arguments.len()]);
        }
        if matches!(
            self.0,
            "lower"
                | "upper"
                | "lcase"
                | "ucase"
                | "length"
                | "char_length"
                | "character_length"
                | "len"
        ) && arguments.len() == 1
            && matches!(arguments[0], DataType::Enum(_))
        {
            return Ok(vec![DataType::Varchar]);
        }
        Ok(arguments.to_vec())
    }
    fn argument_cast_mode(&self, _index: usize) -> crate::common::cast::CastMode {
        if self.0 == "concat" {
            crate::common::cast::CastMode::Explicit
        } else {
            crate::common::cast::CastMode::Implicit
        }
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let count = arguments.len();
        match self.0 {
            "concat" => Ok(DataType::Varchar),
            "lower" | "upper" | "lcase" | "ucase"
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
            _ => Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.0
            ))),
        }
    }
    fn evaluate(&self, args: &[Value], context: &QueryContext) -> Result<Value> {
        context.check()?;
        if self.0 == "concat" {
            let mut output = String::new();
            for argument in args {
                context.check()?;
                let text = match argument {
                    Value::Null => continue,
                    Value::Varchar(text) => text,
                    _ => return Err(Error::Internal("concat argument is not VARCHAR".into())),
                };
                output
                    .try_reserve(text.len())
                    .map_err(|_| Error::Resource("concat result allocation failed".into()))?;
                output.push_str(text);
            }
            return Ok(Value::Varchar(output));
        }
        if args[0].is_null() {
            return Ok(Value::Null);
        }
        Ok(match self.0 {
            "lower" | "lcase" => match &args[0] {
                Value::Varchar(value) => Value::Varchar(case_convert(value, false)),
                _ => return Err(Error::Internal("case argument is not VARCHAR".into())),
            },
            "upper" | "ucase" => match &args[0] {
                Value::Varchar(value) => Value::Varchar(case_convert(value, true)),
                _ => return Err(Error::Internal("case argument is not VARCHAR".into())),
            },
            "length" | "char_length" | "character_length" | "len" => {
                Value::Integer(match &args[0] {
                    Value::Bit(value) => value.length() as i128,
                    Value::Varchar(value) => value.chars().count() as i128,
                    _ => return Err(Error::Internal("length argument has wrong type".into())),
                })
            }
            _ => return Err(Error::Internal("unregistered builtin".into())),
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn case_convert(input: &str, upper: bool) -> String {
    // DuckDB's pinned implementation maps each decoded codepoint with
    // utf8proc_toupper/utf8proc_tolower. Those are simple one-codepoint case
    // mappings, unlike Rust's full Unicode mappings which can expand one input
    // character into several output characters.
    let mut result = String::with_capacity(input.len());
    for character in input.chars() {
        result.push(if character.is_ascii() {
            if upper {
                character.to_ascii_uppercase()
            } else {
                character.to_ascii_lowercase()
            }
        } else if upper {
            utf8proc::case::to_upper(character)
        } else {
            utf8proc::case::to_lower(character)
        });
    }
    result
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
