//! Example registered logical type with two interchangeable implementations.
//! Payloads preserve ASCII spelling; equality and ordering ignore ASCII case.
use super::*;
use crate::common::cast::{CastFunction, CastMode, CastSpec};

pub const FAMILY: &str = "ascii_ci";

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn data_type(max_bytes: usize) -> Result<DataType> {
    let value = i64::try_from(max_bytes)
        .map_err(|_| Error::Bind("ASCII size parameter overflows".into()))?;
    let data_type = DataType::extension(FAMILY, vec![TypeParameter::Integer(value)]);
    limit(&data_type)?;
    Ok(data_type)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn limit(data_type: &DataType) -> Result<usize> {
    let DataType::Extension(identity) = data_type else {
        return Err(Error::Bind("ASCII type requires extension metadata".into()));
    };
    let super::super::TypeIdentity { name, parameters } = identity.as_ref();
    match parameters.as_slice() {
        [TypeParameter::Integer(n @ 1..=16_777_216)] if name == FAMILY => Ok(*n as usize),
        _ => Err(Error::Bind(
            "ascii_ci requires a byte limit from 1 to 16777216".into(),
        )),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bytes(value: &Value) -> Result<&[u8]> {
    let Value::Extension(value) = value else {
        return Err(Error::Conversion(
            "ASCII value requires an extension payload".into(),
        ));
    };
    Ok(&value.bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate(data_type: &DataType, value: &Value, context: &QueryContext) -> Result<()> {
    let bytes = bytes(value)?;
    if bytes.len() > limit(data_type)? {
        return Err(Error::Conversion(
            "ASCII value exceeds its declared byte limit".into(),
        ));
    }
    for chunk in bytes.chunks(1024) {
        context.check()?;
        if !chunk.is_ascii() {
            return Err(Error::Conversion(
                "ascii_ci accepts ASCII bytes only".into(),
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn folded(value: &Value, context: &QueryContext) -> Result<Vec<u8>> {
    let mut result = bytes(value)?.to_vec();
    for chunk in result.chunks_mut(1024) {
        context.check()?;
        chunk.make_ascii_lowercase();
    }
    Ok(result)
}

#[derive(Debug)]
pub struct MaterializedAscii;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for MaterializedAscii {
    fn name(&self) -> &'static str {
        "materialized-ascii-ci"
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        limit(data_type).map(|_| ())
    }
    fn validate_value(
        &self,
        data_type: &DataType,
        value: &Value,
        context: &QueryContext,
    ) -> Result<()> {
        validate(data_type, value, context)
    }
    fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
        Ok(None)
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        context: &QueryContext,
    ) -> Result<Ordering> {
        Ok(folded(left, context)?.cmp(&folded(right, context)?))
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        context: &QueryContext,
    ) -> Result<()> {
        output.extend_from_slice(&folded(value, context)?)
    }
}

#[derive(Debug)]
pub struct StreamingAscii;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for StreamingAscii {
    fn name(&self) -> &'static str {
        "streaming-ascii-ci"
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        limit(data_type).map(|_| ())
    }
    fn validate_value(
        &self,
        data_type: &DataType,
        value: &Value,
        context: &QueryContext,
    ) -> Result<()> {
        validate(data_type, value, context)
    }
    fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
        Ok(None)
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        context: &QueryContext,
    ) -> Result<Ordering> {
        let left = bytes(left)?;
        let right = bytes(right)?;
        for (i, (a, b)) in left.iter().zip(right).enumerate() {
            if i % 1024 == 0 {
                context.check()?;
            }
            let order = a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase());
            if order != Ordering::Equal {
                return Ok(order);
            }
        }
        Ok(left.len().cmp(&right.len()))
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        context: &QueryContext,
    ) -> Result<()> {
        for (i, byte) in bytes(value)?.iter().enumerate() {
            if i % 1024 == 0 {
                context.check()?;
            }
            output.push(byte.to_ascii_lowercase())?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct AsciiCast;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for AsciiCast {
    fn name(&self) -> &'static str {
        "ascii-ci-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.mode != CastMode::Implicit
            && ((spec.source == DataType::Varchar && spec.target.family() == FAMILY)
                || (spec.source.family() == FAMILY && spec.target == DataType::Varchar))
    }
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value> {
        context.check()?;
        match value {
            Value::Varchar(text) => {
                let value = Value::extension(spec.target.clone(), text.as_bytes().to_vec());
                validate(&spec.target, &value, context)?;
                Ok(value)
            }
            Value::Extension(value) => String::from_utf8(value.bytes.clone())
                .map(Value::Varchar)
                .map_err(|_| Error::Conversion("invalid ASCII payload".into())),
            _ => Err(Error::Conversion("ASCII cast source type".into())),
        }
    }
}
