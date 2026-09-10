//! Core clock/timestamp text scanning. Named zones are ignored only for naive
//! timestamps; zoned values require UTC or explicit numeric offsets without ICU.
use super::{MICROS_PER_DAY, TemporalValue, invalid, timestamp_from_calendar};
use crate::common::{DataType, Date, Error, Result};

struct Scanner<'a, 'c> {
    bytes: &'a [u8],
    pos: usize,
    check: &'c mut dyn FnMut() -> Result<()>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Scanner<'_, '_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn advance(&mut self) -> Result<()> {
        if self.pos.is_multiple_of(1024) {
            (self.check)()?;
        }
        self.pos += 1;
        Ok(())
    }
    fn space(&mut self) -> Result<()> {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.advance()?;
        }
        Ok(())
    }
    fn digits(&mut self, strict: bool) -> Result<i64> {
        let Some(first @ b'0'..=b'9') = self.peek() else {
            return Err(invalid("invalid clock or UTC offset field"));
        };
        self.advance()?;
        let mut value = i64::from(first - b'0');
        if let Some(second @ b'0'..=b'9') = self.peek() {
            self.advance()?;
            value = value * 10 + i64::from(second - b'0');
        } else if strict {
            return Err(invalid("UTC offset fields require two digits"));
        }
        Ok(value)
    }
    fn clock(&mut self, nanos: bool, strict: bool) -> Result<Parts> {
        self.space()?;
        let start = self.pos;
        let mut hour = 0_i64;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            if self.pos - start == 9 {
                return Err(invalid("invalid clock hour field"));
            }
            hour = hour * 10 + i64::from(digit - b'0');
            self.advance()?;
        }
        if self.pos == start || self.peek() != Some(b':') {
            return Err(invalid("invalid clock text"));
        }
        self.advance()?;
        let minute_start = self.pos;
        let minute = if self.peek().is_none() && !strict {
            0
        } else {
            self.digits(false)?
        };
        let second = if self.peek().is_none() {
            if strict && self.pos - minute_start != 2 {
                return Err(invalid("strict clock requires a two-digit final minute"));
            }
            0
        } else {
            if self.peek() != Some(b':') {
                return Err(invalid("invalid clock separator"));
            }
            self.advance()?;
            if self.peek().is_none() && !strict {
                0
            } else {
                self.digits(false)?
            }
        };
        if minute >= 60 || second >= 60 {
            return Err(invalid("invalid clock minute or second field"));
        }
        if hour > 24 {
            return Err(invalid("time field value out of range"));
        }
        let mut fraction = 0;
        if self.peek() == Some(b'.') {
            self.advance()?;
            let mut multiplier = if nanos { 100_000_000 } else { 100_000 };
            while let Some(digit @ b'0'..=b'9') = self.peek() {
                fraction += i64::from(digit - b'0') * multiplier;
                multiplier /= 10;
                self.advance()?;
            }
        }
        let remainder = if nanos { fraction % 1000 } else { 0 };
        let micros = ((hour * 60 + minute) * 60 + second) * 1_000_000
            + if nanos { fraction / 1000 } else { fraction };
        if micros > MICROS_PER_DAY {
            return Err(invalid("time field value out of range"));
        }
        if strict {
            self.space()?;
            if self.pos != self.bytes.len() {
                return Err(invalid("invalid strict clock trailing text"));
            }
        }
        Ok(Parts {
            micros,
            nanos: remainder,
        })
    }
    fn offset(&mut self) -> Result<i32> {
        let sign = match self.peek() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => return Err(invalid("invalid UTC offset")),
        };
        self.advance()?;
        let mut offset = self.digits(true)? * 3600;
        let colons = self.peek() == Some(b':');
        if colons {
            self.advance()?;
        }
        if self.pos + 2 > self.bytes.len() || !self.peek().is_some_and(|b| b.is_ascii_digit()) {
            return if colons {
                Err(invalid("invalid UTC offset minute"))
            } else {
                Ok((sign * offset) as i32)
            };
        }
        offset += self.digits(true)? * 60;
        if colons && self.peek() == Some(b':') {
            self.advance()?;
            offset += self.digits(true)?;
        }
        // Core validates digits, not per-field civil ranges. TIMETZ separately
        // bounds the resulting total offset; TIMESTAMP accepts larger totals.
        Ok((sign * offset) as i32)
    }
}

struct Parts {
    micros: i64,
    nanos: i64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn timestamp(
    text: &str,
    use_offset: bool,
    nanos: bool,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Parts> {
    let (date, pos) = Date::parse_prefix_checked(text, &mut *check)?;
    timestamp_after_date(text, date, pos, use_offset, nanos, check)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn timestamp_after_date(
    text: &str,
    date: Date,
    mut pos: usize,
    use_offset: bool,
    nanos: bool,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Parts> {
    if pos == text.len() {
        let micros = if date == Date::INFINITY {
            i64::MAX
        } else if date == Date::NEG_INFINITY {
            -i64::MAX
        } else {
            timestamp_from_calendar(date, 0)
                .map_err(|_| invalid("timestamp outside finite range"))?
        };
        return Ok(Parts { micros, nanos: 0 });
    }
    let bytes = text.as_bytes();
    if matches!(bytes.get(pos), Some(b' ' | b'T')) {
        pos += 1;
    }
    let mut suffix = Scanner { bytes, pos, check };
    suffix.space()?;
    while suffix
        .peek()
        .is_some_and(|b| !matches!(b, b'Z' | b'+' | b'-') && !b.is_ascii_whitespace())
    {
        suffix.advance()?;
    }
    let end = suffix.pos;
    let mut clock = Scanner {
        bytes: &bytes[..end],
        pos,
        check: suffix.check,
    };
    let mut parts = clock.clock(nanos, false)?;
    if clock.pos != end {
        return Err(invalid("invalid timestamp clock suffix"));
    }
    parts.micros = timestamp_from_calendar(date, parts.micros)
        .map_err(|_| invalid("timestamp outside finite range"))?;
    let mut suffix = Scanner {
        bytes,
        pos: end,
        check: clock.check,
    };
    match suffix.peek() {
        None => (),
        Some(b'Z') => suffix.advance()?,
        Some(b'+' | b'-') => {
            let offset = suffix.offset().map_err(|error| match error {
                Error::Conversion(_) => invalid("non-UTC timezone requires the ICU extension"),
                _ => error,
            })?;
            if use_offset {
                parts.micros =
                    finite_timestamp(i128::from(parts.micros) - i128::from(offset) * 1_000_000)?;
            }
        }
        Some(b' ') => {
            suffix.advance()?;
            let start = suffix.pos;
            while suffix.peek().is_some_and(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'+' | b'-' | b':')
            }) {
                suffix.advance()?;
            }
            let zone = &text[start..suffix.pos];
            if use_offset && !zone.is_empty() && !zone.eq_ignore_ascii_case("UTC") {
                return Err(invalid("non-UTC timezone requires the ICU extension"));
            }
        }
        _ => return Err(invalid("non-UTC timezone requires the ICU extension")),
    }
    suffix.space()?;
    if suffix.pos != bytes.len() {
        return Err(invalid("invalid timestamp trailing text"));
    }
    (suffix.check)()?;
    Ok(parts)
}

/// Ordinary DATE conversion accepts a validated timestamp suffix, retaining
/// the original date even when the clock is 24:00. Core retries an overflowing
/// timestamp with a placeholder calendar date. Reuse its parsed suffix offset
/// instead of allocating and reparsing an unbounded replacement string.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn parse_date_cast(text: &str, check: &mut dyn FnMut() -> Result<()>) -> Result<Date> {
    let (date, pos) = Date::parse_prefix_checked(text, &mut *check)?;
    let mut suffix = Scanner {
        bytes: text.as_bytes(),
        pos,
        check,
    };
    suffix.space()?;
    if suffix.pos == text.len() {
        (suffix.check)()?;
        return Ok(date);
    }
    let result = timestamp_after_date(text, date, pos, false, false, suffix.check);
    let result = match result {
        Err(Error::Conversion(message)) if message == "timestamp outside finite range" => {
            timestamp_after_date(
                text,
                Date::from_ymd(2000, 1, 1)?,
                pos,
                false,
                false,
                suffix.check,
            )
        }
        other => other,
    };
    result.map(|_| date).map_err(|error| match error {
        Error::Conversion(message)
            if matches!(
                message.as_str(),
                "timestamp outside finite range" | "time field value out of range"
            ) =>
        {
            invalid("date field value out of range")
        }
        Error::Conversion(_) => invalid("invalid DATE calendar text"),
        other => other,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn finite_timestamp(ticks: i128) -> Result<i64> {
    if ticks <= -i128::from(i64::MAX) || ticks >= i128::from(i64::MAX) {
        Err(invalid("timestamp outside finite range"))
    } else {
        Ok(ticks as i64)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn parse(
    text: &str,
    data_type: &DataType,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<TemporalValue> {
    parse_inner(text, data_type, check).map_err(|error| {
        let Error::Conversion(message) = error else {
            return error;
        };
        // These are diagnostics from this parser's own conversion, not a
        // policy for catching arbitrary adapter/validator errors. Preserve
        // interruption and other fatal categories unchanged, and bound input
        // copied into an error independently of the caller's string length.
        let end = text.char_indices().nth(128).map_or(text.len(), |(i, _)| i);
        let input = &text[..end];
        let tail = if end < text.len() { "..." } else { "" };
        let rendered = format!("{input}{tail}");
        use DataType::*;
        match data_type {
            Time | TimeNs | TimeTz => invalid(&format!(
                "time field value out of range: \"{rendered}\", expected format is ([YYYY-MM-DD ]HH:MM:SS[.MS])"
            )),
            TimestampNs | TimestampS | TimestampMs => invalid(&format!(
                "Could not convert string '{rendered}' to INT64"
            )),
            _ if message == "non-UTC timezone requires the ICU extension" => invalid(&format!(
                "timestamp field value \"{rendered}\" has a timestamp that is not UTC.\nUse the TIMESTAMPTZ type with the ICU extension loaded to handle non-UTC timestamps."
            )),
            _ if matches!(message.as_str(), "date field value out of range" | "DATE outside finite range" | "time field value out of range" | "timestamp outside finite range") => invalid(&format!(
                "timestamp field value out of range: \"{rendered}\""
            )),
            _ => invalid(&format!(
                "invalid timestamp field format: \"{rendered}\", expected format is (YYYY-MM-DD HH:MM[:SS[.US]][±HH[:MM[:SS]]| ZONE])"
            )),
        }
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_inner(
    text: &str,
    data_type: &DataType,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<TemporalValue> {
    use DataType::*;
    if matches!(data_type, Time | TimeNs | TimeTz) {
        return parse_clock(text, data_type, false, check);
    }
    let nanos = matches!(data_type, TimestampNs | TimestampTzNs);
    let parts = timestamp(text, data_type.has_time_zone(), nanos, check)?;
    if parts.micros.abs_diff(0) == i64::MAX as u64 {
        return TemporalValue::from_ticks(data_type, parts.micros);
    }
    if nanos {
        TemporalValue::from_ticks(
            data_type,
            // Core checks the scaled microseconds before adding the retained
            // nanoremainder. A positive tail cannot rescue an overflowing
            // negative product, even when the mathematical total would fit.
            parts
                .micros
                .checked_mul(1000)
                .and_then(|ticks| ticks.checked_add(parts.nanos))
                .filter(|ticks| ticks.abs_diff(0) < i64::MAX as u64)
                .ok_or_else(|| invalid("timestamp nanoseconds outside finite range"))?,
        )
    } else {
        TemporalValue::Timestamp(parts.micros).scale_timestamp(data_type)
    }
}

/// Core VARIANT fallback requests strict text conversion. This is distinct
/// from ordinary explicit casts and from TRY_CAST failure recovery. TIMETZ
/// deliberately keeps permissive clock fields but requires a complete offset;
/// no strict clock target retries its input as a timestamp.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn parse_clock(
    text: &str,
    data_type: &DataType,
    strict: bool,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<TemporalValue> {
    use DataType::*;
    let nanos = *data_type == TimeNs;
    let mut clock = Scanner {
        bytes: text.as_bytes(),
        pos: 0,
        check,
    };
    let parsed = clock.clock(nanos, strict && *data_type != TimeTz);
    let parts = match parsed {
        Ok(parts) => {
            if *data_type == TimeTz {
                clock.space()?;
                let offset = if clock.peek().is_none() {
                    0
                } else {
                    clock.offset()?
                };
                if strict {
                    clock.space()?;
                    if clock.pos != clock.bytes.len() {
                        return Err(invalid("invalid strict clock offset trailing text"));
                    }
                }
                let value = TemporalValue::TimeTz {
                    micros: parts.micros,
                    offset,
                };
                value.validate()?;
                (clock.check)()?;
                return Ok(value);
            }
            parts
        }
        Err(Error::Conversion(_)) if !strict => {
            let mut parts = timestamp(text, *data_type == TimeTz, nanos, clock.check)?;
            if parts.micros.abs_diff(0) == i64::MAX as u64 {
                return Err(invalid("infinite timestamp has no time"));
            }
            parts.micros = parts.micros.rem_euclid(MICROS_PER_DAY);
            if *data_type == TimeTz {
                return Ok(TemporalValue::TimeTz {
                    micros: parts.micros,
                    offset: 0,
                });
            }
            parts
        }
        Err(error) => return Err(error),
    };
    (clock.check)()?;
    TemporalValue::from_ticks(
        data_type,
        if nanos {
            parts.micros * 1000 + parts.nanos
        } else {
            parts.micros
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn date_cast_suffix_validation_is_cooperative_across_calendar_range_retry() {
        for text in [
            " ".repeat(100_000) + "2000-01-01 12:34:56",
            "2000-01-01 12:34:56.".to_owned() + &"0".repeat(100_000),
            "5881580-07-10 24:00:00.".to_owned() + &"0".repeat(100_000),
            "5881580-07-10 24:00:00 ".to_owned() + &"z".repeat(100_000),
            "5877642-06-25 (BC) 24:00:00".to_owned() + &" ".repeat(100_000),
        ] {
            let mut checks = 0;
            let result = super::super::parse_date_cast_checked(&text, &mut || {
                checks += 1;
                if checks == 8 {
                    Err(Error::Interrupted)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(result, Err(Error::Interrupted)), "{result:?}");
            assert_eq!(checks, 8);
        }
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn strict_clock_source_policy_checks_long_inputs_and_preserves_cancellation() {
        for (kind, text) in [
            (DataType::Time, " ".repeat(100_000) + "12:34:56"),
            (
                DataType::TimeNs,
                "12:34:56.".to_owned() + &"0".repeat(100_000),
            ),
            (DataType::Time, "12:34:56".to_owned() + &" ".repeat(100_000)),
            (
                DataType::TimeTz,
                "12:34:56+02".to_owned() + &" ".repeat(100_000),
            ),
        ] {
            let mut checks = 0;
            let result = TemporalValue::parse_clock_strict_checked(&text, &kind, &mut || {
                checks += 1;
                if checks == 8 {
                    Err(Error::Interrupted)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(Error::Interrupted)),
                "{kind}: {result:?}"
            );
            assert_eq!(checks, 8);
        }
        assert!(matches!(
            TemporalValue::parse_clock_strict_checked(
                "12:34:56",
                &DataType::Timestamp,
                &mut || Ok(())
            ),
            Err(Error::Internal(_))
        ));
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn temporal_text_scans_long_calendar_clock_fraction_and_zone_inputs_cooperatively() {
        for (kind, text) in [
            (DataType::Time, " ".repeat(100_000) + "12:34:56"),
            (
                DataType::TimeNs,
                "12:34:56.".to_owned() + &"0".repeat(100_000),
            ),
            (DataType::Timestamp, "0".repeat(100_000) + "2000-01-01"),
            (
                DataType::Timestamp,
                "2000-01-01 ".to_owned() + &" ".repeat(100_000) + "12:34:56",
            ),
            (
                DataType::Timestamp,
                "2000-01-01 12:34:56 ".to_owned() + &"a".repeat(100_000),
            ),
            (
                DataType::TimeTz,
                "12:34:56".to_owned() + &" ".repeat(100_000) + "+02",
            ),
        ] {
            let mut checks = 0;
            let result = TemporalValue::parse_checked(&text, &kind, &mut || {
                checks += 1;
                if checks == 8 {
                    Err(Error::Interrupted)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(Error::Interrupted)),
                "{kind}: {result:?}"
            );
            assert_eq!(checks, 8);
        }
    }
}
