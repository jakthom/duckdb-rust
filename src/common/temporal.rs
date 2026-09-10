//! Owned temporal payloads. Calendar arithmetic retains interval components;
//! comparison uses DuckDB's 30-day month normalization. No host clock or zone.
use std::{cmp::Ordering, fmt};

use serde::{Deserialize, Serialize};

use super::{DataType, Date, Error, Result};

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
                    + i128::from(57599 - offset)
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
        let text = text.trim();
        if *data_type == DataType::Interval {
            return parse_interval(text);
        }
        if matches!(
            data_type,
            DataType::Time | DataType::TimeNs | DataType::TimeTz
        ) {
            let precision = if *data_type == DataType::TimeNs {
                1_000_000_000
            } else {
                1_000_000
            };
            let (ticks, offset) = parse_time(text, precision)?;
            return if *data_type == DataType::TimeTz {
                Ok(Self::TimeTz {
                    micros: ticks,
                    offset: offset.unwrap_or(0),
                })
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
        let date: Date = date_text.parse()?;
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
        let ticks =
            i128::from(self.ticks()?) * i128::from(target_precision) / i128::from(source_precision);
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
    if !(0..16).contains(&values[0])
        || !(0..60).contains(&values[1])
        || !(0..60).contains(&values[2])
    {
        return Err(invalid("UTC offset outside range"));
    }
    Ok(sign * (values[0] * 3600 + values[1] * 60 + values[2]))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_interval(text: &str) -> Result<TemporalValue> {
    let words: Vec<_> = text.split_whitespace().collect();
    let (mut months, mut days, mut micros) = (0_i128, 0_i128, 0_i128);
    let mut i = 0;
    while i < words.len() {
        let number = words[i];
        i += 1;
        if number.contains(':') {
            let sign = if number.starts_with('-') { -1 } else { 1 };
            let clock = number.trim_start_matches(['-', '+']);
            let mut fields = clock.split(':');
            let hour = fields
                .next()
                .unwrap_or("")
                .parse::<i128>()
                .map_err(|_| invalid("invalid interval hour"))?;
            let tail = fields.collect::<Vec<_>>().join(":");
            let (rest, offset) = parse_time(&format!("0:{tail}"), 1_000_000)?;
            if offset.is_some() {
                return Err(invalid("invalid interval clock"));
            }
            micros = micros
                .checked_add(sign * (hour * 3_600_000_000 + i128::from(rest)))
                .ok_or_else(|| invalid("interval overflow"))?;
            continue;
        }
        let unit = words
            .get(i)
            .ok_or_else(|| invalid("interval needs a unit"))?
            .to_ascii_lowercase();
        i += 1;
        let (coefficient, divisor) = decimal_parts(number)?;
        let (month_factor, day_factor, micro_factor) = match unit.trim_end_matches('s') {
            "year" | "yr" => (12, 0, 0),
            "month" | "mon" => (1, 0, 0),
            "decade" => (120, 0, 0),
            "century" | "centurie" => (1200, 0, 0),
            "millennium" | "millennia" => (12000, 0, 0),
            "week" => (0, 7, 0),
            "day" => (0, 1, 0),
            "hour" | "hr" => (0, 0, 3_600_000_000),
            "minute" | "min" => (0, 0, 60_000_000),
            "second" | "sec" => (0, 0, 1_000_000),
            "millisecond" | "ms" => (0, 0, 1000),
            "microsecond" | "us" => (0, 0, 1),
            _ => return Err(invalid("unknown interval unit")),
        };
        let month_part = coefficient
            .checked_mul(month_factor)
            .ok_or_else(|| invalid("interval overflow"))?;
        months += month_part / divisor;
        let day_part = coefficient
            .checked_mul(day_factor)
            .and_then(|d| d.checked_add((month_part % divisor) * 30))
            .ok_or_else(|| invalid("interval overflow"))?;
        days += day_part / divisor;
        micros += coefficient
            .checked_mul(micro_factor)
            .and_then(|m| m.checked_add((day_part % divisor) * i128::from(MICROS_PER_DAY)))
            .ok_or_else(|| invalid("interval overflow"))?
            / divisor;
    }
    if words.is_empty() {
        return Err(invalid("empty interval"));
    }
    Ok(TemporalValue::Interval {
        months: i32::try_from(months).map_err(|_| invalid("interval months overflow"))?,
        days: i32::try_from(days).map_err(|_| invalid("interval days overflow"))?,
        micros: i64::try_from(micros).map_err(|_| invalid("interval micros overflow"))?,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decimal_parts(text: &str) -> Result<(i128, i128)> {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let text = text.trim_start_matches(['-', '+']);
    let mut coefficient = 0_i128;
    let mut divisor = 1_i128;
    let mut fractional = false;
    let mut digits = 0;
    for byte in text.bytes() {
        if byte == b'.' && !fractional {
            fractional = true;
            continue;
        }
        if !byte.is_ascii_digit() {
            return Err(invalid("invalid interval number"));
        }
        coefficient = coefficient
            .checked_mul(10)
            .and_then(|n| n.checked_add(i128::from(byte - b'0')))
            .ok_or_else(|| invalid("interval number overflow"))?;
        if fractional {
            divisor = divisor
                .checked_mul(10)
                .ok_or_else(|| invalid("interval precision overflow"))?;
        }
        digits += 1;
    }
    if digits == 0 {
        return Err(invalid("invalid interval number"));
    }
    Ok((sign * coefficient, divisor))
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
