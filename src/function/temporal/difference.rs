//! Pinned core period crossings and complete-period differences. DATE retains
//! its wide calendar domain where the reference does not construct TIMESTAMP.
use super::units::Unit;
use super::*;
use crate::common::type_registry::TypeRegistry;

#[derive(Debug)]
struct DateDifference {
    name: &'static str,
    complete: bool,
    known_null: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, complete) in [
        ("date_diff", false),
        ("datediff", false),
        ("date_sub", true),
        ("datesub", true),
    ] {
        registry
            .register_scalar(Arc::new(DateDifference {
                name,
                complete,
                known_null: false,
            }))
            .expect("unique calendar difference function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for DateDifference {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        if arguments.len() != 3 {
            return Ok(None);
        }
        query.check()?;
        let sources = (0..3)
            .map(|index| arguments.data_type(index))
            .collect::<Result<Vec<_>>>()?;
        self.argument_types(&sources, query.types())?;
        // Select a valid temporal signature before the speculative NULL probe.
        // String literals can enter temporal overloads, unlike typed VARCHAR
        // columns or parameters. The binder still retains each selected cast.
        for index in [1, 2] {
            if sources[index] == DataType::Varchar && !arguments.is_string_literal(index)? {
                return Err(no_overload(self.name));
            }
        }
        let mut known_null = false;
        for index in 0..3 {
            if arguments.is_provably_null(index)? {
                known_null = true;
                break;
            }
        }
        Ok(Some(Arc::new(Self {
            name: self.name,
            complete: self.complete,
            known_null,
        })))
    }
    fn argument_evaluation(&self) -> super::super::ArgumentEvaluation {
        if self.known_null {
            super::super::ArgumentEvaluation::TypeOnly
        } else {
            super::super::ArgumentEvaluation::NullOnConstant
        }
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        _types: &TypeRegistry,
    ) -> Result<Vec<DataType>> {
        use DataType::*;
        let [part, left, right] = arguments else {
            return Err(no_overload(self.name));
        };
        if !matches!(part, Varchar | Null | Enum(_)) {
            return Err(no_overload(self.name));
        }
        let target = if [left, right]
            .iter()
            .any(|value| matches!(value, TimestampTz | TimestampTzNs))
        {
            return Err(Error::Unsupported(format!(
                "{} with time zones requires a selected ICU timezone adapter",
                self.name
            )));
        } else if [left, right]
            .iter()
            .any(|value| matches!(value, Timestamp | TimestampS | TimestampMs | TimestampNs))
        {
            Timestamp
        } else if *left == Date || *right == Date {
            Date
        } else if *left == Time || *right == Time {
            Time
        } else {
            return Err(no_overload(self.name));
        };
        for value in [left, right] {
            if !matches!(
                value,
                Null | Varchar | Date | Time | Timestamp | TimestampS | TimestampMs | TimestampNs
            ) {
                return Err(no_overload(self.name));
            }
            if matches!(value, Time | TimeNs | TimeTz) && *value != target {
                return Err(no_overload(self.name));
            }
        }
        Ok(vec![Varchar, target.clone(), target])
    }
    fn return_type(&self, arguments: &[DataType], _types: &TypeRegistry) -> Result<DataType> {
        match arguments {
            [DataType::Varchar, left, right]
                if left == right
                    && matches!(left, DataType::Date | DataType::Timestamp | DataType::Time) =>
            {
                Ok(DataType::BigInt)
            }
            _ => Err(no_overload(self.name)),
        }
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        self.evaluate_with_provenance(
            arguments,
            &vec![crate::function::ArgumentProvenance::Unknown; arguments.len()],
            query,
        )
    }
    fn evaluate_with_provenance(
        &self,
        arguments: &[Value],
        provenance: &[crate::function::ArgumentProvenance],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        if arguments.len() != provenance.len() {
            return Err(Error::Internal(
                "calendar difference argument provenance count".into(),
            ));
        }
        if self.known_null {
            if !arguments.is_empty() {
                return Err(Error::Internal(
                    "constant NULL difference received arguments".into(),
                ));
            }
            return Ok(Value::Null);
        }
        let [part, start, end] = arguments else {
            return Err(Error::Internal("calendar difference argument count".into()));
        };
        if part.is_null() {
            return Ok(Value::Null);
        }
        let Value::Varchar(part) = part else {
            return Err(Error::Internal(
                "calendar difference specifier must be bound VARCHAR".into(),
            ));
        };
        // Constant specifiers are dispatched before infinity checks in the core
        // binary executor. Dynamic specifiers are examined only for finite rows.
        let constant = (provenance[0] == crate::function::ArgumentProvenance::Constant)
            .then(|| self.unit(part))
            .transpose()?;
        if start.is_null() || end.is_null() {
            return Ok(Value::Null);
        }
        let finite = |value: &Value| match value {
            Value::Date(date) => Ok(date.is_finite()),
            Value::Temporal(value) => Ok(value.is_finite()),
            _ => Err(Error::Internal(
                "calendar difference expected temporal argument".into(),
            )),
        };
        if !finite(start)? || !finite(end)? {
            return Ok(Value::Null);
        }
        let unit = constant.map_or_else(|| self.unit(part), Ok)?;
        let value = match (start, end) {
            (Value::Date(start), Value::Date(end)) if !self.complete => {
                date_crossings(*start, *end, unit)?
            }
            (Value::Date(start), Value::Date(end)) => {
                complete_periods(calendar_timestamp(*start)?, calendar_timestamp(*end)?, unit)?
            }
            (
                Value::Temporal(TemporalValue::Timestamp(start)),
                Value::Temporal(TemporalValue::Timestamp(end)),
            ) => {
                if self.complete {
                    complete_periods(*start, *end, unit)?
                } else {
                    timestamp_crossings(*start, *end, unit)?
                }
            }
            (
                Value::Temporal(TemporalValue::Time(start)),
                Value::Temporal(TemporalValue::Time(end)),
            ) => {
                let Some(divisor) = unit.clock_divisor() else {
                    return Err(Error::Unsupported(format!(
                        "\"time\" units \"{}\" not recognized",
                        unit.name(self.complete)
                    )));
                };
                if self.complete {
                    subtract(*end, *start)? / divisor
                } else {
                    subtract(end / divisor, start / divisor)?
                }
            }
            _ => {
                return Err(Error::Internal(
                    "calendar difference mismatched bound types".into(),
                ));
            }
        };
        Ok(Value::Integer(i128::from(value)))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DateDifference {
    fn unit(&self, text: &str) -> Result<Unit> {
        let unit = Unit::parse(text)?;
        if matches!(unit, Unit::Unsupported) {
            return Err(Error::Unsupported(format!(
                "Specifier type not implemented for {}",
                if self.complete { "DATESUB" } else { "DATEDIFF" }
            )));
        }
        Ok(unit)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn no_overload(name: &str) -> Error {
    Error::Bind(format!(
        "No matching overload for {name}; add explicit DATE, TIMESTAMP or TIME casts"
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Unit {
    fn clock_divisor(self) -> Option<i64> {
        Some(match self {
            Self::Microsecond => 1,
            Self::Millisecond => 1000,
            Self::Second => 1_000_000,
            Self::Minute => 60_000_000,
            Self::Hour => 3_600_000_000,
            _ => return None,
        })
    }
    fn name(self, complete: bool) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
            Self::Day => "day",
            Self::Decade => "decade",
            Self::Century => "century",
            Self::Millennium => "millennium",
            Self::Quarter => "quarter",
            Self::Week => "week",
            Self::IsoYear if complete => "year",
            Self::IsoYear => "isoyear",
            Self::Microsecond => "microseconds",
            Self::Millisecond => "milliseconds",
            Self::Second => "second",
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Unsupported => "unsupported",
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn subtract(end: i64, start: i64) -> Result<i64> {
    end.checked_sub(start).ok_or_else(|| {
        Error::OutOfRange(format!(
            "Overflow in subtraction of INT64 ({end} - {start})!"
        ))
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn date_micros(date: Date) -> Result<i64> {
    i64::from(date.days())
        .checked_mul(MICROS_PER_DAY)
        .ok_or_else(|| {
            Error::Conversion(format!("Could not convert DATE ({date}) to microseconds"))
        })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_timestamp(date: Date) -> Result<i64> {
    i64::from(date.days())
        .checked_mul(MICROS_PER_DAY)
        .ok_or_else(|| Error::Conversion("Date and time not in timestamp range".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn date_crossings(start: Date, end: Date, unit: Unit) -> Result<i64> {
    let distance = i64::from(end.days()) - i64::from(start.days());
    match unit {
        Unit::Day => return Ok(distance),
        Unit::Week => return Ok(distance / 7),
        Unit::Microsecond => {
            let start = date_micros(start)?;
            let end = date_micros(end)?;
            return subtract(end, start);
        }
        // Preserve the pinned expression's end-before-start conversion order.
        Unit::Millisecond => return Ok(date_micros(end)? / 1000 - date_micros(start)? / 1000),
        Unit::Second => return Ok(distance * 86400),
        Unit::Minute => return Ok(distance * 1440),
        Unit::Hour => return Ok(distance * 24),
        _ => (),
    }
    let (sy, sm, _) = start
        .to_ymd()
        .ok_or_else(|| Error::Internal("finite difference start date".into()))?;
    let (ey, em, _) = end
        .to_ymd()
        .ok_or_else(|| Error::Internal("finite difference end date".into()))?;
    let (sy, ey) = (i64::from(sy), i64::from(ey));
    Ok(match unit {
        Unit::Year => ey - sy,
        Unit::Month => ey * 12 + i64::from(em) - sy * 12 - i64::from(sm),
        Unit::Quarter => (ey * 12 + i64::from(em) - 1) / 3 - (sy * 12 + i64::from(sm) - 1) / 3,
        Unit::Decade => ey / 10 - sy / 10,
        Unit::Century => ey / 100 - sy / 100,
        Unit::Millennium => ey / 1000 - sy / 1000,
        Unit::IsoYear => iso_year(end)? - iso_year(start)?,
        _ => return Err(Error::Internal("bound calendar difference unit".into())),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn iso_year(date: Date) -> Result<i64> {
    let (year, month, day) = date
        .to_ymd()
        .ok_or_else(|| Error::Internal("finite ISO year date".into()))?;
    let weekday = (i64::from(date.days()) + 3).rem_euclid(7) + 1;
    let thursday = i64::from(day) + 4 - weekday;
    Ok(i64::from(year)
        + if month == 1 && thursday < 1 {
            -1
        } else if month == 12 && thursday > 31 {
            1
        } else {
            0
        })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn timestamp_date(ticks: i64) -> Result<Date> {
    Date::from_days(
        i32::try_from(ticks.div_euclid(MICROS_PER_DAY))
            .map_err(|_| Error::Internal("timestamp date width".into()))?,
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn timestamp_crossings(start: i64, end: i64, unit: Unit) -> Result<i64> {
    if let Some(divisor) = unit.clock_divisor() {
        return subtract(end.div_euclid(divisor), start.div_euclid(divisor));
    }
    date_crossings(timestamp_date(start)?, timestamp_date(end)?, unit)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn complete_periods(start: i64, end: i64, unit: Unit) -> Result<i64> {
    if let Some(divisor) = unit.clock_divisor() {
        return Ok(subtract(end, start)? / divisor);
    }
    if matches!(unit, Unit::Day | Unit::Week) {
        return Ok(subtract(end, start)?
            / (MICROS_PER_DAY * if matches!(unit, Unit::Week) { 7 } else { 1 }));
    }
    let months = complete_months(start, end)?;
    Ok(match unit {
        Unit::Month => months,
        Unit::Quarter => months / 3,
        Unit::Year | Unit::IsoYear => months / 12,
        Unit::Decade => months / 120,
        Unit::Century => months / 1200,
        Unit::Millennium => months / 12000,
        _ => return Err(Error::Internal("bound complete-period unit".into())),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_parts(ticks: i64) -> Result<(i32, u8, u8, i64)> {
    let date = timestamp_date(ticks)?;
    let start = i64::from(date.days())
        .checked_mul(MICROS_PER_DAY)
        .ok_or_else(|| Error::Conversion("Date out of range in timestamp conversion".into()))?;
    let (year, month, day) = date
        .to_ymd()
        .ok_or_else(|| Error::Internal("finite timestamp calendar".into()))?;
    Ok((year, month, day, ticks - start))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn complete_months(start: i64, end: i64) -> Result<i64> {
    if start > end {
        return complete_months(end, start).map(|months| -months);
    }
    // The reference clips the earlier day only when the later date is the end
    // of a shorter month, then uses calendar age rather than a 30-day interval.
    let (ey, em, ed, et) = calendar_parts(end)?;
    let (sy, sm, sd, st) = calendar_parts(start)?;
    let last = match em {
        2 if ey % 4 == 0 && (ey % 100 != 0 || ey % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    let threshold = if ed == last { sd.min(last) } else { sd };
    let incomplete = ed < threshold || (ed == threshold && et < st);
    Ok(
        (i64::from(ey) - i64::from(sy)) * 12 + i64::from(em)
            - i64::from(sm)
            - i64::from(incomplete),
    )
}
