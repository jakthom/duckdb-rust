//! Byte-oriented and grapheme-oriented VARCHAR metrics.
//!
//! `strlen` measures the original UTF-8 byte payload, while `reverse` uses
//! utf8proc's extended-grapheme boundaries and then copies those exact byte
//! ranges in reverse order.  No normalization or C-string operation is used,
//! so embedded NULs remain ordinary VARCHAR data.

use std::sync::Arc;

use crate::{
    common::{
        DataType, Error, Result, Value,
        type_registry::TypeRegistry,
        vector::{DataChunk, Vector},
    },
    function::{FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
};

#[derive(Debug)]
struct MetricsFunction(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["strlen", "reverse"] {
        registry
            .register_scalar(Arc::new(MetricsFunction(name)))
            .expect("unique VARCHAR metrics function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for MetricsFunction {
    fn name(&self) -> &str {
        self.0
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if arguments.len() == 1 && matches!(arguments[0], DataType::Varchar | DataType::Null) {
            Ok(vec![DataType::Varchar])
        } else {
            Err(Error::Bind(format!(
                "No function matches {}({arguments:?})",
                self.0,
            )))
        }
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        match (self.0, arguments) {
            ("strlen", [DataType::Varchar]) => Ok(DataType::BigInt),
            ("reverse", [DataType::Varchar]) => Ok(DataType::Varchar),
            _ => Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.0
            ))),
        }
    }

    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }

    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        matches!(arguments, [DataType::Varchar])
    }

    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        let [column] = arguments.columns() else {
            return Ok(None);
        };
        if column.data_type() != &DataType::Varchar {
            return Ok(None);
        }
        if self.0 == "strlen" {
            return batch_strlen(column, query).map(Some);
        }
        let output_type = self.result_type();
        let apply = |value: &Value| self.apply(value);
        if let Some(value) = column.constant_value() {
            query.check()?;
            return Vector::constant(output_type, apply(value)?, arguments.len()).map(Some);
        }
        if column.dictionary().is_some() {
            let mapped = column.map_dictionary_parent(output_type.clone(), |parent| {
                let mut output = Vec::new();
                output.try_reserve_exact(parent.len()).map_err(|_| {
                    Error::Resource("cannot allocate VARCHAR metrics dictionary parent".into())
                })?;
                for (index, value) in parent.values().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    output.push(apply(&value)?);
                }
                Vector::flat(output_type, output)
            })?;
            query.check()?;
            return Ok(Some(mapped));
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate VARCHAR metrics result column".into()))?;
        for (index, value) in column.values().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            output.push(apply(&value)?);
        }
        query.check()?;
        Vector::flat(output_type, output).map(Some)
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [value] = arguments else {
            return Err(Error::Internal(
                "VARCHAR metrics argument count changed after binding".into(),
            ));
        };
        self.apply(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn batch_strlen(column: &Vector, query: &QueryContext) -> Result<Vector> {
    let length = |value: &Value| match value {
        Value::Null => Ok(None),
        Value::Varchar(value) => i64::try_from(value.len())
            .map(Some)
            .map_err(|_| Error::OutOfRange("VARCHAR byte length exceeds BIGINT".into())),
        _ => Err(Error::Internal("strlen input is not VARCHAR".into())),
    };
    if let Some(value) = column.constant_value() {
        return Vector::constant(
            DataType::BigInt,
            length(value)?.map_or(Value::Null, |value| Value::Integer(i128::from(value))),
            column.len(),
        );
    }
    if column.dictionary().is_some() {
        let output = column.map_dictionary_parent(DataType::BigInt, |parent| {
            Vector::try_bigints(parent.values().enumerate().map(|(index, value)| {
                if index % 1024 == 0 {
                    query.check()?;
                }
                length(&value)
            }))
        })?;
        query.check()?;
        return Ok(output);
    }
    let output = if let Some(values) = column.flat_values() {
        Vector::try_bigints(values.iter().enumerate().map(|(index, value)| {
            if index % 1024 == 0 {
                query.check()?;
            }
            length(value)
        }))?
    } else {
        Vector::try_bigints(column.values().enumerate().map(|(index, value)| {
            if index % 1024 == 0 {
                query.check()?;
            }
            length(&value)
        }))?
    };
    query.check()?;
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl MetricsFunction {
    fn result_type(&self) -> DataType {
        match self.0 {
            "strlen" => DataType::BigInt,
            "reverse" => DataType::Varchar,
            _ => unreachable!("registered metrics function"),
        }
    }

    fn apply(&self, value: &Value) -> Result<Value> {
        match value {
            Value::Null => Ok(Value::Null),
            Value::Varchar(input) => match self.0 {
                "strlen" => Ok(Value::Integer(input.len() as i128)),
                "reverse" => reverse_graphemes(input).map(Value::Varchar),
                _ => unreachable!("registered metrics function"),
            },
            _ => Err(Error::Internal(
                "VARCHAR metrics input is not VARCHAR".into(),
            )),
        }
    }
}

/// Reverse UAX#29 extended grapheme clusters while retaining the exact source
/// byte slices. `grapheme_breaks` returns only interior boundaries, so the
/// leading and trailing byte offsets are added explicitly.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn reverse_graphemes(input: &str) -> Result<String> {
    if input.is_empty() {
        return Ok(String::new());
    }
    let mut output = String::new();
    output
        .try_reserve_exact(input.len())
        .map_err(|_| Error::Resource("cannot allocate reversed VARCHAR".into()))?;
    let mut boundaries = Vec::new();
    boundaries
        .try_reserve_exact(8)
        .map_err(|_| Error::Resource("cannot allocate VARCHAR grapheme boundaries".into()))?;
    boundaries.push(0);
    boundaries.extend(utf8proc::grapheme::grapheme_breaks(input));
    boundaries.push(input.len());
    for pair in boundaries.windows(2).rev() {
        output.push_str(&input[pair[0]..pair[1]]);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn reverse_keeps_extended_clusters_and_original_bytes() {
        assert_eq!(reverse_graphemes("").unwrap(), "");
        assert_eq!(reverse_graphemes("a\0é").unwrap(), "é\0a");
        assert_eq!(reverse_graphemes("S\u{308}a").unwrap(), "aS\u{308}");
        assert_eq!(
            reverse_graphemes("🤦🏼\u{200d}♂\u{fe0f}x").unwrap(),
            "x🤦🏼\u{200d}♂\u{fe0f}"
        );
    }
}
