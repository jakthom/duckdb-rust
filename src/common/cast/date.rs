use super::{CastFunction, CastMode, CastSpec};
use crate::{
    common::{DataType, Date, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct DateCast;

impl CastFunction for DateCast {
    fn name(&self) -> &'static str {
        "gregorian-date-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.mode != CastMode::Implicit
            && matches!(
                (&spec.source, &spec.target),
                (DataType::Date, DataType::Varchar) | (DataType::Varchar, DataType::Date)
            )
    }
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value> {
        context.check()?;
        match (value, &spec.target) {
            (Value::Varchar(text), DataType::Date) => {
                Date::parse_checked(text, || context.check()).map(Value::Date)
            }
            (Value::Date(date), DataType::Varchar) => Ok(Value::Varchar(date.to_string())),
            _ => Err(Error::Conversion("invalid DATE cast input".into())),
        }
    }
}
