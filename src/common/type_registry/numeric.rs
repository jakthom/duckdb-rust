use super::{KeyWriter, TypeAdapter, ValueValidation};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};
use std::cmp::Ordering;

/// Full-width unsigned values and parameterized fixed-point decimals. These
/// deliberately use canonical keys rather than the signed-integer shortcut.
#[derive(Debug)]
pub struct ExactNumericTypes;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for ExactNumericTypes {
    fn name(&self) -> &'static str {
        "exact-numeric-types"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        super::check_metadata(data_type)?;
        if !data_type.is_decimal() && !data_type.is_unsigned_integer() {
            return Err(Error::Bind(
                "exact numeric adapter requires decimal or unsigned metadata".into(),
            ));
        }
        Ok(())
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
        value.append_primitive_key(output)
    }
}

/// Independent decimal-digit ordering and textual equality keys. Useful for
/// checking interchange and comparison without native-width integer kernels.
#[derive(Debug)]
pub struct LexicalNumericTypes;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for LexicalNumericTypes {
    fn name(&self) -> &'static str {
        "lexical-numeric-types"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        ExactNumericTypes.validate_type(data_type)
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
        let a = coefficient_text(left)?;
        let b = coefficient_text(right)?;
        let an = a.starts_with('-');
        let bn = b.starts_with('-');
        if an != bn {
            return Ok(bn.cmp(&an));
        }
        let a = a.trim_start_matches('-');
        let b = b.trim_start_matches('-');
        let order = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
        Ok(if an { order.reverse() } else { order })
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        output.extend_from_slice(coefficient_text(value)?.as_bytes())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn coefficient_text(value: &Value) -> Result<String> {
    match value {
        Value::Unsigned(n) => Ok(n.to_string()),
        Value::Decimal { value, .. } => Ok(value.to_string()),
        _ => Err(Error::Internal("numeric comparison input".into())),
    }
}
