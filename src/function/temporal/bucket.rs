//! Fixed-duration and calendar-month buckets, with explicit offset and origin
//! overloads. TIME wraps at midnight; no ambient timezone is consulted.
use super::{truncation::calendar_date, *};
use crate::function::{ArgumentEvaluation, ArgumentProvenance, ScalarSignature};

const ORIGIN_MICROS: i64 = 10959 * MICROS_PER_DAY;
const ORIGIN_MONTHS: i32 = 360;

#[derive(Debug)]
struct TimeBucket {
    known_null: bool,
    offset: bool,
    signature: Option<ScalarSignature>,
}

#[derive(Clone, Copy)]
enum Width {
    Micros(i64),
    Months(i32),
}

#[derive(Clone, Copy)]
struct Interval {
    months: i32,
    days: i32,
    micros: i64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    registry
        .register_scalar(Arc::new(TimeBucket {
            known_null: false,
            offset: false,
            signature: None,
        }))
        .expect("unique time_bucket function");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn candidates() -> Vec<ScalarSignature> {
    use DataType::*;
    // Preserve advertised placeholder order, including unavailable ICU entries.
    [
        vec![Interval, Date],
        vec![Interval, Date, Date],
        vec![Interval, Date, Interval],
        vec![Interval, Time],
        vec![Interval, Time, Interval],
        vec![Interval, Time, Time],
        vec![Interval, Timestamp],
        vec![Interval, Timestamp, Interval],
        vec![Interval, Timestamp, Timestamp],
        vec![Interval, TimestampTz],
        vec![Interval, TimestampTz, Interval],
        vec![Interval, TimestampTz, TimestampTz],
        vec![Interval, TimestampTz, Varchar],
    ]
    .into_iter()
    .map(|arguments| ScalarSignature {
        return_type: arguments[1].clone(),
        arguments,
    })
    .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for TimeBucket {
    fn name(&self) -> &str {
        "time_bucket"
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let candidates = candidates();
        ScalarSignature::validate_candidates(self.name(), &candidates, query)?;
        let selected = arguments.select_overload(self.name(), &candidates)?;
        let signature = ScalarSignature::selected(&candidates, selected)?.clone();
        if signature.arguments.len() != arguments.len() {
            return Err(Error::Internal("selected bucket overload arity".into()));
        }
        if signature.return_type == DataType::TimestampTz {
            return Err(Error::Unsupported(
                "time_bucket with time zones requires a selected ICU timezone adapter".into(),
            ));
        }
        let mut known_null = false;
        for index in 0..arguments.len() {
            if arguments.is_provably_null(index)? {
                known_null = true;
                break;
            }
        }
        Ok(Some(Arc::new(Self {
            known_null,
            offset: signature.arguments.get(2) == Some(&DataType::Interval),
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
            Error::Unsupported("bucket requires selected overload binding".into())
        })?;
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Internal("bound bucket argument count".into()));
        }
        Ok(signature.arguments.clone())
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let signature = self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported("bucket requires selected overload binding".into())
        })?;
        if arguments != signature.arguments {
            return Err(Error::Internal("bound bucket signature changed".into()));
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
            return Err(Error::Internal("bucket argument provenance count".into()));
        }
        if self.known_null {
            if !arguments.is_empty() {
                return Err(Error::Internal(
                    "constant NULL bucket received arguments".into(),
                ));
            }
            return Ok(Value::Null);
        }
        if !matches!(arguments.len(), 2 | 3) {
            return Err(Error::Internal("bucket argument count".into()));
        }
        if arguments[0].is_null() {
            return Ok(Value::Null);
        }
        let width = Interval::from_value(&arguments[0])?;
        let value = &arguments[1];
        let third = arguments.get(2);
        // Retained overload metadata is needed even when the third value is
        // NULL: offset and origin classification have different demand order.
        let offset =
            self.offset || matches!(third, Some(Value::Temporal(TemporalValue::Interval { .. })));
        let origin = third.is_some() && !offset;
        let constant_path = provenance[0] == ArgumentProvenance::Constant
            && (!origin || provenance[2] == ArgumentProvenance::Constant);
        if origin && constant_path && third.is_some_and(|value| value.is_null() || !finite(value)) {
            return Ok(Value::Null);
        }
        let classified = if constant_path {
            width.classify(false)?
        } else {
            None
        };
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if origin && !finite(&arguments[2]) {
            return Ok(Value::Null);
        }
        let classified = match classified {
            Some(width) => width,
            None => width
                .classify(true)?
                .ok_or_else(|| Error::Internal("classified bucket width".into()))?,
        };
        if !finite(value) {
            return Ok(value.clone());
        }
        let target = match value {
            Value::Date(_) => DataType::Date,
            Value::Temporal(value) => value.data_type(),
            _ => return Err(Error::Internal("bucket expected temporal input".into())),
        };
        let result = if offset {
            let offset = Interval::from_value(&arguments[2])?;
            let shifted = add_interval(to_timestamp(value)?, offset.invert()?)?;
            let bucket = match classified {
                Width::Micros(width) => bucket_micros(width, shifted, ORIGIN_MICROS)?,
                Width::Months(width) => timestamp_from_calendar(
                    bucket_months(
                        width,
                        epoch_months(TemporalValue::Timestamp(shifted).date()?)?,
                        ORIGIN_MONTHS,
                    )?,
                    0,
                )?,
            };
            from_timestamp(add_interval(bucket, offset)?, &target)?
        } else {
            match classified {
                Width::Micros(width) => {
                    let origin = if let Some(origin) = third {
                        to_timestamp(origin)?
                    } else {
                        ORIGIN_MICROS
                    };
                    from_timestamp(bucket_micros(width, to_timestamp(value)?, origin)?, &target)?
                }
                Width::Months(width) => {
                    let origin = if let Some(origin) = third {
                        epoch_months(to_date(origin)?)?
                    } else {
                        ORIGIN_MONTHS
                    };
                    let date = bucket_months(width, epoch_months(to_date(value)?)?, origin)?;
                    match target {
                        DataType::Date => Value::Date(date),
                        DataType::Time => Value::Temporal(TemporalValue::Time(0)),
                        DataType::Timestamp => Value::Temporal(TemporalValue::Timestamp(
                            timestamp_from_calendar(date, 0)?,
                        )),
                        _ => return Err(Error::Internal("bucket return type".into())),
                    }
                }
            }
        };
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Interval {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Temporal(TemporalValue::Interval {
                months,
                days,
                micros,
            }) => Ok(Self {
                months: *months,
                days: *days,
                micros: *micros,
            }),
            _ => Err(Error::Internal("bucket expected INTERVAL".into())),
        }
    }
    fn classify(self, throw: bool) -> Result<Option<Width>> {
        if self.months == 0 {
            let days = i64::from(self.days)
                .checked_mul(MICROS_PER_DAY)
                .ok_or_else(|| Error::Conversion("Could not convert Day to Microseconds".into()))?;
            let micros = self.micros.checked_add(days).ok_or_else(|| {
                Error::Conversion("Could not convert Interval to Microseconds".into())
            })?;
            if micros > 0 {
                return Ok(Some(Width::Micros(micros)));
            }
        } else if self.days == 0 && self.micros == 0 {
            if self.months > 0 {
                return Ok(Some(Width::Months(self.months)));
            }
        } else if throw {
            return Err(Error::NotImplemented(
                "Month intervals cannot have day or time component".into(),
            ));
        }
        if throw {
            Err(Error::NotImplemented(
                "Period must be greater than 0".into(),
            ))
        } else {
            Ok(None)
        }
    }
    fn invert(self) -> Result<Self> {
        let days = self
            .days
            .checked_neg()
            .ok_or_else(|| Error::OutOfRange("Interval days value out of range".into()))?;
        let micros = self
            .micros
            .checked_neg()
            .ok_or_else(|| Error::OutOfRange("Interval micros value out of range".into()))?;
        let months = self
            .months
            .checked_neg()
            .ok_or_else(|| Error::OutOfRange("Interval months value out of range".into()))?;
        Ok(Self {
            months,
            days,
            micros,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn finite(value: &Value) -> bool {
    match value {
        Value::Date(date) => date.is_finite(),
        Value::Temporal(value) => value.is_finite(),
        _ => false,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn to_date(value: &Value) -> Result<Date> {
    match value {
        Value::Date(date) => Ok(*date),
        Value::Temporal(TemporalValue::Time(_)) => Date::from_days(0),
        Value::Temporal(value @ TemporalValue::Timestamp(_)) => value.date(),
        _ => Err(Error::Internal("bucket date conversion type".into())),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn to_timestamp(value: &Value) -> Result<i64> {
    match value {
        Value::Date(date) => timestamp_from_calendar(*date, 0),
        Value::Temporal(TemporalValue::Time(ticks) | TemporalValue::Timestamp(ticks)) => Ok(*ticks),
        _ => Err(Error::Internal("bucket timestamp conversion type".into())),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn from_timestamp(ticks: i64, target: &DataType) -> Result<Value> {
    let value = TemporalValue::Timestamp(ticks);
    Ok(match target {
        DataType::Timestamp => Value::Temporal(value),
        DataType::Date => Value::Date(value.date()?),
        DataType::Time => {
            value.check_text_renderable()?;
            Value::Temporal(TemporalValue::Time(ticks.rem_euclid(MICROS_PER_DAY)))
        }
        _ => return Err(Error::Internal("bucket timestamp return type".into())),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sub64(left: i64, right: i64) -> Result<i64> {
    left.checked_sub(right).ok_or_else(|| {
        Error::OutOfRange(format!(
            "Overflow in subtraction of INT64 ({left} - {right})!"
        ))
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bucket_micros(width: i64, ticks: i64, origin: i64) -> Result<i64> {
    let origin = origin % width;
    let ticks = sub64(ticks, origin)?;
    let mut result = ticks / width * width;
    if ticks < 0 && ticks % width != 0 {
        result = sub64(result, width)?;
    }
    result.checked_add(origin).ok_or_else(|| {
        Error::OutOfRange(format!(
            "Overflow in addition of INT64 ({result} + {origin})!"
        ))
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn epoch_months(date: Date) -> Result<i32> {
    let (year, month, _) = date
        .to_ymd()
        .ok_or_else(|| Error::Internal("finite bucket calendar".into()))?;
    Ok((year - 1970) * 12 + i32::from(month) - 1)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bucket_months(width: i32, months: i32, origin: i32) -> Result<Date> {
    let origin = origin % width;
    let sub = |left: i32, right: i32| {
        left.checked_sub(right).ok_or_else(|| {
            Error::OutOfRange(format!(
                "Overflow in subtraction of INT32 ({left} - {right})!"
            ))
        })
    };
    let months = sub(months, origin)?;
    let mut result = months / width * width;
    if months < 0 && months % width != 0 {
        result = sub(result, width)?;
    }
    let result = result.checked_add(origin).ok_or_else(|| {
        Error::OutOfRange(format!(
            "Overflow in addition of INT32 ({result} + {origin})!"
        ))
    })?;
    calendar_date(
        1970 + result.div_euclid(12),
        (result.rem_euclid(12) + 1) as u8,
        1,
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn add_interval(ticks: i64, interval: Interval) -> Result<i64> {
    let temporal = TemporalValue::Timestamp(ticks);
    if !temporal.is_finite() {
        return Ok(ticks);
    }
    // Interval::Add decomposes its input before shifting the month component.
    temporal.check_text_renderable()?;
    let mut date = temporal.date()?;
    if interval.months != 0 {
        let (year, month, day) = date
            .to_ymd()
            .ok_or_else(|| Error::Internal("finite interval calendar".into()))?;
        let total = i64::from(year) * 12 + i64::from(month) - 1 + i64::from(interval.months);
        let year = total.div_euclid(12) as i32;
        let month = (total.rem_euclid(12) + 1) as u8;
        let last = match month {
            2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        date = calendar_date(year, month, day.min(last))?;
    }
    let mut days = date
        .days()
        .checked_add(interval.days)
        .and_then(|days| days.checked_add((interval.micros / MICROS_PER_DAY) as i32))
        .filter(|days| days.abs_diff(0) < i32::MAX as u32)
        .ok_or_else(|| Error::OutOfRange("Date out of range".into()))?;
    let mut clock = ticks.rem_euclid(MICROS_PER_DAY) + interval.micros % MICROS_PER_DAY;
    if clock >= MICROS_PER_DAY {
        clock -= MICROS_PER_DAY;
        days = days
            .checked_add(1)
            .ok_or_else(|| Error::OutOfRange("Date out of range".into()))?;
    } else if clock < 0 {
        clock += MICROS_PER_DAY;
        days = days
            .checked_sub(1)
            .ok_or_else(|| Error::OutOfRange("Date out of range".into()))?;
    }
    timestamp_from_calendar(Date::from_days(days)?, clock)
}
