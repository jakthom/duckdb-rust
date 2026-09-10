use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use super::{Error, Result};

/// A proleptic Gregorian date, stored as days since 1970-01-01. Year zero is
/// 1 BC. The two infinity values are ordered beyond every finite date; the
/// physical INT32_MIN NULL sentinel is never a valid non-NULL date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
pub struct Date(i32);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Date {
    pub const EPOCH: Self = Self(0);
    pub const NEG_INFINITY: Self = Self(-i32::MAX);
    pub const INFINITY: Self = Self(i32::MAX);

    pub fn from_days(days: i32) -> Result<Self> {
        if days == i32::MIN {
            return Err(Error::Conversion(
                "reserved DATE NULL representation".into(),
            ));
        }
        Ok(Self(days))
    }

    pub const fn days(self) -> i32 {
        self.0
    }

    pub fn is_finite(self) -> bool {
        self != Self::NEG_INFINITY && self != Self::INFINITY
    }

    pub fn from_ymd(year: i32, month: u8, day: u8) -> Result<Self> {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let length = match month {
            2 => {
                if leap {
                    29
                } else {
                    28
                }
            }
            4 | 6 | 9 | 11 => 30,
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            _ => return Err(Error::Conversion("date field value out of range".into())),
        };
        if day == 0 || day > length {
            return Err(Error::Conversion("date field value out of range".into()));
        }
        // March starts the computational year, keeping leap days at its end.
        let year = i64::from(year) - i64::from(month <= 2);
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let month = i64::from(month) + if month > 2 { -3 } else { 9 };
        let day_of_year = (153 * month + 2) / 5 + i64::from(day) - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days = era * 146_097 + day_of_era - 719_468;
        if !(-2_147_483_646..=2_147_483_646).contains(&days) {
            return Err(Error::Conversion("DATE outside finite range".into()));
        }
        Ok(Self(days as i32))
    }

    pub fn to_ymd(self) -> Option<(i32, u8, u8)> {
        if !self.is_finite() {
            return None;
        }
        let days = i64::from(self.0) + 719_468;
        let era = days.div_euclid(146_097);
        let day_of_era = days - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month + 2) / 5 + 1;
        let month = month + if month < 10 { 3 } else { -9 };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        Some((year as i32, month as u8, day as u8))
    }

    /// Calendar text uses matching -, /, backslash or space separators,
    /// optional (BC), and ASCII whitespace. Timestamp suffixes are not parsed.
    pub(crate) fn parse_checked(text: &str, check: impl FnMut() -> Result<()>) -> Result<Self> {
        Self::parse_full_checked(text, false, check)
    }

    /// Strict source conversion additionally requires at least two year
    /// digits. Neither calendar-only entry point accepts a timestamp suffix.
    pub(crate) fn parse_strict_checked(
        text: &str,
        check: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        Self::parse_full_checked(text, true, check)
    }

    fn parse_full_checked(
        text: &str,
        strict: bool,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        let (date, mut pos) = Self::parse_prefix_with_policy_checked(text, strict, &mut check)?;
        skip_space(text.as_bytes(), &mut pos, &mut check)?;
        check()?;
        if pos != text.len() {
            return Err(invalid_date());
        }
        Ok(date)
    }

    /// Parse a calendar prefix while retaining the exact first suffix byte.
    /// Timestamp parsing and selected SQL casts own suffix validation;
    /// `parse_checked` remains fully consuming. Special values stay strict.
    pub(crate) fn parse_prefix_checked(
        text: &str,
        check: impl FnMut() -> Result<()>,
    ) -> Result<(Self, usize)> {
        Self::parse_prefix_with_policy_checked(text, false, check)
    }

    fn parse_prefix_with_policy_checked(
        text: &str,
        strict: bool,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<(Self, usize)> {
        check()?;
        let bytes = text.as_bytes();
        let mut pos = 0;
        skip_space(bytes, &mut pos, &mut check)?;
        let negative = bytes.get(pos) == Some(&b'-');
        pos += usize::from(negative);
        // Core accepts the abbreviation only at the actual end of input;
        // `inf ` is not the same grammar as the full `infinity ` spelling.
        if bytes[pos..].eq_ignore_ascii_case(b"inf") {
            check()?;
            return Ok((
                if negative {
                    Self::NEG_INFINITY
                } else {
                    Self::INFINITY
                },
                bytes.len(),
            ));
        }
        for (word, date) in [
            (b"infinity".as_slice(), Self::INFINITY),
            (b"epoch".as_slice(), Self::EPOCH),
        ] {
            if bytes
                .get(pos..pos + word.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(word))
            {
                pos += word.len();
                skip_space(bytes, &mut pos, &mut check)?;
                return if pos == bytes.len() {
                    Ok((if negative { Self(-date.0) } else { date }, pos))
                } else {
                    Err(invalid_date())
                };
            }
        }
        let start = pos;
        let mut year = 0_i32;
        while let Some(digit) = bytes.get(pos).filter(|b| b.is_ascii_digit()) {
            if pos.is_multiple_of(1024) {
                check()?;
            }
            if year >= 100_000_000 {
                return Err(Error::Conversion("date field value out of range".into()));
            }
            year = year
                .checked_mul(10)
                .and_then(|n| n.checked_add(i32::from(*digit - b'0')))
                .ok_or_else(invalid_date)?;
            pos += 1;
        }
        if pos == start || (strict && pos - start < 2) {
            return Err(invalid_date());
        }
        let separator = *bytes.get(pos).ok_or_else(invalid_date)?;
        if !matches!(separator, b'-' | b'/' | b'\\' | b' ') {
            return Err(invalid_date());
        }
        pos += 1;
        let month = double_digit(bytes, &mut pos)?;
        if bytes.get(pos) != Some(&separator) {
            return Err(invalid_date());
        }
        pos += 1;
        let day = double_digit(bytes, &mut pos)?;
        if negative {
            year = -year;
        }
        if bytes.get(pos).is_some_and(u8::is_ascii_whitespace)
            && bytes
                .get(pos + 1..pos + 5)
                .is_some_and(|s| s.eq_ignore_ascii_case(b"(BC)"))
        {
            if negative || year == 0 {
                return Err(invalid_date());
            }
            year = 1 - year;
            pos += 5;
        }
        if bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            return Err(invalid_date());
        }
        check()?;
        Ok((Self::from_ymd(year, month, day)?, pos))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn invalid_date() -> Error {
    Error::Conversion("invalid DATE calendar text".into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn skip_space(bytes: &[u8], pos: &mut usize, check: &mut impl FnMut() -> Result<()>) -> Result<()> {
    while bytes.get(*pos).is_some_and(u8::is_ascii_whitespace) {
        if (*pos).is_multiple_of(1024) {
            check()?;
        }
        *pos += 1;
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn double_digit(bytes: &[u8], pos: &mut usize) -> Result<u8> {
    let digit = bytes
        .get(*pos)
        .filter(|b| b.is_ascii_digit())
        .ok_or_else(invalid_date)?;
    let mut value = *digit - b'0';
    *pos += 1;
    if let Some(digit) = bytes.get(*pos).filter(|b| b.is_ascii_digit()) {
        value = value * 10 + *digit - b'0';
        *pos += 1;
    }
    Ok(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FromStr for Date {
    type Err = Error;
    fn from_str(text: &str) -> Result<Self> {
        Self::parse_checked(text, || Ok(()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TryFrom<i32> for Date {
    type Error = Error;
    fn try_from(days: i32) -> Result<Self> {
        Self::from_days(days)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl From<Date> for i32 {
    fn from(date: Date) -> Self {
        date.days()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((year, month, day)) = self.to_ymd() {
            if year <= 0 {
                write!(f, "{:04}-{month:02}-{day:02} (BC)", 1 - year)
            } else {
                write!(f, "{year:04}-{month:02}-{day:02}")
            }
        } else if *self == Self::INFINITY {
            f.write_str("infinity")
        } else {
            f.write_str("-infinity")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn date_prefix_keeps_suffix_boundaries_and_full_date_contract() -> Result<()> {
        for (text, expected, suffix) in [
            ("2000 01 02 12:34:56", "2000-01-02", " 12:34:56"),
            ("0001-01-01 (BC)T00:00:00", "0001-01-01 (BC)", "T00:00:00"),
            ("\t2024/1/2\t", "2024-01-02", "\t"),
        ] {
            let (date, pos) = Date::parse_prefix_checked(text, || Ok(()))?;
            assert_eq!(date.to_string(), expected);
            assert_eq!(&text[pos..], suffix);
            assert_eq!(
                Date::parse_checked(text, || Ok(())).is_ok(),
                suffix.trim().is_empty()
            );
        }
        for text in ["2000-01-011", "epoch 12:00", "infinityT00:00"] {
            assert!(Date::parse_prefix_checked(text, || Ok(())).is_err());
        }
        let input = " ".repeat(100_000) + "2000-01-01";
        let mut checks = 0;
        assert!(matches!(
            Date::parse_prefix_checked(&input, || {
                checks += 1;
                if checks == 3 {
                    Err(Error::Interrupted)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Interrupted)
        ));
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn date_special_abbreviations_keep_exact_input_boundaries_and_range_categories() -> Result<()> {
        for (text, expected) in [
            ("inf", Date::INFINITY),
            ("\tINF", Date::INFINITY),
            ("-InF", Date::NEG_INFINITY),
            ("infinity \t", Date::INFINITY),
            (" -INFINITY\n", Date::NEG_INFINITY),
        ] {
            assert_eq!(Date::parse_checked(text, || Ok(()))?, expected);
        }
        for text in [
            "inf ",
            "-inf\t",
            "infi",
            "infin",
            "+inf",
            "infT00:00",
            "infinityT00:00",
        ] {
            assert!(Date::parse_checked(text, || Ok(())).is_err(), "{text}");
        }
        for text in ["1900-02-29", "2000-00-01", "2000-01-32"] {
            assert!(
                matches!(Date::parse_checked(text, || Ok(())), Err(Error::Conversion(message)) if message == "date field value out of range")
            );
        }
        assert!(matches!(
            Date::parse_checked("inf", || Err(Error::Interrupted)),
            Err(Error::Interrupted)
        ));
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn strict_calendar_year_policy_keeps_full_consumption_and_checked_scanning() -> Result<()> {
        for text in ["1-1-1", "-1-1-1", "1-1-1 (BC)"] {
            assert!(Date::parse_checked(text, || Ok(())).is_ok());
            assert!(Date::parse_strict_checked(text, || Ok(())).is_err());
        }
        for text in ["01-1-1", "-01-1-1", "01-1-1 (BC)", "epoch", "infinity "] {
            assert_eq!(
                Date::parse_checked(text, || Ok(()))?,
                Date::parse_strict_checked(text, || Ok(()))?
            );
        }
        for text in ["01-1-1 12:34:56", "5881580-07-10 24:00:00"] {
            assert!(Date::parse_checked(text, || Ok(())).is_err());
            assert!(Date::parse_strict_checked(text, || Ok(())).is_err());
        }
        for text in [
            " ".repeat(100_000) + "01-1-1",
            "0".repeat(100_000) + "1-1-1",
            "01-1-1".to_owned() + &" ".repeat(100_000),
        ] {
            let mut checks = 0;
            let result = Date::parse_strict_checked(&text, || {
                checks += 1;
                if checks == 8 {
                    Err(Error::Interrupted)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(result, Err(Error::Interrupted)));
            assert_eq!(checks, 8);
        }
        for text in ["1000000000-01-01", "2147483648-01-01"] {
            assert!(
                matches!(Date::parse_checked(text,|| Ok(())),Err(Error::Conversion(message)) if message == "date field value out of range")
            );
        }
        Ok(())
    }
}
