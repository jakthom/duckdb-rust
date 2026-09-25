//! ISO-calendar and Julian-day extraction shared by the named functions and
//! the generic `date_part` adapters.
use std::sync::Arc;

use super::*;
use crate::{common::temporal::MICROS_PER_DAY, function::ScalarSignature};

const JULIAN_EPOCH: i64 = 2_440_588;

#[derive(Clone, Copy, Debug)]
enum CalendarPart {
    Era,
    IsoYear,
    Week,
    Weekday,
    YearWeek,
    Julian,
}

#[derive(Debug)]
struct CalendarExtract {
    name: &'static str,
    part: CalendarPart,
    signature: Option<ScalarSignature>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, part) in [
        ("era", CalendarPart::Era),
        ("isoyear", CalendarPart::IsoYear),
        ("week", CalendarPart::Week),
        ("weekofyear", CalendarPart::Week),
        ("weekday", CalendarPart::Weekday),
        ("yearweek", CalendarPart::YearWeek),
        ("julian", CalendarPart::Julian),
    ] {
        registry
            .register_scalar(Arc::new(CalendarExtract {
                name,
                part,
                signature: None,
            }))
            .expect("unique calendar extraction function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CalendarPart {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "era" => Self::Era,
            "isoyear" => Self::IsoYear,
            "week" | "weeks" | "w" | "weekofyear" => Self::Week,
            "dow" | "dayofweek" | "weekday" => Self::Weekday,
            "yearweek" => Self::YearWeek,
            "julian" | "jd" => Self::Julian,
            _ => return None,
        })
    }

    fn result_type(self) -> DataType {
        if matches!(self, Self::Julian) {
            DataType::Double
        } else {
            DataType::BigInt
        }
    }

    fn interval_name(self) -> &'static str {
        match self {
            Self::Era => "era",
            Self::IsoYear => "isoyear",
            Self::Week => "week",
            Self::Weekday => "dow",
            Self::YearWeek => "week",
            Self::Julian => "julian",
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn candidates(part: CalendarPart) -> Vec<ScalarSignature> {
    let types: &[DataType] = if matches!(part, CalendarPart::Julian) {
        &[DataType::Date, DataType::Timestamp]
    } else {
        &[DataType::Date, DataType::Timestamp, DataType::Interval]
    };
    types
        .iter()
        .map(|data_type| ScalarSignature {
            arguments: vec![data_type.clone()],
            argument_names: None,
            return_type: part.result_type(),
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CalendarExtract {
    fn signature(&self) -> Result<&ScalarSignature> {
        self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported(format!(
                "{} requires selected statement-local binding",
                self.name
            ))
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for CalendarExtract {
    fn name(&self) -> &str {
        self.name
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let candidates = candidates(self.part);
        ScalarSignature::validate_candidates(self.name, &candidates, query)?;
        let selected = arguments.select_overload(self.name, &candidates)?;
        let signature = ScalarSignature::selected(&candidates, selected)?.clone();
        if signature.arguments.len() != arguments.len() {
            return Err(Error::Internal(
                "calendar extraction overload changed argument count".into(),
            ));
        }
        Ok(Some(Arc::new(Self {
            name: self.name,
            part: self.part,
            signature: Some(signature),
        })))
    }

    fn argument_types(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        let signature = self.signature()?;
        if arguments.len() != 1 {
            return Err(Error::Bind(format!("{} requires one argument", self.name)));
        }
        Ok(signature.arguments.clone())
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let signature = self.signature()?;
        if arguments != signature.arguments {
            return Err(Error::Bind(format!(
                "{} arguments differ from the selected overload",
                self.name
            )));
        }
        Ok(signature.return_type.clone())
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let signature = self.signature()?;
        let [value] = arguments else {
            return Err(Error::Internal(
                "calendar extraction argument count changed after binding".into(),
            ));
        };
        if !value.is_null() && !value.fits_type(&signature.arguments[0]) {
            return Err(Error::Internal(
                "calendar extraction input was not coerced".into(),
            ));
        }
        if value.is_null() {
            return Ok(Value::Null);
        }
        extract_part(value, self.part)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn is_part(text: &str) -> bool {
    CalendarPart::parse(text).is_some()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn returns_double(text: &str) -> bool {
    matches!(CalendarPart::parse(text), Some(CalendarPart::Julian))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn extract(value: &Value, text: &str) -> Option<Result<Value>> {
    CalendarPart::parse(text).map(|part| extract_part(value, part))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn extract_part(value: &Value, part: CalendarPart) -> Result<Value> {
    if matches!(value, Value::Temporal(TemporalValue::Interval { .. })) {
        return Err(Error::NotImplemented(format!(
            "interval units \"{}\" not recognized",
            part.interval_name()
        )));
    }
    let date = match value {
        Value::Date(date) => *date,
        Value::Temporal(value) if value.data_type() == DataType::Timestamp => value.date()?,
        _ => {
            return Err(Error::Internal(
                "calendar extraction received an unbound input".into(),
            ));
        }
    };
    if !date.is_finite() {
        return Ok(Value::Null);
    }
    if matches!(part, CalendarPart::Julian) {
        let mut result = f64::from(date.days()) + JULIAN_EPOCH as f64;
        if let Value::Temporal(TemporalValue::Timestamp(ticks)) = value {
            result += ticks.rem_euclid(MICROS_PER_DAY) as f64 / MICROS_PER_DAY as f64;
        }
        return Ok(Value::Double(result));
    }
    let result = match part {
        CalendarPart::Era => {
            if date.to_ymd().expect("finite date").0 > 0 {
                1
            } else {
                0
            }
        }
        CalendarPart::Weekday => (i64::from(date.days()) + 4).rem_euclid(7),
        CalendarPart::IsoYear | CalendarPart::Week | CalendarPart::YearWeek => {
            let (year, week) = iso_year_week(date)?;
            match part {
                CalendarPart::IsoYear => i64::from(year),
                CalendarPart::Week => week,
                CalendarPart::YearWeek => {
                    i64::from(year) * 100 + if year > 0 { week } else { -week }
                }
                _ => unreachable!(),
            }
        }
        CalendarPart::Julian => unreachable!(),
    };
    Ok(Value::Integer(i128::from(result)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn iso_week_one(year: i32) -> Result<i64> {
    let january_first = i64::from(Date::from_ymd(year, 1, 1)?.days());
    let weekday = (january_first + 3).rem_euclid(7);
    Ok(january_first - weekday + if weekday > 3 { 7 } else { 0 })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn iso_year_week(date: Date) -> Result<(i32, i64)> {
    let mut year = date
        .to_ymd()
        .ok_or_else(|| Error::Internal("finite ISO calendar date".into()))?
        .0;
    let days = i64::from(date.days());
    let mut week = (days - iso_week_one(year)?).div_euclid(7);
    if week < 0 {
        year = year
            .checked_sub(1)
            .ok_or_else(|| Error::Conversion("ISO year outside calendar range".into()))?;
        week = (days - iso_week_one(year)?).div_euclid(7);
    } else if week >= 52 {
        let next_year = year
            .checked_add(1)
            .ok_or_else(|| Error::Conversion("ISO year outside calendar range".into()))?;
        if days >= iso_week_one(next_year)? {
            year = next_year;
            week = 0;
        }
    }
    Ok((year, week + 1))
}
