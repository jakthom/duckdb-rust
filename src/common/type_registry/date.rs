use std::cmp::Ordering;

use super::{TypeAdapter, ValueValidation};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

/// DATE semantics are selected through the same contract as extension types.
#[derive(Debug)]
pub struct DateType;

impl TypeAdapter for DateType {
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn name(&self) -> &'static str {
        "gregorian-date"
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        if *data_type != DataType::Date {
            return Err(Error::Unsupported(
                "DATE adapter requires DATE metadata".into(),
            ));
        }
        Ok(())
    }
    fn validate_value(&self, _: &DataType, value: &Value, context: &QueryContext) -> Result<()> {
        context.check()?;
        value.as_date().map(|_| ())
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        Ok(matches!(
            (left, right),
            (DataType::Date, DataType::Date | DataType::Null) | (DataType::Null, DataType::Date)
        )
        .then_some(DataType::Date))
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        context: &QueryContext,
    ) -> Result<Ordering> {
        context.check()?;
        Ok(left.as_date()?.cmp(&right.as_date()?))
    }
    fn key(&self, _: &DataType, value: &Value, context: &QueryContext) -> Result<Vec<u8>> {
        context.check()?;
        Ok(value.as_date()?.days().to_le_bytes().to_vec())
    }
}
