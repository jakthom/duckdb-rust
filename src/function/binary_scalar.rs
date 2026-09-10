use std::sync::Arc;

use super::{
    FunctionRegistry, ScalarFunction,
    operator::{Operator, OperatorFunction, OperatorRegistry, OperatorSignature},
};
use crate::{
    common::{DataType, Error, Result, Value, scalar::hex_digit},
    parallel::QueryContext,
};

#[derive(Debug)]
struct BinaryFunction(&'static str, Option<DataType>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "encode",
        "decode",
        "octet_length",
        "hex",
        "to_hex",
        "unhex",
        "from_hex",
    ] {
        registry
            .register_scalar(Arc::new(BinaryFunction(name, None)))
            .expect("unique binary function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for BinaryFunction {
    fn name(&self) -> &str {
        self.0
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if matches!(self.0, "encode" | "unhex" | "from_hex" | "hex" | "to_hex")
            && arguments.len() == 1
            && matches!(arguments[0], DataType::Enum(_))
        {
            return Ok(vec![DataType::Varchar]);
        }
        Ok(arguments.to_vec())
    }
    fn bind(
        &self,
        arguments: &dyn super::ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if arguments.len() == 1 && matches!(self.0, "hex" | "to_hex") {
            return Ok(Some(Arc::new(Self(self.0, Some(arguments.data_type(0)?)))));
        }
        Ok(None)
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let [input] = arguments else {
            return Err(Error::Bind(format!("{} requires one argument", self.0)));
        };
        let (supported, output) = match self.0 {
            "encode" | "unhex" | "from_hex" => (*input == DataType::Varchar, DataType::Blob),
            "decode" => (*input == DataType::Blob, DataType::Varchar),
            "octet_length" => (*input == DataType::Blob, DataType::BigInt),
            "hex" | "to_hex" => (
                matches!(input, DataType::Varchar | DataType::Blob) || input.is_integer(),
                DataType::Varchar,
            ),
            _ => return Err(Error::Internal("unknown binary scalar function".into())),
        };
        if supported || *input == DataType::Null {
            Ok(output)
        } else {
            Err(Error::Bind(format!("no overload for {}({input})", self.0)))
        }
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [input] = arguments else {
            return Err(Error::Internal("binary function argument count".into()));
        };
        if input.is_null() {
            return Ok(Value::Null);
        }
        match (self.0, input) {
            ("encode", Value::Varchar(text)) => Ok(Value::Blob(text.as_bytes().to_vec())),
            ("decode", Value::Blob(bytes)) => String::from_utf8(bytes.clone())
                .map(Value::Varchar)
                .map_err(|_| {
                    Error::Conversion(
                        "Failure in decode: could not convert BLOB to UTF8 string".into(),
                    )
                }),
            ("octet_length", Value::Blob(bytes)) => Ok(Value::Integer(bytes.len() as i128)),
            ("hex" | "to_hex", Value::Blob(bytes)) => encode_hex(bytes, query).map(Value::Varchar),
            ("hex" | "to_hex", Value::Varchar(text)) => {
                encode_hex(text.as_bytes(), query).map(Value::Varchar)
            }
            ("hex" | "to_hex", Value::Integer(value)) => Ok(Value::Varchar(
                if *value < 0 && self.1 != Some(DataType::HugeInt) {
                    format!("{:X}", *value as u64)
                } else {
                    format!("{value:X}")
                },
            )),
            ("hex" | "to_hex", Value::Unsigned(value)) => Ok(Value::Varchar(format!("{value:X}"))),
            ("unhex" | "from_hex", Value::Varchar(text)) => {
                decode_hex(text, query).map(Value::Blob)
            }
            _ => Err(Error::Internal(
                "binary function input differs from binding".into(),
            )),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encode_hex(bytes: &[u8], query: &QueryContext) -> Result<String> {
    let mut result = String::new();
    result
        .try_reserve(
            bytes
                .len()
                .checked_mul(2)
                .ok_or_else(|| Error::Resource("hex output overflow".into()))?,
        )
        .map_err(|_| Error::Resource("hex allocation failed".into()))?;
    let digits = b"0123456789ABCDEF";
    for (index, byte) in bytes.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        result.push(char::from(digits[usize::from(byte >> 4)]));
        result.push(char::from(digits[usize::from(byte & 15)]));
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decode_hex(text: &str, query: &QueryContext) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len().div_ceil(2));
    let invalid = || Error::InvalidInput("invalid hexadecimal input to unhex".into());
    let odd = bytes.len() % 2;
    if odd != 0 {
        result.push(hex_digit(bytes[0]).ok_or_else(invalid)?);
    }
    for (index, pair) in bytes[odd..].chunks_exact(2).enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        result.push(
            (hex_digit(pair[0]).ok_or_else(invalid)? << 4)
                | hex_digit(pair[1]).ok_or_else(invalid)?,
        );
    }
    Ok(result)
}

#[derive(Debug)]
struct BlobConcatenate;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for BlobConcatenate {
    fn name(&self) -> &'static str {
        "blob-concatenate"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        signature.operator == Operator::Concat
            && signature.arguments == [DataType::Blob, DataType::Blob]
            && signature.result == DataType::Blob
            && !signature.nullable
    }
    fn null_constant_type(&self, _: &OperatorSignature) -> Option<DataType> {
        Some(DataType::Null)
    }
    fn evaluate(
        &self,
        _: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        let [Value::Blob(a), Value::Blob(b)] = arguments else {
            return Err(Error::Internal("blob concatenation arguments".into()));
        };
        let size = a
            .len()
            .checked_add(b.len())
            .filter(|size| *size <= 16 * 1024 * 1024)
            .ok_or_else(|| Error::Resource("blob concatenation exceeds 16 MiB".into()))?;
        let mut bytes = Vec::with_capacity(size);
        for chunk in a.chunks(4096).chain(b.chunks(4096)) {
            query.check()?;
            bytes.extend_from_slice(chunk);
        }
        Ok(Value::Blob(bytes))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register_operators(registry: &mut OperatorRegistry) {
    registry
        .register(
            OperatorSignature {
                operator: Operator::Concat,
                arguments: vec![DataType::Blob, DataType::Blob],
                result: DataType::Blob,
                nullable: false,
            },
            Arc::new(BlobConcatenate),
        )
        .expect("unique blob concatenate");
}
