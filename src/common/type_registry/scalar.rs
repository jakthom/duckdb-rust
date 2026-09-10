use std::cmp::Ordering;

use super::{KeyWriter, TypeAdapter, ValueValidation};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct BinaryScalarTypes;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for BinaryScalarTypes {
    fn name(&self) -> &'static str {
        "binary-scalar-types"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        if matches!(data_type, DataType::Blob | DataType::Uuid) {
            Ok(())
        } else {
            Err(Error::Bind(
                "binary scalar adapter requires BLOB or UUID".into(),
            ))
        }
    }
    fn validate_value(&self, _: &DataType, _: &Value, context: &QueryContext) -> Result<()> {
        context.check()
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        Ok(DataType::common(left, right).ok())
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        context: &QueryContext,
    ) -> Result<Ordering> {
        context.check()?;
        left.compare(right)
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        context: &QueryContext,
    ) -> Result<()> {
        context.check()?;
        match value {
            Value::Blob(bytes) => output.extend_from_slice(bytes),
            Value::Uuid(value) => output.extend_from_slice(&value.to_be_bytes()),
            _ => Err(Error::Internal("invalid binary scalar key".into())),
        }
    }
}
