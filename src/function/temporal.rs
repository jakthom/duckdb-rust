//! Core calendar and epoch functions. Zone-aware ICU operations are not silently
//! emulated by consulting the machine's local timezone.
use super::{ArgumentEvaluation, FunctionRegistry, ScalarBindArguments, ScalarFunction};
use crate::{
    common::{
        DataType, Date, Error, Result, TemporalValue, Value,
        temporal::{MICROS_PER_DAY, timestamp_from_calendar},
    },
    parallel::QueryContext,
};
use std::sync::Arc;
mod bucket;
mod calendar_extract;
mod difference;
mod epoch_extra;
mod formatting;
mod parsing;
mod timezone_core;
mod truncation;
mod units;

#[derive(Debug)]
struct TemporalFunction {
    name: &'static str,
    part: Option<String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    difference::register(registry);
    bucket::register(registry);
    calendar_extract::register(registry);
    epoch_extra::register(registry);
    truncation::register(registry);
    formatting::register(registry);
    parsing::register(registry);
    timezone_core::register(registry);
    for name in [
        "year",
        "month",
        "day",
        "dayofmonth",
        "hour",
        "minute",
        "second",
        "microsecond",
        "millisecond",
        "nanosecond",
        "timetz_byte_comparable",
        "quarter",
        "dayofyear",
        "dayofweek",
        "isodow",
        "century",
        "decade",
        "millennium",
        "epoch",
        "epoch_ms",
        "epoch_us",
        "epoch_ns",
        "isfinite",
        "isinf",
        "make_date",
        "make_time",
        "make_timestamp",
        "make_timestamp_ms",
        "make_timestamp_ns",
        "to_years",
        "to_centuries",
        "to_decades",
        "to_millennia",
        "to_quarters",
        "to_weeks",
        "to_months",
        "to_days",
        "to_hours",
        "to_minutes",
        "to_seconds",
        "to_milliseconds",
        "to_microseconds",
        "date_part",
        "datepart",
        "last_day",
        "dayname",
        "monthname",
    ] {
        registry
            .register_scalar(Arc::new(TemporalFunction { name, part: None }))
            .expect("unique temporal function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn invalid(message: &str) -> Error {
    Error::Conversion(message.into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for TemporalFunction {
    fn name(&self) -> &str {
        self.name
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if matches!(self.name, "date_part" | "datepart") {
            ArgumentEvaluation::NullOnConstant
        } else {
            ArgumentEvaluation::Eager
        }
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        _query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        if matches!(self.name, "date_part" | "datepart") && arguments.len() == 2 {
            return match arguments.constant(0) {
                Ok(Value::Varchar(part)) => {
                    // Core NULL handling replaces a provably NULL temporal
                    // argument before the constant-specifier bind callback.
                    // Keep the generic DOUBLE result and do not validate a
                    // specifier whose value cannot be demanded.
                    if arguments.is_provably_null(1)? {
                        return Ok(Some(Arc::new(Self {
                            name: self.name,
                            part: None,
                        })));
                    }
                    units::Unit::parse(&part)?;
                    Ok(Some(Arc::new(Self {
                        name: self.name,
                        part: Some(part.to_ascii_lowercase()),
                    })))
                }
                Ok(Value::Null) | Err(Error::Bind(_) | Error::Unsupported(_)) => Ok(None),
                Ok(_) => Err(Error::Bind("date_part specifier must be VARCHAR".into())),
                Err(error) => Err(error),
            };
        }
        Ok(None)
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        use DataType::*;
        let mut types = match (self.name, arguments.len()) {
            ("make_date", 3) => vec![BigInt; 3],
            ("make_date", 1) => vec![Integer],
            ("make_time", 3) => vec![BigInt, BigInt, Double],
            ("make_timestamp", 6) => vec![BigInt, BigInt, BigInt, BigInt, BigInt, Double],
            ("make_timestamp" | "make_timestamp_ms" | "make_timestamp_ns", 1) => vec![BigInt],
            (
                "to_years" | "to_centuries" | "to_decades" | "to_millennia" | "to_quarters"
                | "to_weeks" | "to_months" | "to_days" | "to_hours" | "to_minutes"
                | "to_microseconds",
                1,
            ) => vec![BigInt],
            ("to_seconds" | "to_milliseconds", 1) => vec![Double],
            ("epoch_ms", 1) if arguments[0].is_signed_integer() => vec![BigInt],
            ("timetz_byte_comparable", 1) if arguments[0] == Null => vec![TimeTz],
            _ => arguments.to_vec(),
        };
        if !self.name.starts_with("make_")
            && !self.name.starts_with("to_")
            && let Some(last) = types.last_mut()
        {
            // Core overloads use microsecond TIMESTAMP; NS has an exact
            // overload for epoch_ns and nanosecond. Coercion happens before evaluation.
            if matches!(last, TimestampS | TimestampMs)
                || (*last == TimestampNs && !matches!(self.name, "epoch_ns" | "nanosecond"))
            {
                *last = Timestamp;
            }
            if matches!(self.name, "isfinite" | "isinf") && last.is_numeric() && *last != Float {
                *last = Double;
            }
        }
        if matches!(self.name, "date_part" | "datepart") && types.len() == 2 {
            // An untyped second NULL is ambiguous across the DATE, TIMESTAMP,
            // TIME and INTERVAL overloads. Once that overload is fixed by a
            // typed temporal argument, the first NULL belongs to VARCHAR.
            if types[1] == Null {
                return Err(Error::Bind(format!(
                    "no overload for {}({arguments:?})",
                    self.name
                )));
            }
            if types[0] == Null
                && matches!(
                    types[1],
                    Date | Timestamp | Time | TimeNs | TimeTz | Interval
                )
            {
                types[0] = Varchar;
            }
        }
        Ok(types)
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        use DataType::*;
        let result = match (self.name, arguments) {
            ("make_date", [BigInt, BigInt, BigInt]) => Date,
            ("make_date", [Integer]) => Date,
            ("make_time", [BigInt, BigInt, Double]) => Time,
            ("make_timestamp", [BigInt] | [BigInt, BigInt, BigInt, BigInt, BigInt, Double]) => {
                Timestamp
            }
            ("make_timestamp_ms", [BigInt]) => Timestamp,
            ("make_timestamp_ns", [BigInt]) => TimestampNs,
            (
                "to_years" | "to_centuries" | "to_decades" | "to_millennia" | "to_quarters"
                | "to_weeks" | "to_months" | "to_days" | "to_hours" | "to_minutes"
                | "to_microseconds",
                [BigInt],
            ) => Interval,
            ("to_seconds" | "to_milliseconds", [Double]) => Interval,
            ("epoch_ms", [BigInt]) => Timestamp,
            ("timetz_byte_comparable", [TimeTz]) => UBigInt,
            (
                "nanosecond",
                [
                    Date | Timestamp | TimestampNs | TimestampTz | TimestampTzNs | Time | TimeNs
                    | TimeTz | Interval,
                ],
            ) => BigInt,
            (
                "date_part" | "datepart",
                [
                    Varchar,
                    Date | Timestamp | Time | TimeNs | TimeTz | Interval,
                ],
            ) => {
                if self.part.is_none()
                    || self.part.as_deref() == Some("epoch")
                    || self
                        .part
                        .as_deref()
                        .is_some_and(calendar_extract::returns_double)
                {
                    Double
                } else {
                    BigInt
                }
            }
            ("isfinite" | "isinf", [Date | Timestamp | TimestampTz | Float | Double | Null]) => {
                Boolean
            }
            ("epoch", [Date | Timestamp | Time | TimeNs | TimeTz | Interval | Null]) => Double,
            ("epoch_ms" | "epoch_us" | "epoch_ns", [t])
                if matches!(
                    t,
                    Date | Timestamp | TimestampTz | Time | TimeNs | TimeTz | Interval | Null
                ) || (self.name == "epoch_ns" && matches!(t, TimestampNs | TimestampTzNs)) =>
            {
                BigInt
            }
            ("last_day", [Date | Timestamp | Null]) => Date,
            ("dayname" | "monthname", [Date | Timestamp | Null]) => Varchar,
            (name, [Date | Timestamp | Interval | Null]) if is_extract(name) => BigInt,
            (name, [Time | TimeNs | TimeTz]) if is_clock_extract(name) => BigInt,
            _ => {
                return Err(Error::Bind(format!(
                    "no overload for {}({arguments:?})",
                    self.name
                )));
            }
        };
        Ok(result)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if matches!(self.name, "isfinite" | "isinf") && arguments[0].data_type().is_numeric() {
            let value = arguments[0].as_f64()?;
            return Ok(Value::Boolean(if self.name == "isfinite" {
                value.is_finite()
            } else {
                value.is_infinite()
            }));
        }
        match self.name {
            "make_date" if arguments.len() == 1 => {
                return i32::try_from(arguments[0].as_i128()?)
                    .map_err(|_| invalid("date days range"))
                    .and_then(Date::from_days)
                    .map(Value::Date);
            }
            "make_date" => return make_date(arguments).map(Value::Date),
            "make_time" => {
                return make_time(arguments).map(|v| Value::Temporal(TemporalValue::Time(v)));
            }
            "make_timestamp" if arguments.len() == 6 => {
                let date = make_date(&arguments[..3])?;
                let clock = make_time(&arguments[3..])?;
                let micros = timestamp_from_calendar(date, clock)?;
                return TemporalValue::from_ticks(&DataType::Timestamp, micros)
                    .map(Value::Temporal);
            }
            "make_timestamp" | "make_timestamp_ns" | "make_timestamp_ms" => {
                let ticks = i64::try_from(arguments[0].as_i128()?)
                    .map_err(|_| invalid("timestamp range"))?;
                let ticks = if self.name == "make_timestamp_ms" {
                    ticks
                        .checked_mul(1000)
                        .ok_or_else(|| invalid("timestamp range"))?
                } else {
                    ticks
                };
                return TemporalValue::from_ticks(
                    if self.name == "make_timestamp_ns" {
                        &DataType::TimestampNs
                    } else {
                        &DataType::Timestamp
                    },
                    ticks,
                )
                .map(Value::Temporal);
            }
            name if name.starts_with("to_") => return interval_constructor(name, &arguments[0]),
            "epoch_ms" if arguments[0].data_type().is_signed_integer() => {
                let ticks = arguments[0]
                    .as_i128()?
                    .checked_mul(1000)
                    .and_then(|v| i64::try_from(v).ok())
                    .ok_or_else(|| invalid("epoch milliseconds range"))?;
                return TemporalValue::from_ticks(&DataType::Timestamp, ticks).map(Value::Temporal);
            }
            _ => (),
        }
        if self.name == "timetz_byte_comparable" {
            let Value::Temporal(value) = arguments[0] else {
                return Err(Error::Internal("expected bound TIMETZ input".into()));
            };
            let bits = value.packed_time_tz()?;
            // The core biases the UTC-adjusted clock by its maximum offset.
            // Widen before shifting so a future physical-domain experiment
            // cannot silently overflow the public UBIGINT result.
            let key = u128::from(bits) + ((u128::from(bits & 0xff_ffff) * 1_000_000) << 24);
            let key = u64::try_from(key)
                .map_err(|_| Error::OutOfRange("TIMETZ comparison key outside UBIGINT".into()))?;
            return Ok(Value::Unsigned(u128::from(key)));
        }
        let value = arguments
            .last()
            .ok_or_else(|| Error::Internal("temporal function input".into()))?;
        let finite = match value {
            Value::Date(date) => date.is_finite(),
            Value::Temporal(t) => t.is_finite(),
            _ => return Err(invalid("expected temporal value")),
        };
        if matches!(self.name, "isfinite" | "isinf") {
            return Ok(Value::Boolean(finite == (self.name == "isfinite")));
        }
        if !finite {
            return Ok(Value::Null);
        }
        if self.name == "nanosecond" {
            return nanosecond(value);
        }
        let part = if matches!(self.name, "date_part" | "datepart") {
            match &arguments[0] {
                Value::Varchar(part) => part.to_ascii_lowercase(),
                _ => return Err(invalid("date_part specifier must be VARCHAR")),
            }
        } else {
            self.name.into()
        };
        if part.starts_with("epoch") {
            return epoch(value, &part);
        }
        let result = extract(value, &part)?;
        if matches!(self.name, "date_part" | "datepart")
            && self.part.is_none()
            && let Value::Integer(value) = result
        {
            return Ok(Value::Double(value as f64));
        }
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nanosecond(value: &Value) -> Result<Value> {
    use TemporalValue::*;
    let result = match value {
        Value::Date(_) => 0,
        Value::Temporal(TimeNs(ticks)) => ticks % 60_000_000_000,
        Value::Temporal(value @ (TimestampNs(ticks) | TimestampTzNs(ticks))) => {
            // This overload uses checked calendar conversion, unlike the
            // microsecond overload's direct clock extraction. Its core
            // registration is non-fallible, so development exposes a fatal
            // INTERNAL category for a physically valid partial earliest day.
            value.check_text_renderable().map_err(|error| match error {
                Error::Conversion(message) => Error::Internal(format!(
                    "Scalar function \"\"nanosecond\"\" threw an execution error, but the function is not marked as fallible - the function must call SetFallible(). Error: {message}"
                )),
                error => error,
            })?;
            ticks.rem_euclid(60_000_000_000)
        }
        Value::Temporal(Timestamp(ticks) | TimestampTz(ticks)) => {
            ticks.rem_euclid(60_000_000) * 1000
        }
        Value::Temporal(Time(micros) | TimeTz { micros, .. } | Interval { micros, .. }) => {
            (micros % 60_000_000) * 1000
        }
        _ => return Err(Error::Internal("expected bound nanosecond input".into())),
    };
    Ok(Value::Integer(i128::from(result)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn make_date(arguments: &[Value]) -> Result<Date> {
    let year = i32::try_from(arguments[0].as_i128()?).map_err(|_| invalid("date year range"))?;
    let month = u8::try_from(arguments[1].as_i128()?).map_err(|_| invalid("date month range"))?;
    let day = u8::try_from(arguments[2].as_i128()?).map_err(|_| invalid("date day range"))?;
    Date::from_ymd(year, month, day)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn is_clock_extract(name: &str) -> bool {
    matches!(
        name,
        "hour" | "minute" | "second" | "microsecond" | "millisecond"
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn is_extract(name: &str) -> bool {
    is_clock_extract(name)
        || calendar_extract::is_part(name)
        || matches!(
            name,
            "year"
                | "month"
                | "day"
                | "dayofmonth"
                | "quarter"
                | "dayofyear"
                | "dayofweek"
                | "isodow"
                | "century"
                | "decade"
                | "millennium"
        )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn make_time(arguments: &[Value]) -> Result<i64> {
    let hour = arguments[0].as_i128()?;
    let minute = arguments[1].as_i128()?;
    let second = arguments[2].as_f64()?;
    if !(0..=24).contains(&hour) || !(0..60).contains(&minute) || !second.is_finite() {
        return Err(invalid("time fields outside range"));
    }
    // Development separately casts whole seconds and rounds their fraction.
    // IsValidTime permits leap second 60 and a carried microsecond component.
    let whole = if (0.0..=60.0).contains(&second) {
        second.trunc()
    } else {
        second.round_ties_even()
    };
    let fraction = ((second - whole) * 1_000_000.0).round();
    if !(0.0..=60.0).contains(&whole)
        || !(0.0..=1_000_000.0).contains(&fraction)
        || (hour == 24 && (minute != 0 || whole != 0.0 || fraction != 0.0))
    {
        return Err(invalid("time fields outside range"));
    }
    let micros = ((hour * 60 + minute) * 60 + whole as i128) * 1_000_000 + fraction as i128;
    Ok(micros as i64)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn interval_constructor(name: &str, value: &Value) -> Result<Value> {
    let (months, days, micros) = match name {
        "to_years" | "to_centuries" | "to_decades" | "to_millennia" | "to_quarters" => (
            value
                .as_i128()?
                .checked_mul(match name {
                    "to_centuries" => 1200,
                    "to_decades" => 120,
                    "to_millennia" => 12000,
                    "to_quarters" => 3,
                    _ => 12,
                })
                .ok_or_else(|| invalid("interval years overflow"))?,
            0,
            0,
        ),
        "to_months" => (value.as_i128()?, 0, 0),
        "to_days" => (0, value.as_i128()?, 0),
        "to_weeks" => (
            0,
            value
                .as_i128()?
                .checked_mul(7)
                .ok_or_else(|| invalid("interval weeks overflow"))?,
            0,
        ),
        "to_hours" => (
            0,
            0,
            value
                .as_i128()?
                .checked_mul(3_600_000_000)
                .ok_or_else(|| invalid("interval hours overflow"))?,
        ),
        "to_minutes" => (
            0,
            0,
            value
                .as_i128()?
                .checked_mul(60_000_000)
                .ok_or_else(|| invalid("interval minutes overflow"))?,
        ),
        "to_microseconds" => (0, 0, value.as_i128()?),
        "to_seconds" | "to_milliseconds" => {
            let value = value.as_f64()?
                * if name == "to_seconds" {
                    1_000_000.0
                } else {
                    1000.0
                };
            if !(i64::MIN as f64..-(i64::MIN as f64)).contains(&value) {
                return Err(invalid("interval fractional units overflow"));
            }
            (0, 0, value.round() as i128)
        }
        _ => return Err(invalid("unknown interval constructor")),
    };
    Ok(Value::Temporal(TemporalValue::Interval {
        months: i32::try_from(months).map_err(|_| invalid("interval months overflow"))?,
        days: i32::try_from(days).map_err(|_| invalid("interval days overflow"))?,
        micros: i64::try_from(micros).map_err(|_| invalid("interval micros overflow"))?,
    }))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn epoch(value: &Value, part: &str) -> Result<Value> {
    if part == "epoch_ms"
        && let Value::Temporal(t) = value
    {
        if t.data_type() == DataType::TimeNs {
            // The pinned core advertises this overload, but its execution
            // is an unimplemented cast; preserve NULL propagation above.
            return Err(invalid("core epoch_ms(TIME_NS) cast is unimplemented"));
        }
        if t.data_type().timestamp_precision().is_some() {
            return t
                .scale_timestamp(&DataType::TimestampMs)?
                .ticks()
                .map(|n| Value::Integer(i128::from(n)));
        }
    }
    if part != "epoch"
        && let Value::Temporal(TemporalValue::Interval {
            months,
            days,
            micros,
        }) = value
    {
        // Integer epoch units use 30-day months, unlike floating epoch's
        // 365.25-day years. Match checked component additions before scaling.
        let divisor = if part == "epoch_ms" { 1000 } else { 1 };
        let month = i64::from(*months)
            .checked_mul(MICROS_PER_DAY * 30 / divisor)
            .ok_or_else(|| invalid("interval epoch month overflow"))?;
        let day = i64::from(*days)
            .checked_mul(MICROS_PER_DAY / divisor)
            .ok_or_else(|| invalid("interval epoch day overflow"))?;
        let total = (micros / divisor)
            .checked_add(month)
            .and_then(|n| n.checked_add(day))
            .ok_or_else(|| invalid("interval epoch overflow"))?;
        let total = if part == "epoch_ns" {
            total
                .checked_mul(1000)
                .ok_or_else(|| invalid("interval epoch nanos overflow"))?
        } else {
            total
        };
        return Ok(Value::Integer(i128::from(total)));
    }
    let (ticks, precision) = match value {
        Value::Date(date) => (i128::from(date.days()) * 86400, 1),
        Value::Temporal(TemporalValue::Interval {
            months,
            days,
            micros,
        }) => {
            let total_days = i128::from(months / 12) * 36525
                + i128::from(months % 12) * 3000
                + i128::from(*days) * 100;
            (
                total_days * i128::from(MICROS_PER_DAY) / 100 + i128::from(*micros),
                1_000_000,
            )
        }
        Value::Temporal(TemporalValue::TimeTz { micros, .. }) => (i128::from(*micros), 1_000_000),
        Value::Temporal(TemporalValue::TimeNs(nanos)) if part != "epoch_ns" => {
            (i128::from(nanos / 1000), 1_000_000)
        }
        Value::Temporal(t) => (
            i128::from(t.ticks()?),
            t.data_type()
                .timestamp_precision()
                .unwrap_or(if t.data_type() == DataType::TimeNs {
                    1_000_000_000
                } else {
                    1_000_000
                }),
        ),
        _ => return Err(invalid("epoch requires temporal input")),
    };
    if part == "epoch" {
        return Ok(Value::Double(ticks as f64 / precision as f64));
    }
    let factor = match part {
        "epoch_ms" => 1000,
        "epoch_us" => 1_000_000,
        "epoch_ns" => 1_000_000_000,
        _ => return Err(invalid("unknown epoch unit")),
    };
    let result = ticks * factor / i128::from(precision);
    Ok(Value::Integer(i128::from(
        i64::try_from(result).map_err(|_| invalid("epoch unit overflow"))?,
    )))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn extract(value: &Value, part: &str) -> Result<Value> {
    if let Some(result) = calendar_extract::extract(value, part) {
        return result;
    }
    let (date, months, days, micros) = match value {
        Value::Date(date) => (Some(*date), 0, 0, 0),
        Value::Temporal(TemporalValue::Interval {
            months,
            days,
            micros,
        }) => (None, *months, *days, *micros),
        Value::Temporal(TemporalValue::TimeTz { micros, .. }) => (None, 0, 0, *micros),
        Value::Temporal(t) => {
            let timestamp = t.data_type().timestamp_precision().is_some();
            let precision = t.data_type().timestamp_precision().unwrap_or(
                if t.data_type() == DataType::TimeNs {
                    1_000_000_000
                } else {
                    1_000_000
                },
            );
            let clock = if timestamp {
                t.ticks()?.rem_euclid(precision * 86400)
            } else {
                t.ticks()?
            };
            (
                if timestamp { Some(t.date()?) } else { None },
                0,
                0,
                (i128::from(clock) * 1_000_000 / i128::from(precision)) as i64,
            )
        }
        _ => return Err(invalid("expected temporal input")),
    };
    let ymd = date.and_then(Date::to_ymd);
    let interval = value.data_type() == DataType::Interval;
    let result = match part {
        "year" | "years" => i64::from(if let Some((y, _, _)) = ymd {
            y
        } else if interval {
            months / 12
        } else {
            return Err(invalid("time has no year"));
        }),
        "month" | "months" => i64::from(if let Some((_, m, _)) = ymd {
            i32::from(m)
        } else if interval {
            months % 12
        } else {
            return Err(invalid("time has no month"));
        }),
        "day" | "days" | "dayofmonth" => i64::from(if let Some((_, _, d)) = ymd {
            i32::from(d)
        } else if interval {
            days
        } else {
            return Err(invalid("time has no day"));
        }),
        "hour" | "hours" => micros / 3_600_000_000,
        "minute" | "minutes" => micros / 60_000_000 % 60,
        "second" | "seconds" => micros / 1_000_000 % 60,
        "millisecond" | "milliseconds" => micros % 60_000_000 / 1000,
        "microsecond" | "microseconds" => micros % 60_000_000,
        "quarter" if interval => i64::from((months % 12) / 3 + 1),
        "century" if interval => i64::from(months / 1200),
        "decade" if interval => i64::from(months / 120),
        "millennium" if interval => i64::from(months / 12000),
        "timezone" | "timezone_hour" | "timezone_minute" => {
            let domain = match value {
                Value::Date(_) => Some("date"),
                Value::Temporal(TemporalValue::Interval { .. }) => Some("interval"),
                _ => None,
            };
            if let Some(domain) = domain {
                return Err(Error::NotImplemented(format!(
                    "\"{domain}\" units \"{part}\" not recognized"
                )));
            }
            let offset = match value {
                Value::Temporal(TemporalValue::TimeTz { offset, .. }) => i64::from(*offset),
                _ => 0,
            };
            match part {
                "timezone_hour" => offset / 3600,
                "timezone_minute" => offset / 60 % 60,
                _ => offset,
            }
        }
        _ => {
            let date = date.ok_or_else(|| invalid("date part requires calendar date"))?;
            let (year, month, day) =
                ymd.ok_or_else(|| invalid("date part requires finite date"))?;
            match part {
                "quarter" => i64::from((month - 1) / 3 + 1),
                "dayofyear" | "doy" => {
                    i64::from(date.days() - Date::from_ymd(year, 1, 1)?.days() + 1)
                }
                "dayofweek" | "dow" => (i64::from(date.days()) + 4).rem_euclid(7),
                "isodow" => (i64::from(date.days()) + 3).rem_euclid(7) + 1,
                "century" => {
                    if year > 0 {
                        i64::from((year - 1) / 100 + 1)
                    } else {
                        i64::from(year / 100 - 1)
                    }
                }
                "decade" => i64::from(year / 10),
                "millennium" => {
                    if year > 0 {
                        i64::from((year - 1) / 1000 + 1)
                    } else {
                        i64::from(year / 1000 - 1)
                    }
                }
                "monthname" => {
                    return Ok(Value::Varchar(
                        [
                            "January",
                            "February",
                            "March",
                            "April",
                            "May",
                            "June",
                            "July",
                            "August",
                            "September",
                            "October",
                            "November",
                            "December",
                        ][usize::from(month - 1)]
                        .into(),
                    ));
                }
                "dayname" => {
                    return Ok(Value::Varchar(
                        [
                            "Sunday",
                            "Monday",
                            "Tuesday",
                            "Wednesday",
                            "Thursday",
                            "Friday",
                            "Saturday",
                        ][(i64::from(date.days()) + 4).rem_euclid(7) as usize]
                            .into(),
                    ));
                }
                "last_day" => {
                    let mut last = day;
                    while last < 31 && Date::from_ymd(year, month, last + 1).is_ok() {
                        last += 1;
                    }
                    return Date::from_ymd(year, month, last).map(Value::Date);
                }
                _ => return Err(invalid("unknown date part specifier")),
            }
        }
    };
    Ok(Value::Integer(i128::from(result)))
}
