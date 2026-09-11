//! Calendar and duration truncation use the pinned core's distinct algorithms:
//! fixed TIMESTAMP units floor, while INTERVAL components truncate toward zero.
use super::{units::Unit, *};
use crate::{
    common::cast::CastMode,
    function::{ArgumentEvaluation, ArgumentProvenance, ScalarSignature},
};

#[derive(Debug)]
struct DateTruncation {
    name: &'static str,
    known_null: bool,
    signature: Option<ScalarSignature>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["date_trunc", "datetrunc"] {
        registry
            .register_scalar(Arc::new(DateTruncation {
                name,
                known_null: false,
                signature: None,
            }))
            .expect("unique calendar truncation function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn candidates() -> Vec<ScalarSignature> {
    use DataType::*;
    // Advertised extension-placeholder order is part of candidate diagnostics.
    // The ICU signature is metadata only; selecting it still rejects execution.
    [Date, Interval, Timestamp, TimestampTz]
        .into_iter()
        .map(|kind| ScalarSignature {
            return_type: if kind == Date {
                Timestamp
            } else {
                kind.clone()
            },
            arguments: vec![Varchar, kind],
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unsupported(statistics: bool) -> Error {
    Error::NotImplemented(format!(
        "Specifier type not implemented for DATETRUNC{}",
        if statistics { " statistics" } else { "" }
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for DateTruncation {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let candidates = candidates();
        ScalarSignature::validate_candidates(self.name, &candidates, query)?;
        let selected = arguments.select_overload(self.name, &candidates)?;
        let signature = ScalarSignature::selected(&candidates, selected)?.clone();
        if signature.arguments.len() != arguments.len() {
            return Err(Error::Internal("selected truncation overload arity".into()));
        }
        if signature.return_type == DataType::TimestampTz {
            return Err(Error::Unsupported(format!(
                "{} with time zones requires a selected ICU timezone adapter",
                self.name
            )));
        }
        let known_null = arguments.is_provably_null(0)? || arguments.is_provably_null(1)?;
        // The DATE/TIMESTAMP bind callback validates a closed part for its
        // statistics callback even in an unexecuted CASE branch. INTERVAL has
        // no such bind callback. Required evaluation failures remain fatal.
        if !known_null && signature.arguments[1] != DataType::Interval && arguments.is_closed(0)? {
            let part = arguments.constant_as(0, &DataType::Varchar, CastMode::Implicit)?;
            if let Value::Varchar(part) = part {
                if matches!(Unit::parse(&part)?, Unit::Unsupported) {
                    return Err(unsupported(true));
                }
            } else if !part.is_null() {
                return Err(Error::Internal(
                    "truncation bound part must be VARCHAR".into(),
                ));
            }
        }
        Ok(Some(Arc::new(Self {
            name: self.name,
            known_null,
            signature: Some(signature),
        })))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self.known_null {
            ArgumentEvaluation::TypeOnly
        } else {
            ArgumentEvaluation::NullOnConstant
        }
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        let signature = self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported("truncation requires selected overload binding".into())
        })?;
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Internal("bound truncation argument count".into()));
        }
        Ok(signature.arguments.clone())
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let signature = self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported("truncation requires selected overload binding".into())
        })?;
        if arguments != signature.arguments {
            return Err(Error::Internal("bound truncation signature changed".into()));
        }
        Ok(signature.return_type.clone())
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        self.evaluate_with_provenance(
            arguments,
            &vec![ArgumentProvenance::Unknown; arguments.len()],
            query,
        )
    }
    fn evaluate_with_provenance(
        &self,
        arguments: &[Value],
        provenance: &[ArgumentProvenance],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        if arguments.len() != provenance.len() {
            return Err(Error::Internal(
                "truncation argument provenance count".into(),
            ));
        }
        if self.known_null {
            if !arguments.is_empty() {
                return Err(Error::Internal(
                    "constant NULL truncation received arguments".into(),
                ));
            }
            return Ok(Value::Null);
        }
        let [part, value] = arguments else {
            return Err(Error::Internal("truncation argument count".into()));
        };
        if part.is_null() {
            return Ok(Value::Null);
        }
        let Value::Varchar(part) = part else {
            return Err(Error::Internal("truncation expected VARCHAR part".into()));
        };
        let constant = provenance[0] == ArgumentProvenance::Constant;
        let unit = if constant {
            let unit = Unit::parse(part)?;
            if matches!(unit, Unit::Unsupported) {
                return Err(unsupported(false));
            }
            Some(unit)
        } else {
            None
        };
        if value.is_null() {
            return Ok(Value::Null);
        }
        let unit = unit.map_or_else(|| Unit::parse(part), Ok)?;
        // Dynamic recognized-but-unsupported parts preserve infinite values;
        // unrecognized parts still fail before entering TruncateElement.
        match value {
            Value::Date(date) if !date.is_finite() => Ok(Value::Temporal(
                TemporalValue::Timestamp(if date.days() < 0 { -i64::MAX } else { i64::MAX }),
            )),
            Value::Temporal(temporal) if !temporal.is_finite() => Ok(value.clone()),
            _ if matches!(unit, Unit::Unsupported) => Err(unsupported(false)),
            Value::Date(date) => Ok(Value::Temporal(TemporalValue::Timestamp(
                timestamp_from_calendar(truncate_date(*date, unit)?, 0)?,
            ))),
            Value::Temporal(TemporalValue::Timestamp(ticks)) => {
                let ticks = if let Some(width) = fixed_width(unit) {
                    // The pinned development TruncFixed multiplies without an
                    // overflow check. Its measured lower-bound modular result
                    // is explicit here, never Rust debug overflow or UB.
                    ticks.div_euclid(width).wrapping_mul(width)
                } else {
                    timestamp_from_calendar(
                        truncate_date(TemporalValue::Timestamp(*ticks).date()?, unit)?,
                        0,
                    )?
                };
                Ok(Value::Temporal(TemporalValue::Timestamp(ticks)))
            }
            Value::Temporal(TemporalValue::Interval {
                months,
                days,
                micros,
            }) => Ok(Value::Temporal(truncate_interval(
                *months, *days, *micros, unit,
            ))),
            _ => Err(Error::Internal(
                "truncation expected bound temporal value".into(),
            )),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixed_width(unit: Unit) -> Option<i64> {
    Some(match unit {
        Unit::Microsecond => 1,
        Unit::Millisecond => 1_000,
        Unit::Second => 1_000_000,
        Unit::Minute => 60_000_000,
        Unit::Hour => 3_600_000_000,
        Unit::Day => MICROS_PER_DAY,
        _ => return None,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn calendar_date(year: i32, month: u8, day: u8) -> Result<Date> {
    Date::from_ymd(year, month, day)
        .map_err(|_| Error::Conversion(format!("Date out of range: {year}-{month}-{day}")))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn truncate_date(date: Date, unit: Unit) -> Result<Date> {
    let (year, month, _) = date
        .to_ymd()
        .ok_or_else(|| Error::Internal("finite truncation calendar".into()))?;
    match unit {
        Unit::Millennium => calendar_date(year / 1000 * 1000, 1, 1),
        Unit::Century => calendar_date(year / 100 * 100, 1, 1),
        Unit::Decade => calendar_date(year / 10 * 10, 1, 1),
        Unit::Year => calendar_date(year, 1, 1),
        Unit::Quarter => calendar_date(year, (month - 1) / 3 * 3 + 1, 1),
        Unit::Month => calendar_date(year, month, 1),
        Unit::Week | Unit::IsoYear => {
            let monday = i64::from(date.days()) - (i64::from(date.days()) + 3).rem_euclid(7);
            let days = if matches!(unit, Unit::IsoYear) {
                // Thursday identifies the ISO year. Its first week contains
                // January 4, including negative/proleptic Gregorian years.
                let thursday = Date::from_days(
                    i32::try_from(monday + 3)
                        .map_err(|_| Error::OutOfRange("Date out of range".into()))?,
                )?;
                let iso_year = thursday
                    .to_ymd()
                    .ok_or_else(|| Error::OutOfRange("Date out of range".into()))?
                    .0;
                let jan4 = i64::from(calendar_date(iso_year, 1, 4)?.days());
                jan4 - (jan4 + 3).rem_euclid(7)
            } else {
                monday
            };
            Date::from_days(
                i32::try_from(days).map_err(|_| Error::OutOfRange("Date out of range".into()))?,
            )
        }
        _ if fixed_width(unit).is_some() => Ok(date),
        _ => Err(unsupported(false)),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn truncate_interval(mut months: i32, mut days: i32, mut micros: i64, unit: Unit) -> TemporalValue {
    let month_width = match unit {
        Unit::Millennium => Some(12000),
        Unit::Century => Some(1200),
        Unit::Decade => Some(120),
        Unit::Year | Unit::IsoYear => Some(12),
        Unit::Quarter => Some(3),
        Unit::Month => Some(1),
        _ => None,
    };
    if let Some(width) = month_width {
        months = months / width * width;
        days = 0;
        micros = 0;
    } else if matches!(unit, Unit::Week) {
        days = days / 7 * 7;
        micros = 0;
    } else if matches!(unit, Unit::Day) {
        micros = 0;
    } else if let Some(width) = fixed_width(unit) {
        micros = micros / width * width;
    }
    TemporalValue::Interval {
        months,
        days,
        micros,
    }
}
