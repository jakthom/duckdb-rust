//! Owned temporal payloads. Calendar arithmetic retains interval components;
//! comparison uses DuckDB's 30-day month normalization. No host clock or zone.
use std::{cmp::Ordering, fmt};

use serde::{Deserialize, Serialize};

use super::{DataType, Date, Error, NestedPayload, Result, Value};

mod interval;
mod text;

pub const MICROS_PER_DAY: i64 = 86_400_000_000;
/// Provisional physical domain closed under the selected SQL clock constructors
/// and casts. make_time's leap-second rounding can produce 24:00:00.5; text
/// parsing is independently stricter. Arbitrary native/API raw clocks are not
/// accepted merely because they fit a signed storage word.
pub const MAX_CLOCK_MICROS: i64 = MICROS_PER_DAY + 500_000;

/// Calendar construction has a narrower path than the physical timestamp
/// domain: the start of its day must fit before the clock is added. A later
/// clock/offset cannot repair that intermediate overflow.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn timestamp_from_calendar(date: Date, clock: i64) -> Result<i64> {
    i64::from(date.days())
        .checked_mul(MICROS_PER_DAY)
        .and_then(|ticks| ticks.checked_add(clock))
        .filter(|ticks| ticks.abs_diff(0) < i64::MAX as u64)
        .ok_or_else(|| invalid("Date and time not in timestamp range"))
}

/// Check the built-in text-result boundary without changing physical values or
/// invoking SQL casts. Diagnostic Display may show raw unrenderable instants;
/// result serializers must not publish that fallback as successful SQL text.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn check_text_renderable(value: &Value, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
    check_text_renderable_inner(value, 0, &mut 0, check)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn check_text_renderable_inner(
    value: &Value,
    depth: usize,
    visited: &mut usize,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    if (*visited).is_multiple_of(1024) {
        check()?;
    }
    *visited += 1;
    if depth > 64 {
        return Err(Error::Resource("text result nesting exceeds 64".into()));
    }
    match value {
        Value::Temporal(value) => value.check_text_renderable(),
        Value::Nested(value) => {
            let mut child =
                |value: &Value| check_text_renderable_inner(value, depth + 1, visited, check);
            match &value.payload {
                NestedPayload::Sequence(values) | NestedPayload::Struct(values) => {
                    for value in values {
                        child(value)?;
                    }
                }
                NestedPayload::Map(entries) => {
                    for (key, value) in entries {
                        child(key)?;
                        child(value)?;
                    }
                }
                NestedPayload::Union { value, .. } | NestedPayload::Variant { value, .. } => {
                    child(value)?
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Pinned development's string-cast operator is non-fallible, so its underlying
/// calendar rendering failure is surfaced as INTERNAL and remains fatal under
/// TRY_CAST. Keep that source-specific SQL category separate from serializer
/// conversion errors and from ordinary parse input rejection.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn check_cast_text_renderable(
    value: &Value,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    check_text_renderable(value, check).map_err(|error| match error {
        Error::Conversion(message) => Error::Internal(format!(
            "Scalar function \"\"__cast\"\" threw an execution error, but the function is not marked as fallible - the function must call SetFallible(). Error: {message}"
        )),
        _ => error,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TemporalValue {
    Time(i64),
    TimeNs(i64),
    TimeTz { micros: i64, offset: i32 },
    Timestamp(i64),
    TimestampS(i64),
    TimestampMs(i64),
    TimestampNs(i64),
    TimestampTz(i64),
    TimestampTzNs(i64),
    Interval { months: i32, days: i32, micros: i64 },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DataType {
    pub fn is_temporal(&self) -> bool {
        matches!(
            self,
            Self::Time
                | Self::TimeNs
                | Self::TimeTz
                | Self::Timestamp
                | Self::TimestampS
                | Self::TimestampMs
                | Self::TimestampNs
                | Self::TimestampTz
                | Self::TimestampTzNs
                | Self::Interval
        )
    }
    pub fn timestamp_precision(&self) -> Option<i64> {
        match self {
            Self::TimestampS => Some(1),
            Self::TimestampMs => Some(1_000),
            Self::Timestamp | Self::TimestampTz => Some(1_000_000),
            Self::TimestampNs | Self::TimestampTzNs => Some(1_000_000_000),
            _ => None,
        }
    }
    pub fn has_time_zone(&self) -> bool {
        matches!(self, Self::TimeTz | Self::TimestampTz | Self::TimestampTzNs)
    }
}

pub const TEMPORAL_TYPES: [DataType; 10] = [
    DataType::Time,
    DataType::TimeNs,
    DataType::TimeTz,
    DataType::Timestamp,
    DataType::TimestampS,
    DataType::TimestampMs,
    DataType::TimestampNs,
    DataType::TimestampTz,
    DataType::TimestampTzNs,
    DataType::Interval,
];

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TemporalValue {
    /// Calendar string renderability is deliberately not physical validation.
    /// Epoch constructors, keys and storage can retain a valid raw instant that
    /// the core calendar formatter cannot represent.
    pub fn check_text_renderable(self) -> Result<()> {
        self.validate()?;
        if !self.is_finite() || self.data_type().timestamp_precision().is_none() {
            return Ok(());
        }
        let value = match self {
            Self::TimestampS(ticks) => Self::Timestamp(
                ticks
                    .checked_mul(1_000_000)
                    .ok_or_else(|| invalid("Could not convert Timestamp(S) to Timestamp(US)"))?,
            ),
            Self::TimestampMs(ticks) => Self::Timestamp(
                ticks
                    .checked_mul(1000)
                    .ok_or_else(|| invalid("Could not convert Timestamp(MS) to Timestamp(US)"))?,
            ),
            _ => self,
        };
        let precision = value
            .data_type()
            .timestamp_precision()
            .ok_or_else(|| invalid("expected timestamp"))?;
        i64::from(value.date()?.days())
            .checked_mul(precision * 86400)
            .ok_or_else(|| {
                invalid(if precision == 1_000_000_000 {
                    "Date out of range in timestamp_ns conversion"
                } else {
                    "Date out of range in timestamp conversion"
                })
            })?;
        Ok(())
    }
    pub fn data_type(self) -> DataType {
        match self {
            Self::Time(_) => DataType::Time,
            Self::TimeNs(_) => DataType::TimeNs,
            Self::TimeTz { .. } => DataType::TimeTz,
            Self::Timestamp(_) => DataType::Timestamp,
            Self::TimestampS(_) => DataType::TimestampS,
            Self::TimestampMs(_) => DataType::TimestampMs,
            Self::TimestampNs(_) => DataType::TimestampNs,
            Self::TimestampTz(_) => DataType::TimestampTz,
            Self::TimestampTzNs(_) => DataType::TimestampTzNs,
            Self::Interval { .. } => DataType::Interval,
        }
    }
    pub fn validate(self) -> Result<()> {
        let valid = match self {
            Self::Time(v) => (0..=MAX_CLOCK_MICROS).contains(&v),
            Self::TimeNs(v) => (0..=MAX_CLOCK_MICROS * 1000).contains(&v),
            Self::TimeTz { micros, offset } => {
                (0..=MAX_CLOCK_MICROS).contains(&micros) && (-57599..=57599).contains(&offset)
            }
            Self::Interval { .. } => true,
            _ => self.ticks()? != i64::MIN,
        };
        if valid {
            Ok(())
        } else {
            Err(invalid("temporal representation out of range"))
        }
    }
    pub fn ticks(self) -> Result<i64> {
        match self {
            Self::Time(v)
            | Self::TimeNs(v)
            | Self::Timestamp(v)
            | Self::TimestampS(v)
            | Self::TimestampMs(v)
            | Self::TimestampNs(v)
            | Self::TimestampTz(v)
            | Self::TimestampTzNs(v) => Ok(v),
            _ => Err(invalid("temporal value has no scalar unit count")),
        }
    }
    pub fn from_ticks(data_type: &DataType, ticks: i64) -> Result<Self> {
        let value = match data_type {
            DataType::Time => Self::Time(ticks),
            DataType::TimeNs => Self::TimeNs(ticks),
            DataType::Timestamp => Self::Timestamp(ticks),
            DataType::TimestampS => Self::TimestampS(ticks),
            DataType::TimestampMs => Self::TimestampMs(ticks),
            DataType::TimestampNs => Self::TimestampNs(ticks),
            DataType::TimestampTz => Self::TimestampTz(ticks),
            DataType::TimestampTzNs => Self::TimestampTzNs(ticks),
            _ => return Err(invalid("temporal type has no scalar unit count")),
        };
        value.validate()?;
        Ok(value)
    }
    pub fn is_finite(self) -> bool {
        !self
            .data_type()
            .timestamp_precision()
            .is_some_and(|_| self.ticks().is_ok_and(|v| v == i64::MAX || v == -i64::MAX))
    }
    pub fn comparison_key(self) -> i128 {
        match self {
            Self::Interval {
                months,
                days,
                micros,
            } => {
                (i128::from(months) * 30 + i128::from(days)) * i128::from(MICROS_PER_DAY)
                    + i128::from(micros)
            }
            Self::TimeTz { micros, offset } => {
                ((i128::from(micros) - i128::from(offset) * 1_000_000) << 24)
                    + (57599_i128 - i128::from(offset))
            }
            _ => i128::from(self.ticks().expect("scalar temporal variant")),
        }
    }
    pub fn compare(self, other: Self) -> Result<Ordering> {
        if self.data_type() != other.data_type() {
            return Err(invalid("temporal comparison requires common logical type"));
        }
        Ok(self.comparison_key().cmp(&other.comparison_key()))
    }
    pub fn packed_time_tz(self) -> Result<u64> {
        if let Self::TimeTz { micros, offset } = self {
            self.validate()?;
            Ok(((micros as u64) << 24) | (57599 - offset) as u64)
        } else {
            Err(invalid("expected TIME WITH TIME ZONE"))
        }
    }
    pub fn from_packed_time_tz(bits: u64) -> Result<Self> {
        let value = Self::TimeTz {
            micros: (bits >> 24) as i64,
            offset: 57599 - (bits & 0xff_ffff) as i32,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn parse(text: &str, data_type: &DataType) -> Result<Self> {
        Self::parse_checked(text, data_type, &mut || Ok(()))
    }
    pub(crate) fn parse_checked(
        text: &str,
        data_type: &DataType,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        if *data_type == DataType::Interval {
            return interval::parse(text, check);
        }
        text::parse(text, data_type, check)
    }
    pub fn date(self) -> Result<Date> {
        if !self.is_finite() {
            return Ok(if self.ticks()? > 0 {
                Date::INFINITY
            } else {
                Date::NEG_INFINITY
            });
        }
        let precision = self
            .data_type()
            .timestamp_precision()
            .ok_or_else(|| invalid("expected timestamp"))?;
        let days = self.ticks()?.div_euclid(precision * 86400);
        Date::from_days(i32::try_from(days).map_err(|_| invalid("timestamp date range"))?)
    }
    pub fn scale_timestamp(self, target: &DataType) -> Result<Self> {
        let source_precision = self
            .data_type()
            .timestamp_precision()
            .ok_or_else(|| invalid("expected timestamp"))?;
        let target_precision = target
            .timestamp_precision()
            .ok_or_else(|| invalid("expected timestamp"))?;
        if !self.is_finite() {
            return Self::from_ticks(target, self.ticks()?);
        }
        let ticks = i128::from(self.ticks()?);
        let ticks = if target_precision < source_precision {
            // Pinned development rounds ties away from the epoch; release
            // truncation is intentionally not authoritative for semantics.
            let factor = i128::from(source_precision / target_precision);
            ticks.signum() * ((ticks.abs() + factor / 2) / factor)
        } else {
            ticks * i128::from(target_precision) / i128::from(source_precision)
        };
        let ticks = i64::try_from(ticks).map_err(|_| invalid("timestamp precision overflow"))?;
        if ticks.abs_diff(0) >= i64::MAX as u64 {
            return Err(invalid("timestamp precision overflow"));
        }
        Self::from_ticks(target, ticks)
    }
    pub fn append_storage(self, output: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        match self {
            Self::Interval {
                months,
                days,
                micros,
            } => {
                output.extend(months.to_le_bytes());
                output.extend(days.to_le_bytes());
                output.extend(micros.to_le_bytes());
            }
            Self::TimeTz { .. } => output.extend(self.packed_time_tz()?.to_le_bytes()),
            _ => output.extend(self.ticks()?.to_le_bytes()),
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn invalid(message: &str) -> Error {
    Error::Conversion(message.into())
}

#[cfg(kani)]
mod verification {
    use super::*;
    #[kani::proof]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn kani_timetz_packing_and_equality_preserve_valid_local_time_and_offset() {
        let micros: i64 = kani::any();
        let offset: i32 = kani::any();
        let other_micros: i64 = kani::any();
        let other_offset: i32 = kani::any();
        // Physical TIME/TIMETZ validity is established before keys or storage.
        kani::assume((0..=MAX_CLOCK_MICROS).contains(&micros));
        kani::assume((-57599..=57599).contains(&offset));
        kani::assume((0..=MAX_CLOCK_MICROS).contains(&other_micros));
        kani::assume((-57599..=57599).contains(&other_offset));
        let value = TemporalValue::TimeTz { micros, offset };
        let other = TemporalValue::TimeTz {
            micros: other_micros,
            offset: other_offset,
        };
        let packed = value.packed_time_tz().unwrap();
        assert_eq!(TemporalValue::from_packed_time_tz(packed).unwrap(), value);
        assert_eq!(
            value.comparison_key() == other.comparison_key(),
            micros == other_micros && offset == other_offset
        );
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn clock_text(ticks: i64, precision: i64) -> String {
    let ticks = i128::from(ticks);
    let precision = i128::from(precision);
    let sign = if ticks < 0 { "-" } else { "" };
    let ticks = ticks.abs();
    let seconds = ticks / precision;
    let fraction = ticks % precision;
    let mut result = format!(
        "{sign}{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    if fraction != 0 {
        result.push('.');
        result.push_str(
            format!("{fraction:0width$}", width = precision.ilog10() as usize)
                .trim_end_matches('0'),
        );
    }
    result
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for TemporalValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Time(v) => f.write_str(&clock_text(v, 1_000_000)),
            Self::TimeNs(v) => f.write_str(&clock_text(v, 1_000_000_000)),
            Self::TimeTz { micros, offset } => {
                write!(
                    f,
                    "{}{}{:02}",
                    clock_text(micros, 1_000_000),
                    if offset < 0 { '-' } else { '+' },
                    offset.unsigned_abs() / 3600
                )?;
                if offset % 3600 != 0 {
                    write!(f, ":{:02}", offset.unsigned_abs() / 60 % 60)?;
                }
                if offset % 60 != 0 {
                    write!(f, ":{:02}", offset.unsigned_abs() % 60)?;
                }
                Ok(())
            }
            Self::Interval {
                months,
                days,
                micros,
            } => {
                let mut parts = Vec::new();
                for (count, name) in [(months / 12, "year"), (months % 12, "month"), (days, "day")]
                {
                    if count != 0 {
                        parts.push(format!(
                            "{count} {name}{}",
                            if count.abs_diff(0) == 1 { "" } else { "s" }
                        ));
                    }
                }
                if micros != 0 || parts.is_empty() {
                    parts.push(clock_text(micros, 1_000_000));
                }
                f.write_str(&parts.join(" "))
            }
            _ => {
                if !self.is_finite() {
                    return f.write_str(if self.ticks().map_err(|_| fmt::Error)? > 0 {
                        "infinity"
                    } else {
                        "-infinity"
                    });
                }
                let precision = self.data_type().timestamp_precision().ok_or(fmt::Error)?;
                let date = match self.date() {
                    Ok(date) => date,
                    Err(_) => {
                        return write!(
                            f,
                            "<{} raw ticks {}>",
                            self.data_type(),
                            self.ticks().map_err(|_| fmt::Error)?
                        );
                    }
                };
                let clock = self
                    .ticks()
                    .map_err(|_| fmt::Error)?
                    .rem_euclid(precision * 86400);
                write!(
                    f,
                    "{} {}{}",
                    date,
                    clock_text(clock, precision),
                    if self.data_type().has_time_zone() {
                        "+00"
                    } else {
                        ""
                    }
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn nested_temporal_render_checks_remain_cooperative_and_do_not_narrow_physical_values()
    -> Result<()> {
        let value = super::super::NestedValue::value(
            super::super::NestedType::List(DataType::Time).data_type(),
            NestedPayload::Sequence(vec![Value::Temporal(TemporalValue::Time(0)); 3000]),
        )?;
        let mut visits = 0;
        assert!(matches!(
            check_text_renderable(&value, &mut || {
                visits += 1;
                if visits == 2 {
                    Err(Error::Interrupted)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Interrupted)
        ));
        assert_eq!(visits, 2);
        let raw = TemporalValue::TimestampS(i64::MAX - 1);
        raw.validate()?;
        assert!(matches!(
            raw.check_text_renderable(),
            Err(Error::Conversion(_))
        ));
        assert!(raw.to_string().starts_with("<TIMESTAMP_S raw ticks "));
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn development_timestamp_precision_rounds_half_away_from_epoch() -> Result<()> {
        for (input, expected) in [
            (-1500, -2),
            (-1499, -1),
            (-500, -1),
            (-499, 0),
            (0, 0),
            (499, 0),
            (500, 1),
            (1499, 1),
            (1500, 2),
        ] {
            assert_eq!(
                TemporalValue::TimestampNs(input)
                    .scale_timestamp(&DataType::Timestamp)?
                    .ticks()?,
                expected
            );
            assert_eq!(
                TemporalValue::Timestamp(input)
                    .scale_timestamp(&DataType::TimestampMs)?
                    .ticks()?,
                expected
            );
        }
        for input in [i64::MIN + 2, i64::MAX - 1, -i64::MAX, i64::MAX] {
            let output = TemporalValue::TimestampNs(input).scale_timestamp(&DataType::Timestamp)?;
            if input.abs_diff(0) == i64::MAX as u64 {
                assert_eq!(output.ticks()?, input);
            } else {
                assert_eq!(
                    output.ticks()?,
                    if input > 0 {
                        9223372036854776
                    } else {
                        -9223372036854776
                    }
                );
            }
        }
        assert_eq!(
            TemporalValue::parse("1969-12-31 23:59:59.5", &DataType::TimestampS)?.ticks()?,
            -1
        );
        assert_eq!(
            TemporalValue::parse("1970-01-01 00:00:00.5", &DataType::TimestampS)?.ticks()?,
            1
        );
        assert_eq!(
            TemporalValue::parse("2000-01-01 00:00:00+23:59", &DataType::TimestampTz)?.to_string(),
            "1999-12-31 00:01:00+00"
        );
        assert!(TemporalValue::parse("00:00:00+23:59", &DataType::TimeTz).is_err());
        assert!(TemporalValue::parse("500000-01-01", &DataType::TimestampS).is_err());
        assert!(
            TemporalValue::Timestamp(i64::MAX - 1)
                .scale_timestamp(&DataType::TimestampNs)
                .is_err()
        );
        Ok(())
    }
}
