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
            _ => return Err(invalid_date()),
        };
        if day == 0 || day > length {
            return Err(invalid_date());
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
    pub(crate) fn parse_checked(text: &str, mut check: impl FnMut() -> Result<()>) -> Result<Self> {
        check()?;
        let bytes = text.as_bytes();
        let mut pos = 0;
        skip_space(bytes, &mut pos, &mut check)?;
        let negative = bytes.get(pos) == Some(&b'-');
        pos += usize::from(negative);
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
                    Ok(if negative { Self(-date.0) } else { date })
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
            year = year
                .checked_mul(10)
                .and_then(|n| n.checked_add(i32::from(*digit - b'0')))
                .ok_or_else(invalid_date)?;
            pos += 1;
        }
        if pos == start {
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
        skip_space(bytes, &mut pos, &mut check)?;
        if pos != bytes.len() {
            return Err(invalid_date());
        }
        Self::from_ymd(year, month, day)
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
