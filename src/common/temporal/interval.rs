//! Streaming core INTERVAL text semantics. Component overflow is checked in
//! input order; later cancellation cannot repair a prior overflowing addition.
use super::{MICROS_PER_DAY, TemporalValue, invalid};
use crate::common::{Error, Result};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn malformed() -> Error {
    invalid("Could not convert string to INTERVAL")
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unknown_unit(unit: &str) -> Error {
    // Bound diagnostic allocation independently of caller-controlled input.
    let end = unit.len().min(128);
    invalid(&format!(
        "extract specifier {:?} not recognized",
        &unit[..end]
    ))
}

struct Scanner<'a, 'c> {
    text: &'a str,
    pos: usize,
    check: &'c mut dyn FnMut() -> Result<()>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Scanner<'_, '_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }
    fn advance(&mut self) -> Result<()> {
        if self.pos.is_multiple_of(1024) {
            (self.check)()?;
        }
        self.pos += 1;
        Ok(())
    }
    fn space(&mut self) -> Result<()> {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n')) {
            self.advance()?;
        }
        Ok(())
    }
    fn integer(&mut self) -> Result<i64> {
        let start = self.pos;
        let mut value = 0_i64;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            value = value
                .checked_mul(10)
                .and_then(|n| n.checked_add(i64::from(digit - b'0')))
                .ok_or_else(|| Error::InvalidInput("Could not convert string to INT64".into()))?;
            self.advance()?;
        }
        if self.pos == start {
            return Err(malformed());
        }
        Ok(value)
    }
    fn fraction(&mut self) -> Result<f64> {
        if self.peek() != Some(b'.') {
            return Ok(0.0);
        }
        self.advance()?;
        let mut fraction = String::from("0.");
        let mut sticky = false;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            // Every binary64 rounding midpoint in [0,1] terminates within
            // 1075 decimal places. A bounded longer prefix plus a sticky digit
            // preserves which side of a midpoint an arbitrary tail occupies.
            if fraction.len() < 1102 {
                fraction.push(char::from(digit));
            } else {
                sticky |= digit != b'0';
            }
            self.advance()?;
        }
        if sticky {
            fraction.push('1');
        }
        fraction
            .parse()
            .map_err(|_| invalid("invalid interval fraction"))
    }
    fn unit(&mut self) -> Result<String> {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
            if self.pos - start >= 16 {
                return Err(unknown_unit(&self.text[start..self.pos]));
            }
            self.advance()?;
        }
        Ok(self.text[start..self.pos].to_owned())
    }
    fn double_digit(&mut self) -> Result<i64> {
        let Some(first @ b'0'..=b'9') = self.peek() else {
            return Err(malformed());
        };
        self.advance()?;
        let mut number = i64::from(first - b'0');
        if let Some(second @ b'0'..=b'9') = self.peek() {
            number = number * 10 + i64::from(second - b'0');
            self.advance()?;
        }
        if number >= 60 {
            return Err(malformed());
        }
        Ok(number)
    }
    fn clock(&mut self, hours: i64, hour_digits: usize) -> Result<i64> {
        if hour_digits > 9 {
            return Err(malformed());
        }
        self.advance()?; // colon
        let minutes = if self.peek().is_none() {
            0
        } else {
            self.double_digit()?
        };
        let seconds = if self.peek().is_none() {
            0
        } else {
            if self.peek() != Some(b':') {
                return Err(malformed());
            }
            self.advance()?;
            if self.peek().is_none() {
                0
            } else {
                self.double_digit()?
            }
        };
        let mut micros = 0_i64;
        if self.peek() == Some(b'.') {
            self.advance()?;
            let mut multiplier = 100_000;
            while let Some(digit @ b'0'..=b'9') = self.peek() {
                micros += i64::from(digit - b'0') * multiplier;
                multiplier /= 10;
                self.advance()?;
            }
        }
        // Core's non-strict interval clock consumes the final clock and ignores
        // its remaining suffix, including an apparent later unit or AGO.
        Ok(((hours * 60 + minutes) * 60 + seconds) * 1_000_000 + micros)
    }
}

#[derive(Default)]
struct Parts {
    months: i32,
    days: i32,
    micros: i64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn addition(number: i64, multiplier: i64) -> Result<i64> {
    number
        .checked_mul(multiplier)
        .ok_or_else(|| Error::OutOfRange("interval value is out of range".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fractional(fraction: f64, multiplier: i64) -> i64 {
    if fraction.abs() > 1e-10 {
        (fraction * multiplier as f64).round() as i64
    } else {
        0
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn component_overflow(component: usize) -> Error {
    Error::OutOfRange(
        if component == 0 {
            "interval value is out of range"
        } else {
            "interval fraction is out of range"
        }
        .into(),
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Parts {
    fn months(&mut self, number: i64, multiplier: i64, fraction: f64) -> Result<()> {
        for (component, value) in [
            addition(number, multiplier)?,
            fractional(fraction, multiplier),
        ]
        .into_iter()
        .enumerate()
        {
            let value = i32::try_from(value).map_err(|_| {
                Error::InvalidInput(format!("Type INT64 with value {value} can't be cast because the value is out of range for the destination type INT32"))
            })?;
            self.months = self
                .months
                .checked_add(value)
                .ok_or_else(|| component_overflow(component))?;
        }
        Ok(())
    }
    fn days(&mut self, number: i64, multiplier: i64, fraction: f64) -> Result<()> {
        for (component, value) in [
            addition(number, multiplier)?,
            fractional(fraction, multiplier),
        ]
        .into_iter()
        .enumerate()
        {
            let value = i32::try_from(value).map_err(|_| {
                Error::InvalidInput(format!("Type INT64 with value {value} can't be cast because the value is out of range for the destination type INT32"))
            })?;
            self.days = self
                .days
                .checked_add(value)
                .ok_or_else(|| component_overflow(component))?;
        }
        Ok(())
    }
    fn micros(&mut self, number: i64, multiplier: i64, fraction: f64) -> Result<()> {
        for (component, value) in [
            addition(number, multiplier)?,
            fractional(fraction, multiplier),
        ]
        .into_iter()
        .enumerate()
        {
            self.micros = self
                .micros
                .checked_add(value)
                .ok_or_else(|| component_overflow(component))?;
        }
        Ok(())
    }
    fn apply(&mut self, number: i64, fraction: f64, unit: &str) -> Result<()> {
        match unit.to_ascii_lowercase().as_str() {
            "year" | "yr" | "y" | "years" | "yrs" => self.months(number, 12, fraction),
            "decade" | "dec" | "decades" | "decs" => self.months(number, 120, fraction),
            "century" | "cent" | "centuries" | "c" => self.months(number, 1200, fraction),
            "millennium" | "mil" | "millenniums" | "millennia" | "mils" | "millenium" => {
                self.months(number, 12000, fraction)
            }
            "quarter" | "quarters" => {
                self.months(number, 3, 0.0)?;
                let months = (fraction * 3.0).trunc() as i64;
                self.months(months, 1, 0.0)?;
                self.days(0, 30, fraction * 3.0 - months as f64)
            }
            "month" | "mon" | "months" | "mons" => {
                self.months(number, 1, 0.0)?;
                let days = (fraction * 30.0).trunc() as i64;
                self.days(days, 1, 0.0)?;
                self.micros(0, MICROS_PER_DAY, fraction * 30.0 - days as f64)
            }
            "day" | "days" | "d" | "dayofmonth" => {
                self.days(number, 1, 0.0)?;
                self.micros(0, MICROS_PER_DAY, fraction)
            }
            "week" | "weeks" | "w" | "weekofyear" => {
                self.days(number, 7, 0.0)?;
                let days = (fraction * 7.0).trunc() as i64;
                self.days(days, 1, 0.0)?;
                self.micros(0, MICROS_PER_DAY, fraction * 7.0 - days as f64)
            }
            "hour" | "hr" | "hours" | "hrs" | "h" => self.micros(number, 3_600_000_000, fraction),
            "minute" | "min" | "minutes" | "mins" | "m" => {
                self.micros(number, 60_000_000, fraction)
            }
            "second" | "sec" | "seconds" | "secs" | "s" => self.micros(number, 1_000_000, fraction),
            "millisecond" | "milliseconds" | "ms" | "msec" | "msecs" | "msecond" | "mseconds" => {
                self.micros(number, 1000, fraction)
            }
            "microsecond" | "microseconds" | "us" | "usec" | "usecs" | "usecond" | "useconds" => {
                self.micros(number, 1, 0.0)
            }
            "epoch" | "dow" | "dayofweek" | "weekday" | "isodow" | "doy" | "dayofyear"
            | "yearweek" | "isoyear" | "era" | "timezone" | "julian" | "jd" => Err(invalid(
                &format!("extract specifier {unit:?} not supported for interval"),
            )),
            _ => Err(unknown_unit(unit)),
        }
    }
    fn negate(&mut self) -> Result<()> {
        self.months = self
            .months
            .checked_neg()
            .ok_or_else(|| Error::OutOfRange("AGO interval value is out of range".into()))?;
        self.days = self
            .days
            .checked_neg()
            .ok_or_else(|| Error::OutOfRange("AGO interval value is out of range".into()))?;
        self.micros = self
            .micros
            .checked_neg()
            .ok_or_else(|| Error::OutOfRange("AGO interval value is out of range".into()))?;
        Ok(())
    }
    fn value(self) -> TemporalValue {
        TemporalValue::Interval {
            months: self.months,
            days: self.days,
            micros: self.micros,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn parse(text: &str, check: &mut dyn FnMut() -> Result<()>) -> Result<TemporalValue> {
    check()?;
    let mut scan = Scanner {
        text,
        pos: 0,
        check,
    };
    if scan.peek() == Some(b'@') {
        scan.advance()?;
    }
    let mut parts = Parts::default();
    let mut found = false;
    loop {
        scan.space()?;
        if scan.peek().is_none() {
            break;
        }
        if matches!(scan.peek(), Some(b'a' | b'A')) {
            if !scan.unit()?.eq_ignore_ascii_case("ago") {
                return Err(malformed());
            }
            scan.space()?;
            if scan.peek().is_some() {
                return Err(malformed());
            }
            parts.negate()?;
            break;
        }
        let negative = scan.peek() == Some(b'-');
        if negative {
            scan.advance()?;
        }
        let number_start = scan.pos;
        let mut number = scan.integer()?;
        if scan.peek() == Some(b':') {
            let clock = scan.clock(number, scan.pos - number_start)?;
            parts.micros(if negative { -clock } else { clock }, 1, 0.0)?;
            found = true;
            break;
        }
        let mut fraction = scan.fraction()?;
        if negative {
            number = -number;
            fraction = -fraction;
        }
        scan.space()?;
        let unit = scan.unit()?;
        if unit.is_empty() && !found {
            parts.micros(number, 1_000_000, fraction)?;
            scan.space()?;
            if scan.peek().is_some() {
                return Err(malformed());
            }
            found = true;
            break;
        }
        parts.apply(number, fraction, &unit)?;
        found = true;
    }
    (scan.check)()?;
    if !found {
        return Err(malformed());
    }
    Ok(parts.value())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Error;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn interval_scanning_checks_cancellation_inside_each_unbounded_input_shape() {
        for input in [
            " ".repeat(100_000) + "1day",
            "0".repeat(100_000) + "1day",
            "0.".to_owned() + &"0".repeat(100_000) + "1 seconds",
            "0days ".repeat(20_000),
            "1:02:03.".to_owned() + &"0".repeat(100_000),
        ] {
            let mut checks = 0;
            let result = parse(&input, &mut || {
                checks += 1;
                if checks == 3 {
                    Err(Error::Interrupted)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(result, Err(Error::Interrupted)));
            assert_eq!(checks, 3);
        }
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn bounded_fraction_prefix_retains_sticky_midpoint_rounding() -> Result<()> {
        let midpoint = ".500000000000000055511151231257827021181583404541015625";
        for suffix in ["0".repeat(4096), "0".repeat(4096) + "1"] {
            let text = midpoint.to_owned() + &suffix;
            let mut check = || Ok(());
            let mut scan = Scanner {
                text: &text,
                pos: 0,
                check: &mut check,
            };
            let parsed = scan.fraction()?;
            assert_eq!(parsed.to_bits(), text.parse::<f64>().unwrap().to_bits());
            assert_eq!(scan.pos, text.len());
        }
        Ok(())
    }
}
