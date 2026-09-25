use std::cmp::Ordering;

use super::{KeyWriter, TypeAdapter, ValueValidation};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct EnumTypes;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for EnumTypes {
    fn name(&self) -> &'static str {
        "ordered-enum-types"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        match data_type {
            DataType::Enum(metadata) => metadata.validate(),
            _ => Err(Error::Bind(
                "ENUM adapter requires ordered label metadata".into(),
            )),
        }
    }
    fn validate_value(&self, _: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        query.check()?;
        match value {
            Value::Enum(value) => value.label().map(|_| ()),
            _ => Err(Error::Conversion("ENUM physical value required".into())),
        }
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        match (left, right) {
            (DataType::Enum(_), DataType::Enum(_)) if left == right => Ok(Some(left.clone())),
            (DataType::Enum(_), DataType::Enum(_) | DataType::Varchar)
            | (DataType::Varchar, DataType::Enum(_)) => Ok(Some(DataType::Varchar)),
            _ => Ok(None),
        }
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        query.check()?;
        match (left, right) {
            (Value::Enum(a), Value::Enum(b)) => Ok(a.ordinal.cmp(&b.ordinal)),
            _ => Err(Error::Internal("ENUM comparison physical values".into())),
        }
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        match value {
            Value::Enum(value) => output.extend_from_slice(&value.ordinal.to_be_bytes()),
            _ => Err(Error::Internal("ENUM key physical value".into())),
        }
    }
}
