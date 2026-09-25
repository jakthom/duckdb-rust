use std::cmp::Ordering;

use super::{KeyWriter, TypeAdapter, ValueValidation};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct BitType;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for BitType {
    fn name(&self) -> &'static str {
        "packed-bit-type"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        if *data_type == DataType::Bit {
            Ok(())
        } else {
            Err(Error::Bind("BIT adapter requires BIT type".into()))
        }
    }
    fn validate_value(&self, _: &DataType, _: &Value, query: &QueryContext) -> Result<()> {
        query.check()
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        Ok(DataType::common(left, right).ok())
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        query.check()?;
        left.compare(right)
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        let Value::Bit(value) = value else {
            return Err(Error::Internal("invalid BIT key input".into()));
        };
        output.extend_from_slice(&(value.length() as u64).to_be_bytes())?;
        output.extend_from_slice(value.bytes())
    }
}
