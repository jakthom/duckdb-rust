//! NUL-safe VARCHAR codepoint primitives.
//!
//! These functions operate on Rust `String` values rather than C strings, so
//! embedded U+0000 is ordinary data throughout both scalar and vector paths.

use std::sync::Arc;

use crate::{
    common::{
        type_registry::TypeRegistry,
        vector::{DataChunk, Vector},
        DataType, Error, Result, Value,
    },
    function::{FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
};

#[derive(Debug)]
struct Chr;
#[derive(Debug)]
struct Ascii;
#[derive(Debug)]
struct Contains;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    registry
        .register_scalar(Arc::new(Chr))
        .expect("unique chr scalar function");
    registry
        .register_scalar(Arc::new(Ascii))
        .expect("unique ascii scalar function");
    // This is deliberately VARCHAR-only. Nested `contains` families retain
    // their own type-directed implementations rather than being stringified.
    registry
        .register_scalar(Arc::new(Contains))
        .expect("unique varchar contains scalar function");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unary_varchar_batch(
    arguments: &DataChunk,
    output_type: DataType,
    query: &QueryContext,
    apply: impl Fn(&Value) -> Result<Value> + Copy,
) -> Result<Option<Vector>> {
    let [column] = arguments.columns() else {
        return Ok(None);
    };
    if column.data_type() != &DataType::Varchar {
        return Ok(None);
    }
    if let Some(value) = column.constant_value() {
        return Vector::constant(output_type, apply(value)?, column.len()).map(Some);
    }
    if column.dictionary().is_some() {
        let mapped = column.map_dictionary_parent(output_type.clone(), |parent| {
            let values = parent.values().enumerate().map(|(index, value)| {
                if index % 1024 == 0 {
                    query.check()?;
                }
                apply(&value)
            });
            Vector::flat(output_type.clone(), values.collect::<Result<Vec<_>>>()?)
        })?;
        query.check()?;
        return Ok(Some(mapped));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(arguments.len())
        .map_err(|_| Error::Resource("cannot allocate VARCHAR codepoint result column".into()))?;
    for (index, value) in column.values().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        output.push(apply(&value)?);
    }
    query.check()?;
    Vector::flat(output_type, output).map(Some)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn chr_value(value: &Value) -> Result<Value> {
    let Value::Integer(value) = value else {
        return if value.is_null() {
            Ok(Value::Null)
        } else {
            Err(Error::Internal("chr argument is not an INTEGER".into()))
        };
    };
    let codepoint = u32::try_from(*value)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(|| Error::InvalidInput(format!("Invalid UTF8 Codepoint {value}")))?;
    Ok(Value::Varchar(codepoint.to_string()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ascii_value(value: &Value) -> Result<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Varchar(value) => Ok(Value::Integer(i128::from(
            value.chars().next().map(u32::from).unwrap_or(0),
        ))),
        _ => Err(Error::Internal("ascii argument is not VARCHAR".into())),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn contains_value(arguments: &[Value]) -> Result<Value> {
    let [haystack, needle] = arguments else {
        return Err(Error::Internal("contains argument count".into()));
    };
    match (haystack, needle) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Varchar(haystack), Value::Varchar(needle)) => {
            Ok(Value::Boolean(haystack.contains(needle)))
        }
        _ => Err(Error::Internal(
            "VARCHAR contains arguments are not VARCHAR".into(),
        )),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn varchar_arguments(name: &str, arguments: &[DataType], count: usize) -> Result<Vec<DataType>> {
    if arguments.len() != count
        || !arguments
            .iter()
            .all(|argument| matches!(argument, DataType::Varchar | DataType::Null))
    {
        return Err(Error::Bind(format!(
            "{name} requires {count} VARCHAR argument(s)"
        )));
    }
    Ok(vec![DataType::Varchar; count])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_contains(arguments: &DataChunk, query: &QueryContext) -> Result<Option<Vector>> {
    let [left, right] = arguments.columns() else {
        return Ok(None);
    };
    if left.data_type() != &DataType::Varchar || right.data_type() != &DataType::Varchar {
        return Ok(None);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(arguments.len())
        .map_err(|_| Error::Resource("cannot allocate VARCHAR contains result column".into()))?;
    for index in 0..arguments.len() {
        if index % 1024 == 0 {
            query.check()?;
        }
        let left = left.value(index).ok_or_else(|| {
            Error::Internal("VARCHAR contains left encoding is out of bounds".into())
        })?;
        let right = right.value(index).ok_or_else(|| {
            Error::Internal("VARCHAR contains right encoding is out of bounds".into())
        })?;
        output.push(contains_value(&[left, right])?);
    }
    query.check()?;
    Vector::flat(DataType::Boolean, output).map(Some)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Ascii {
    fn name(&self) -> &str {
        "ascii"
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        varchar_arguments("ascii", arguments, 1)
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        varchar_arguments("ascii", arguments, 1)?;
        Ok(DataType::BigInt)
    }
    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::Varchar]
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        unary_varchar_batch(arguments, DataType::BigInt, query, ascii_value)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [value] = arguments else {
            return Err(Error::Internal("ascii argument count".into()));
        };
        ascii_value(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Contains {
    fn name(&self) -> &str {
        "contains"
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        varchar_arguments("contains", arguments, 2)
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        varchar_arguments("contains", arguments, 2)?;
        Ok(DataType::Boolean)
    }
    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::Varchar, DataType::Varchar]
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        batch_contains(arguments, query)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        contains_value(arguments)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Chr {
    fn name(&self) -> &str {
        "chr"
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        match arguments {
            [argument] if argument.is_integer() || *argument == DataType::Null => {
                Ok(vec![DataType::BigInt])
            }
            _ => Err(Error::Bind("chr requires an INTEGER argument".into())),
        }
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        match arguments {
            [argument] if argument.is_integer() || *argument == DataType::Null => {}
            _ => return Err(Error::Bind(format!("no overload for chr({arguments:?})"))),
        }
        Ok(DataType::Varchar)
    }
    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::BigInt]
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        let [column] = arguments.columns() else {
            return Ok(None);
        };
        if column.data_type() != &DataType::BigInt {
            return Ok(None);
        }
        if let Some(value) = column.constant_value() {
            return Vector::constant(DataType::Varchar, chr_value(value)?, column.len()).map(Some);
        }
        if column.dictionary().is_some() {
            let mapped = column.map_dictionary_parent(DataType::Varchar, |parent| {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(parent.len())
                    .map_err(|_| Error::Resource("cannot allocate chr dictionary parent".into()))?;
                for (index, value) in parent.values().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    values.push(chr_value(&value)?);
                }
                Vector::flat(DataType::Varchar, values)
            })?;
            query.check()?;
            return Ok(Some(mapped));
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate chr result column".into()))?;
        for (index, value) in column.values().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            values.push(chr_value(&value)?);
        }
        query.check()?;
        Vector::flat(DataType::Varchar, values).map(Some)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [value] = arguments else {
            return Err(Error::Internal("chr argument count".into()));
        };
        chr_value(value)
    }
}
