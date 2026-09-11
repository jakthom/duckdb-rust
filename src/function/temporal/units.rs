//! Core calendar specifier aliases shared by period differences and truncation.
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum Unit {
    Year,
    Month,
    Day,
    Decade,
    Century,
    Millennium,
    Quarter,
    Week,
    IsoYear,
    Microsecond,
    Millisecond,
    Second,
    Minute,
    Hour,
    Unsupported,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Unit {
    pub(super) fn parse(text: &str) -> Result<Self> {
        // Length-mismatched aliases return immediately, without allocating or
        // scanning an unbounded string merely to discover an invalid unit.
        for (unit, aliases) in [
            (Self::Year, &["year", "yr", "y", "years", "yrs"][..]),
            (Self::Month, &["month", "mon", "months", "mons"]),
            (
                Self::Day,
                &[
                    "day",
                    "days",
                    "d",
                    "dayofmonth",
                    "dow",
                    "dayofweek",
                    "weekday",
                    "isodow",
                    "doy",
                    "dayofyear",
                    "julian",
                    "jd",
                ],
            ),
            (Self::Decade, &["decade", "dec", "decades", "decs"]),
            (Self::Century, &["century", "cent", "centuries", "c"]),
            (
                Self::Millennium,
                &[
                    "millennium",
                    "mil",
                    "millenniums",
                    "millennia",
                    "mils",
                    "millenium",
                ],
            ),
            (
                Self::Microsecond,
                &[
                    "microseconds",
                    "microsecond",
                    "us",
                    "usec",
                    "usecs",
                    "usecond",
                    "useconds",
                ],
            ),
            (
                Self::Millisecond,
                &[
                    "milliseconds",
                    "millisecond",
                    "ms",
                    "msec",
                    "msecs",
                    "msecond",
                    "mseconds",
                ],
            ),
            (
                Self::Second,
                &["second", "sec", "seconds", "secs", "s", "epoch"],
            ),
            (Self::Minute, &["minute", "min", "minutes", "mins", "m"]),
            (Self::Hour, &["hour", "hr", "hours", "hrs", "h"]),
            (
                Self::Week,
                &["week", "weeks", "w", "weekofyear", "yearweek"],
            ),
            (Self::Quarter, &["quarter", "quarters"]),
            (Self::IsoYear, &["isoyear"]),
            (
                Self::Unsupported,
                &["era", "timezone", "timezone_hour", "timezone_minute"],
            ),
        ] {
            if aliases.iter().any(|alias| text.eq_ignore_ascii_case(alias)) {
                return Ok(unit);
            }
        }
        let mut diagnostic: String = text.chars().take(128).collect();
        if diagnostic.len() < text.len() {
            diagnostic.push_str("...");
        }
        Err(Error::Conversion(format!(
            "extract specifier \"{diagnostic}\" not recognized"
        )))
    }
}
