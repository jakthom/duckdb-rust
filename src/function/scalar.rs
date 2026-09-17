use std::sync::Arc;
mod conditional;
mod numeric;
mod text;

use super::{ArgumentEvaluation, FunctionRegistry, ScalarFunction};
use crate::{
    common::{
        DataType, Error, Result, Value,
        vector::{DataChunk, Vector},
    },
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
    fn is_total(&self, _arguments: &[Option<&Value>]) -> bool {
        matches!(
            self.0,
            "length" | "char_length" | "character_length" | "len"
        )
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        matches!(
            self.0,
            "length" | "char_length" | "character_length" | "len"
        ) && matches!(arguments, [DataType::Varchar] | [DataType::Bit])
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        query.check()?;
        let Some(column) = arguments.columns().first() else {
            return Ok(None);
        };
        if arguments.columns().len() != 1
            || !self.supports_batch_evaluation(std::slice::from_ref(column.data_type()))
        {
            return Ok(None);
        }
        if let Some(value) = column.constant_value() {
            return Vector::constant(
                DataType::BigInt,
                length_value(value)?.map_or(Value::Null, Value::Integer),
                column.len(),
            )
            .map(Some);
        }
        if let Some((parent, selection)) = column.dictionary() {
            let values = parent.values().enumerate().map(|(index, value)| {
                if index % 1024 == 0 {
                    query.check()?;
                }
                length_value(&value)
            });
            let parent = Arc::new(Vector::try_bigints(values)?);
            query.check()?;
            return parent.select(selection.to_vec()).map(Some);
        }
        let values = column.values().enumerate().map(|(index, value)| {
            if index % 1024 == 0 {
                query.check()?;
            }
            length_value(&value)
        });
        let output = Vector::try_bigints(values)?;
        query.check()?;
        Ok(Some(output))
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
                length_value(&args[0])?.map_or(Value::Null, Value::Integer)
            }
            _ => return Err(Error::Internal("unregistered builtin".into())),
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn length_value(value: &Value) -> Result<Option<i64>> {
    let length = match value {
        Value::Null => return Ok(None),
        Value::Bit(value) => value.length(),
        Value::Varchar(value) => value.chars().count(),
        _ => return Err(Error::Internal("length argument has wrong type".into())),
    };
    i64::try_from(length)
        .map(Some)
        .map_err(|_| Error::Resource("VARCHAR length exceeds BIGINT".into()))
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::common::{BitString, vector::DataChunk};

    #[test]
    fn length_batch_preserves_constant_dictionary_slice_and_bit_contracts() -> Result<()> {
        let query = QueryContext::background();
        let length = Builtin("length");
        let parent = Arc::new(Vector::flat(
            DataType::Varchar,
            vec![
                Value::Varchar("é".into()),
                Value::Null,
                Value::Varchar("a\0🦆".into()),
            ],
        )?);
        let dictionary = parent.select(vec![2, 0, 1, 2])?.slice(1, 3)?;
        let output = length
            .evaluate_batch(&DataChunk::new(vec![dictionary], 3)?, &query)?
            .expect("length batch callback");
        assert!(output.dictionary().is_some());
        assert_eq!(
            output.values().collect::<Vec<_>>(),
            vec![Value::Integer(1), Value::Null, Value::Integer(3)]
        );

        let constant = Vector::constant(DataType::Varchar, Value::Varchar("é🦆".into()), 3)?;
        let output = length
            .evaluate_batch(&DataChunk::new(vec![constant], 3)?, &query)?
            .expect("constant length batch callback");
        assert_eq!(output.constant_value(), Some(&Value::Integer(2)));

        let bit = Vector::constant(
            DataType::Bit,
            Value::Bit(Arc::new(BitString::from_parts(vec![0b1010_0000], 3)?)),
            2,
        )?;
        let output = length
            .evaluate_batch(&DataChunk::new(vec![bit], 2)?, &query)?
            .expect("BIT length batch callback");
        assert_eq!(
            output.values().collect::<Vec<_>>(),
            vec![Value::Integer(3); 2]
        );
        Ok(())
    }
}
