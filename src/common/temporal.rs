//! Owned temporal payloads. Calendar arithmetic retains interval components;
//! comparison uses DuckDB's 30-day month normalization. No host clock or zone.
use std::{cmp::Ordering, fmt};

use serde::{Deserialize, Serialize};

use super::{DataType, Date, Error, Result};

mod interval;

pub const MICROS_PER_DAY: i64 = 86_400_000_000;

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
            Self::Time(v) => (0..=MICROS_PER_DAY).contains(&v),
            Self::TimeNs(v) => (0..=MICROS_PER_DAY * 1000).contains(&v),
            Self::TimeTz { micros, offset } => {
                (0..=MICROS_PER_DAY).contains(&micros) && (-57599..=57599).contains(&offset)
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
        let text = text.trim();
        // Narrow timestamp literals are parsed at microsecond precision first,
        // then rounded half away from the epoch, including negative instants.
        if matches!(data_type, DataType::TimestampS | DataType::TimestampMs) {
            return Self::parse_checked(text, &DataType::Timestamp, check)?
                .scale_timestamp(data_type);
        }
        if matches!(
            data_type,
            DataType::Time | DataType::TimeNs | DataType::TimeTz
        ) {
            if let Some(position) = text.find(['+', '-', 'Z', 'z'])
                && text[..position]
                    .bytes()
                    .filter(|byte| *byte == b':')
                    .count()
                    != 2
            {
                return Err(invalid("standalone time offset requires seconds"));
            }
            let precision = if *data_type == DataType::TimeNs {
                1_000_000_000
            } else {
                1_000_000
            };
            let (ticks, offset) = parse_time(text, precision)?;
            return if *data_type == DataType::TimeTz {
                let value = Self::TimeTz {
                    micros: ticks,
                    offset: offset.unwrap_or(0),
                };
                value.validate()?;
                Ok(value)
            } else {
                Self::from_ticks(data_type, ticks)
            };
        }
        let precision = data_type
            .timestamp_precision()
            .ok_or_else(|| invalid("expected temporal type"))?;
        if text.eq_ignore_ascii_case("infinity") {
            return Self::from_ticks(data_type, i64::MAX);
        }
        if text.eq_ignore_ascii_case("-infinity") {
            return Self::from_ticks(data_type, -i64::MAX);
        }
        if text.eq_ignore_ascii_case("epoch") {
            return Self::from_ticks(data_type, 0);
        }
        let split = text
            .char_indices()
            .find(|(i, c)| {
                (*c == 'T' || *c == ' ')
                    && text.as_bytes()[*i + 1..]
                        .first()
                        .is_some_and(u8::is_ascii_digit)
            })
            .map(|(i, _)| i);
        let (date_text, time) = split.map_or((text, "00:00:00"), |i| (&text[..i], &text[i + 1..]));
        let date = Date::parse_checked(date_text, check)?;
        if !date.is_finite() {
            return Err(invalid("timestamp date must be finite"));
        }
        let (clock, offset) = parse_time(time, precision)?;
        let adjustment = if data_type.has_time_zone() {
            i128::from(offset.unwrap_or(0)) * i128::from(precision)
        } else {
            0
        };
        let ticks = i128::from(date.days()) * 86400 * i128::from(precision) + i128::from(clock)
            - adjustment;
        let ticks = i64::try_from(ticks).map_err(|_| invalid("timestamp outside finite range"))?;
        if ticks.abs_diff(0) >= i64::MAX as u64 {
            return Err(invalid("timestamp outside finite range"));
        }
        Self::from_ticks(data_type, ticks)
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
        kani::assume((0..=MICROS_PER_DAY).contains(&micros));
        kani::assume((-57599..=57599).contains(&offset));
        kani::assume((0..=MICROS_PER_DAY).contains(&other_micros));
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
fn parse_time(text: &str, precision: i64) -> Result<(i64, Option<i32>)> {
    let split = text.find(['+', '-', 'Z', 'z']);
    let (clock, zone) = split.map_or((text, None), |i| (&text[..i], Some(&text[i..])));
    let mut pieces = clock.split(':');
    let hour = pieces
        .next()
        .ok_or_else(|| invalid("invalid time"))?
        .parse::<i64>()
        .map_err(|_| invalid("invalid hour"))?;
    let minute = pieces
        .next()
        .ok_or_else(|| invalid("invalid time"))?
        .parse::<i64>()
        .map_err(|_| invalid("invalid minute"))?;
    let second = pieces.next().unwrap_or("0");
    if pieces.next().is_some() {
        return Err(invalid("invalid time"));
    }
    let (seconds, fraction) = second.split_once('.').unwrap_or((second, ""));
    let seconds = seconds
        .parse::<i64>()
        .map_err(|_| invalid("invalid second"))?;
    if !(0..=24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&seconds)
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid("time outside range"));
    }
    let mut subsecond = 0;
    let mut factor = precision / 10;
    for digit in fraction.bytes() {
        if factor == 0 {
            break;
        }
        subsecond += i64::from(digit - b'0') * factor;
        factor /= 10;
    }
    let ticks = ((hour * 60 + minute) * 60 + seconds) * precision + subsecond;
    if ticks > 86400 * precision {
        return Err(invalid("time outside range"));
    }
    let offset = zone.map(parse_offset).transpose()?;
    Ok((ticks, offset))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_offset(text: &str) -> Result<i32> {
    if text.eq_ignore_ascii_case("z") {
        return Ok(0);
    }
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let text = &text[1..];
    let parts: Vec<&str> = if text.contains(':') {
        text.split(':').collect()
    } else if text.len() == 4 && text.is_ascii() {
        vec![&text[..2], &text[2..]]
    } else {
        vec![text]
    };
    if parts.len() > 3 {
        return Err(invalid("invalid UTC offset"));
    }
    let mut values = [0; 3];
    for (slot, part) in values.iter_mut().zip(parts) {
        *slot = part
            .parse::<i32>()
            .map_err(|_| invalid("invalid UTC offset"))?;
    }
    if !(0..24).contains(&values[0])
        || !(0..60).contains(&values[1])
        || !(0..60).contains(&values[2])
    {
        return Err(invalid("UTC offset outside range"));
    }
    Ok(sign * (values[0] * 3600 + values[1] * 60 + values[2]))
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
                    offset.abs() / 3600
                )?;
                if offset % 3600 != 0 {
                    write!(f, ":{:02}", offset.abs() / 60 % 60)?;
                }
                if offset % 60 != 0 {
                    write!(f, ":{:02}", offset.abs() % 60)?;
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
                let clock = self
                    .ticks()
                    .map_err(|_| fmt::Error)?
                    .rem_euclid(precision * 86400);
                write!(
                    f,
                    "{} {}{}",
                    self.date().map_err(|_| fmt::Error)?,
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
