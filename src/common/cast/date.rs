use super::{
    CastBehavior, CastFailure, CastFunction, CastMode, CastResult, CastSourceContext, CastSpec,
};
use crate::{
    common::{DataType, Date, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct DateCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
                crate::common::temporal::parse_date_cast_checked(text, &mut || context.check())
                    .map(Value::Date)
                    .map_err(|error| date_text_error(text, error, false))
            }
            (Value::Date(date), DataType::Varchar) => Ok(Value::Varchar(date.to_string())),
            _ => Err(Error::Conversion("invalid DATE cast input".into())),
        }
    }
    fn cast_attempt_with_context(
        &self,
        value: &Value,
        spec: &CastSpec,
        behavior: CastBehavior,
        source_context: CastSourceContext,
        query: &QueryContext,
    ) -> CastResult<Value> {
        if source_context == CastSourceContext::Variant
            && spec.target == DataType::Date
            && let Value::Varchar(text) = value
        {
            return Date::parse_strict_checked(text, || query.check())
                .map(Value::Date)
                .map_err(|error| CastFailure::from(date_text_error(text, error, true)));
        }
        self.cast_attempt(value, spec, behavior, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn date_text_error(text: &str, error: Error, variant: bool) -> Error {
    let Error::Conversion(message) = error else {
        return error;
    };
    let end = text.char_indices().nth(128).map_or(text.len(), |(i, _)| i);
    let tail = if end < text.len() { "..." } else { "" };
    let input = &text[..end];
    Error::Conversion(if variant {
        format!("Can't convert VARIANT(VARCHAR) value '{input}{tail}' to 'DATE'")
    } else if matches!(
        message.as_str(),
        "date field value out of range" | "DATE outside finite range"
    ) {
        format!("date field value out of range: \"{input}{tail}\"")
    } else {
        format!("invalid date field format: \"{input}{tail}\", expected format is (YYYY-MM-DD)")
    })
}
