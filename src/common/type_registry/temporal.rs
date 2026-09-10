use std::cmp::Ordering;

use super::{KeyWriter, TypeAdapter, ValueValidation};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct TemporalType;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for TemporalType {
    fn supports_index(&self, data_type: &DataType) -> bool {
        *data_type != DataType::Interval
    }
    fn name(&self) -> &'static str {
        "calendar-temporal"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        if data_type.is_temporal() {
            Ok(())
        } else {
            Err(Error::Bind("expected temporal metadata".into()))
        }
    }
    fn validate_value(&self, _: &DataType, value: &Value, context: &QueryContext) -> Result<()> {
        context.check()?;
        value.as_temporal()?.validate()
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        if left == right || *right == DataType::Null {
            return Ok(Some(left.clone()));
        }
        if *left == DataType::Null {
            return Ok(Some(right.clone()));
        }
        if left.timestamp_precision().is_some() && *right == DataType::Date {
            return Ok(Some(left.clone()));
        }
        if right.timestamp_precision().is_some() && *left == DataType::Date {
            return Ok(Some(right.clone()));
        }
        if let (Some(a), Some(b)) = (left.timestamp_precision(), right.timestamp_precision()) {
            if left.has_time_zone() || right.has_time_zone() {
                return Ok(Some(if a.max(b) == 1_000_000_000 {
                    DataType::TimestampTzNs
                } else {
                    DataType::TimestampTz
                }));
            }
            return Ok(Some(if a >= b { left.clone() } else { right.clone() }));
        }
        if matches!(
            (left, right),
            (DataType::Time, DataType::TimeNs) | (DataType::TimeNs, DataType::Time)
        ) {
            return Ok(Some(DataType::TimeNs));
        }
        Ok(None)
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        context: &QueryContext,
    ) -> Result<Ordering> {
        context.check()?;
        left.as_temporal()?.compare(right.as_temporal()?)
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        context: &QueryContext,
    ) -> Result<()> {
        context.check()?;
        output.extend_from_slice(&value.as_temporal()?.comparison_key().to_le_bytes())
    }
}
