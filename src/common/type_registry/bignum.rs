use std::cmp::Ordering;

use super::{KeyWriter, TypeAdapter, ValueValidation};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct BignumType;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for BignumType {
    fn name(&self) -> &'static str {
        "magnitude-limb-bignum-type"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        if *data_type == DataType::Bignum {
            Ok(())
        } else {
            Err(Error::Bind("BIGNUM adapter requires BIGNUM type".into()))
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
        let (Value::Bignum(left), Value::Bignum(right)) = (left, right) else {
            return Err(Error::Internal("BIGNUM comparison input".into()));
        };
        left.compare(right, || query.check())
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        let Value::Bignum(value) = value else {
            return Err(Error::Internal("BIGNUM key input".into()));
        };
        output.extend_from_slice(&value.to_native(|| query.check())?)
    }
}
