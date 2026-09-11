use super::*;
use crate::common::{
    Date, TemporalValue,
    temporal::{MICROS_PER_DAY, timestamp_from_calendar},
};

#[derive(Debug)]
pub struct TemporalArithmetic;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn overflow() -> Error {
    Error::Execution("temporal arithmetic overflow".into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn interval_parts(value: &Value) -> Result<(i32, i32, i64)> {
    match value.as_temporal()? {
        TemporalValue::Interval {
            months,
            days,
            micros,
        } => Ok((months, days, micros)),
        _ => Err(Error::Internal("expected interval input".into())),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_months(date: Date, months: i32) -> Result<Date> {
    let (year, month, mut day) = date.to_ymd().ok_or_else(overflow)?;
    let month = i64::from(year) * 12 + i64::from(month) - 1 + i64::from(months);
    let year = i32::try_from(month.div_euclid(12)).map_err(|_| overflow())?;
    let month = (month.rem_euclid(12) + 1) as u8;
    loop {
        if let Ok(date) = Date::from_ymd(year, month, day) {
            return Ok(date);
        }
        if day <= 28 {
            return Err(overflow());
        }
        day -= 1;
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn calendar_interval(date: Date, clock: i64, months: i32, days: i32, micros: i64) -> Result<i64> {
    let date = calendar_months(date, months)?;
    // Check component additions in the source order. In particular, overflow
    // in the day component is not repaired by cancelling microseconds later.
    let days = date
        .days()
        .checked_add(days)
        .and_then(|value| value.checked_add((micros / MICROS_PER_DAY) as i32))
        .filter(|value| value.abs_diff(0) < i32::MAX as u32)
        .ok_or_else(|| Error::OutOfRange("Date out of range".into()))?;
    let clock = clock + micros % MICROS_PER_DAY;
    let days = days
        .checked_add(clock.div_euclid(MICROS_PER_DAY) as i32)
        .ok_or_else(|| Error::Conversion("Date and time not in timestamp range".into()))?;
    timestamp_from_calendar(Date::from_days(days)?, clock.rem_euclid(MICROS_PER_DAY))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for TemporalArithmetic {
    fn name(&self) -> &'static str {
        "calendar-temporal-arithmetic"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        signatures().iter().any(|s| s == signature)
    }
    fn evaluate(
        &self,
        signature: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        use Operator::*;
        if signature.operator == Negate {
            let (months, days, micros) = interval_parts(&arguments[0])?;
            return Ok(Value::Temporal(TemporalValue::Interval {
                months: months.checked_neg().ok_or_else(overflow)?,
                days: days.checked_neg().ok_or_else(overflow)?,
                micros: micros.checked_neg().ok_or_else(overflow)?,
            }));
        }
        if signature.arguments == [DataType::Interval, DataType::Interval] {
            let (am, ad, au) = interval_parts(&arguments[0])?;
            let (bm, bd, bu) = interval_parts(&arguments[1])?;
            let subtract = signature.operator == Subtract;
            return Ok(Value::Temporal(TemporalValue::Interval {
                months: if subtract {
                    am.checked_sub(bm)
                } else {
                    am.checked_add(bm)
                }
                .ok_or_else(overflow)?,
                days: if subtract {
                    ad.checked_sub(bd)
                } else {
                    ad.checked_add(bd)
                }
                .ok_or_else(overflow)?,
                micros: if subtract {
                    au.checked_sub(bu)
                } else {
                    au.checked_add(bu)
                }
                .ok_or_else(overflow)?,
            }));
        }
        if signature.operator == Multiply || signature.operator == Divide {
            let (interval, number) = if signature.arguments[0] == DataType::Interval {
                (&arguments[0], &arguments[1])
            } else {
                (&arguments[1], &arguments[0])
            };
            let (months, days, micros) = interval_parts(interval)?;
            let number = number.as_f64()?;
            if signature.operator == Divide && number == 0.0 {
                return Ok(Value::Null);
            }
            let factor = if signature.operator == Divide {
                1.0 / number
            } else {
                number
            };
            let month_part = f64::from(months) * factor;
            let day_part = f64::from(days) * factor + month_part.fract() * 30.0;
            let micro_part = micros as f64 * factor + day_part.fract() * MICROS_PER_DAY as f64;
            if !factor.is_finite()
                || !(i32::MIN as f64..i32::MAX as f64 + 1.0).contains(&month_part.trunc())
                || !(i32::MIN as f64..i32::MAX as f64 + 1.0).contains(&day_part.trunc())
                || !(i64::MIN as f64..-(i64::MIN as f64)).contains(&micro_part.trunc())
            {
                return Err(overflow());
            }
            return Ok(Value::Temporal(TemporalValue::Interval {
                months: month_part as i32,
                days: day_part as i32,
                micros: micro_part as i64,
            }));
        }
        if signature.arguments == [DataType::Timestamp, DataType::Timestamp] {
            let a = arguments[0].as_temporal()?;
            let b = arguments[1].as_temporal()?;
            if !a.is_finite() || !b.is_finite() {
                return Err(Error::Execution(
                    "Cannot subtract infinite timestamps".into(),
                ));
            }
            let difference = a.ticks()?.checked_sub(b.ticks()?).ok_or_else(overflow)?;
            return Ok(Value::Temporal(TemporalValue::Interval {
                months: 0,
                days: (difference / MICROS_PER_DAY) as i32,
                micros: difference % MICROS_PER_DAY,
            }));
        }
        if signature.arguments.contains(&DataType::Date)
            && signature
                .arguments
                .iter()
                .any(|t| matches!(t, DataType::Time | DataType::TimeTz))
        {
            let (date, time) = if signature.arguments[0] == DataType::Date {
                (arguments[0].as_date()?, arguments[1].as_temporal()?)
            } else {
                (arguments[1].as_date()?, arguments[0].as_temporal()?)
            };
            let (time, offset) = match time {
                TemporalValue::TimeTz { micros, offset } => (micros, offset),
                t => (t.ticks()?, 0),
            };
            let ticks = if !date.is_finite() {
                if date == Date::INFINITY {
                    i64::MAX
                } else {
                    -i64::MAX
                }
            } else {
                let range_error = || {
                    Error::OutOfRange(
                        if signature.result == DataType::TimestampTz {
                            "Timestamp with time zone out of range"
                        } else {
                            "Timestamp out of range"
                        }
                        .into(),
                    )
                };
                timestamp_from_calendar(date, time)
                    .map_err(|_| range_error())?
                    .checked_sub(i64::from(offset) * 1_000_000)
                    .filter(|ticks| ticks.abs_diff(0) < i64::MAX as u64)
                    .ok_or_else(range_error)?
            };
            return TemporalValue::from_ticks(&signature.result, ticks).map(Value::Temporal);
        }
        let (value, interval) = if signature.arguments[0] == DataType::Interval {
            (&arguments[1], &arguments[0])
        } else {
            (&arguments[0], &arguments[1])
        };
        let (mut months, mut days, mut micros) = interval_parts(interval)?;
        if signature.operator == Subtract {
            months = months.checked_neg().ok_or_else(overflow)?;
            days = days.checked_neg().ok_or_else(overflow)?;
            micros = micros.checked_neg().ok_or_else(overflow)?;
        }
        if matches!(signature.result, DataType::Time | DataType::TimeTz) {
            let time = value.as_temporal()?;
            let (ticks, offset) = match time {
                TemporalValue::TimeTz { micros, offset } => (micros, Some(offset)),
                _ => (time.ticks()?, None),
            };
            let ticks = ((i128::from(ticks) + i128::from(micros))
                .rem_euclid(i128::from(MICROS_PER_DAY))) as i64;
            return Ok(Value::Temporal(if let Some(offset) = offset {
                TemporalValue::TimeTz {
                    micros: ticks,
                    offset,
                }
            } else {
                TemporalValue::Time(ticks)
            }));
        }
        let (date, clock) = match value {
            Value::Date(date) => {
                if date.is_finite() {
                    timestamp_from_calendar(*date, 0)?;
                }
                (*date, 0)
            }
            _ => {
                let t = value.as_temporal()?;
                if t.is_finite() {
                    // Timestamp::Convert checks the day start even when the
                    // original raw instant itself fits the physical domain.
                    timestamp_from_calendar(t.date()?, 0).map_err(|_| {
                        Error::Conversion("Date out of range in timestamp conversion".into())
                    })?;
                }
                (
                    t.date()?,
                    if t.is_finite() {
                        t.ticks()?.rem_euclid(MICROS_PER_DAY)
                    } else {
                        0
                    },
                )
            }
        };
        if !date.is_finite() {
            return Ok(Value::Temporal(TemporalValue::Timestamp(
                if date == Date::INFINITY {
                    i64::MAX
                } else {
                    -i64::MAX
                },
            )));
        }
        let ticks = calendar_interval(date, clock, months, days, micros)?;
        Ok(Value::Temporal(TemporalValue::Timestamp(ticks)))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn signatures() -> Vec<OperatorSignature> {
    use DataType::*;
    use Operator::*;
    let mut entries = vec![
        (Negate, vec![Interval], Interval),
        (Add, vec![Interval, Interval], Interval),
        (Subtract, vec![Interval, Interval], Interval),
        (Subtract, vec![Timestamp, Timestamp], Interval),
        (Multiply, vec![Interval, Double], Interval),
        (Multiply, vec![Double, Interval], Interval),
        (Divide, vec![Interval, Double], Interval),
    ];
    for (input, result) in [
        (Date, Timestamp),
        (Timestamp, Timestamp),
        (Time, Time),
        (TimeTz, TimeTz),
    ] {
        entries.push((Add, vec![input.clone(), Interval], result.clone()));
        entries.push((Add, vec![Interval, input.clone()], result.clone()));
        entries.push((Subtract, vec![input, Interval], result));
    }
    for (input, result) in [(Time, Timestamp), (TimeTz, TimestampTz)] {
        entries.push((Add, vec![Date, input.clone()], result.clone()));
        entries.push((Add, vec![input, Date], result));
    }
    entries
        .into_iter()
        .map(|(operator, arguments, result)| OperatorSignature {
            operator,
            arguments,
            result,
            nullable: operator == Divide,
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut OperatorRegistry) {
    for signature in signatures() {
        registry
            .register(signature, Arc::new(TemporalArithmetic))
            .expect("unique temporal arithmetic");
    }
}
