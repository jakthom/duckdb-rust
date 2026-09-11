use std::sync::Arc;
mod base64;
mod codec;

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
        "bin",
        "to_binary",
        "unbin",
        "from_binary",
        "unhex",
        "from_hex",
        "base64",
        "to_base64",
        "from_base64",
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
        if arguments.len() == 1 {
            match self.0 {
                "base64" | "to_base64" => return Ok(vec![DataType::Blob]),
                "from_base64" | "unbin" | "from_binary" => {
                    return Ok(vec![DataType::Varchar]);
                }
                _ => (),
            }
        }
        if self.0 == "decode" {
            return match arguments.len() {
                1 => Ok(vec![DataType::Blob]),
                2 => Ok(vec![DataType::Blob, DataType::Varchar]),
                _ => Err(Error::Bind("decode requires one or two arguments".into())),
            };
        }
        if matches!(
            self.0,
            "encode"
                | "unbin"
                | "from_binary"
                | "unhex"
                | "from_hex"
                | "hex"
                | "to_hex"
                | "bin"
                | "to_binary"
        ) && arguments.len() == 1
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
        if arguments.len() == 1 && matches!(self.0, "hex" | "to_hex" | "bin" | "to_binary") {
            return Ok(Some(Arc::new(Self(self.0, Some(arguments.data_type(0)?)))));
        }
        Ok(None)
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if self.0 == "decode" {
            return match arguments {
                [DataType::Blob] | [DataType::Blob, DataType::Varchar] => Ok(DataType::Varchar),
                _ => Err(Error::Bind("no overload for decode".into())),
            };
        }
        let [input] = arguments else {
            return Err(Error::Bind(format!("{} requires one argument", self.0)));
        };
        let (supported, output) = match self.0 {
            "encode" | "unbin" | "from_binary" | "unhex" | "from_hex" | "from_base64" => {
                (*input == DataType::Varchar, DataType::Blob)
            }
            "base64" | "to_base64" => (*input == DataType::Blob, DataType::Varchar),
            "octet_length" => (
                matches!(input, DataType::Blob | DataType::Bit),
                DataType::BigInt,
            ),
            "hex" | "to_hex" => (
                matches!(input, DataType::Varchar | DataType::Blob | DataType::Bignum)
                    || input.is_integer(),
                DataType::Varchar,
            ),
            "bin" | "to_binary" => (
                matches!(input, DataType::Varchar | DataType::Bignum) || input.is_integer(),
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
        if self.0 == "decode" {
            return match arguments {
                [Value::Null] | [_, Value::Null] | [Value::Null, _] => Ok(Value::Null),
                [Value::Blob(bytes)] => codec::decode_utf8(bytes, None, query).map(Value::Varchar),
                [Value::Blob(bytes), Value::Varchar(specifier)] => {
                    codec::decode_utf8(bytes, Some(specifier), query).map(Value::Varchar)
                }
                _ => Err(Error::Internal("decode input differs from binding".into())),
            };
        }
        let [input] = arguments else {
            return Err(Error::Internal("binary function argument count".into()));
        };
        if input.is_null() {
            return Ok(Value::Null);
        }
        match (self.0, input) {
            ("base64" | "to_base64", Value::Blob(bytes)) => {
                base64::encode(bytes, query).map(Value::Varchar)
            }
            ("from_base64", Value::Varchar(text)) => base64::decode(text, query).map(Value::Blob),
            ("encode", Value::Varchar(text)) => Ok(Value::Blob(text.as_bytes().to_vec())),
            ("unbin" | "from_binary", Value::Varchar(text)) => {
                codec::decode_binary(text, query).map(Value::Blob)
            }
            ("octet_length", Value::Blob(bytes)) => Ok(Value::Integer(bytes.len() as i128)),
            ("octet_length", Value::Bit(bits)) => Ok(Value::Integer(bits.bytes().len() as i128)),
            ("hex" | "to_hex", Value::Blob(bytes)) => encode_hex(bytes, query).map(Value::Varchar),
            ("hex" | "to_hex", Value::Bignum(value)) => {
                encode_hex(&value.to_native(|| query.check())?, query).map(Value::Varchar)
            }
            ("bin" | "to_binary", Value::Bignum(value)) => {
                encode_binary(&value.to_native(|| query.check())?, query).map(Value::Varchar)
            }
            ("bin" | "to_binary", Value::Varchar(value)) => {
                encode_binary(value.as_bytes(), query).map(Value::Varchar)
            }
            ("bin" | "to_binary", Value::Integer(value)) => Ok(Value::Varchar(
                if *value < 0 && self.1 != Some(DataType::HugeInt) {
                    format!("{:b}", *value as u64)
                } else {
                    format!("{value:b}")
                },
            )),
            ("bin" | "to_binary", Value::Unsigned(value)) => {
                Ok(Value::Varchar(format!("{value:b}")))
            }
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
fn encode_binary(bytes: &[u8], query: &QueryContext) -> Result<String> {
    let mut text = String::new();
    text.try_reserve_exact(
        bytes
            .len()
            .checked_mul(8)
            .ok_or_else(|| Error::Resource("binary text length overflow".into()))?,
    )
    .map_err(|_| Error::Resource("cannot allocate binary text".into()))?;
    for (index, byte) in bytes.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        for bit in (0..8).rev() {
            text.push(if byte & (1 << bit) == 0 { '0' } else { '1' });
        }
    }
    Ok(text)
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
