//! Constant-compiled core `strptime`; explicit numeric offsets, no ICU zones.
use super::formatting::format::{Format, Part};
use crate::{
    common::{DataType, Date, Error, NestedPayload, NestedType, Result, TemporalValue, Value},
    function::{
        ArgumentEvaluation, FunctionRegistry, ScalarBindArguments, ScalarFunction, ScalarSignature,
    },
    parallel::QueryContext,
};
use std::sync::Arc;

const MAX_FORMATS: usize = 65_536;
const MAX_FORMAT_BYTES: usize = 16 * 1024 * 1024;
const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
struct Strptime {
    name: &'static str,
    signature: Option<ScalarSignature>,
    formats: Option<Vec<Format>>,
    result: DataType,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["strptime", "try_strptime"] {
        registry
            .register_scalar(Arc::new(Strptime {
                name,
                signature: None,
                formats: None,
                result: DataType::Timestamp,
            }))
            .expect("unique temporal parsing function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn candidates() -> Vec<ScalarSignature> {
    let list = NestedType::List(DataType::Varchar).data_type();
    [DataType::Varchar, list]
        .into_iter()
        .map(|format| ScalarSignature {
            arguments: vec![DataType::Varchar, format],
            return_type: DataType::Timestamp,
            argument_names: Some(vec!["text".into(), "format".into()]),
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Strptime {
    fn name(&self) -> &str {
        self.name
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        // In particular, an input error is not hidden by a constant NULL format.
        ArgumentEvaluation::Eager
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        let candidates = candidates();
        ScalarSignature::validate_candidates(self.name, &candidates, query)?;
        let selected = arguments.select_overload(self.name, &candidates)?;
        let signature = ScalarSignature::selected(&candidates, selected)?.clone();
        if !arguments.is_closed(1)? {
            return Err(Error::Bind(format!(
                "The \"format\" argument in function \"{}\" must be a constant expression",
                self.name
            )));
        }
        let format_value = arguments.constant_as(
            1,
            &signature.arguments[1],
            crate::common::cast::CastMode::Implicit,
        )?;
        let formats = compile_formats(format_value, query)?;
        let result = formats.as_ref().map_or(DataType::Timestamp, |formats| {
            let nanos = formats.iter().any(|format| format.has('n'));
            let offset = formats.iter().any(|format| format.has('z'));
            match (nanos, offset) {
                (true, true) => DataType::TimestampTzNs,
                (true, false) => DataType::TimestampNs,
                (false, true) => DataType::TimestampTz,
                (false, false) => DataType::Timestamp,
            }
        });
        Ok(Some(Arc::new(Self {
            name: self.name,
            signature: Some(signature),
            formats,
            result,
        })))
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        let signature = self
            .signature
            .as_ref()
            .ok_or_else(|| Error::Unsupported("strptime requires selected binding".into()))?;
        if arguments.len() != 2 {
            return Err(Error::Internal("bound strptime arity".into()));
        }
        Ok(signature.arguments.clone())
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let signature = self
            .signature
            .as_ref()
            .ok_or_else(|| Error::Unsupported("strptime requires selected binding".into()))?;
        if arguments != signature.arguments {
            return Err(Error::Internal("bound strptime signature changed".into()));
        }
        Ok(self.result.clone())
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.len() != 2 {
            return Err(Error::Internal("strptime argument count".into()));
        }
        if arguments.iter().any(Value::is_null) || self.formats.is_none() {
            return Ok(Value::Null);
        }
        let Value::Varchar(text) = &arguments[0] else {
            return Err(Error::Internal("strptime text was not coerced".into()));
        };
        if text.len() > MAX_INPUT_BYTES {
            return Err(Error::Resource("strptime input byte limit".into()));
        }
        let formats = self.formats.as_ref().expect("checked formats");
        let mut last = None;
        for format in formats {
            query.check()?;
            match parse(format, text, &self.result, query)? {
                Ok(value) => return Ok(value),
                Err(error) => last = Some(error),
            }
        }
        if self.name == "try_strptime" {
            Ok(Value::Null)
        } else {
            let error = last.unwrap_or_else(|| ParseFailure::new(0, "No format matched"));
            Err(Error::InvalidInput(format!(
                "Could not parse string \"{text}\" according to format specifier \"{}\"\n{text}\n{}^\nError: {}",
                formats[0].text(),
                " ".repeat(error.position.min(4096)),
                error.message
            )))
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compile_formats(value: Value, query: &QueryContext) -> Result<Option<Vec<Format>>> {
    let texts = match value {
        Value::Null => return Ok(None),
        Value::Varchar(text) => vec![text],
        Value::Nested(value) => {
            let NestedPayload::Sequence(values) = &value.payload else {
                return Err(Error::Internal("strptime format list payload".into()));
            };
            if values.is_empty() {
                return Err(Error::InvalidInput(
                    "strptime format list must not be empty".into(),
                ));
            }
            if values.len() > MAX_FORMATS {
                return Err(Error::Resource("strptime format count limit".into()));
            }
            values
                .iter()
                .map(|value| match value {
                    Value::Varchar(text) => Ok(text.clone()),
                    // DuckDB's list binder calls Value::ToString on children.
                    Value::Null => Ok("NULL".into()),
                    _ => Err(Error::Internal("strptime list element type".into())),
                })
                .collect::<Result<Vec<_>>>()?
        }
        _ => return Err(Error::Internal("strptime format was not coerced".into())),
    };
    let mut total = 0_usize;
    let mut formats = Vec::new();
    formats
        .try_reserve(texts.len())
        .map_err(|_| Error::Resource("strptime format allocation".into()))?;
    for text in texts {
        query.check()?;
        total = total
            .checked_add(text.len())
            .filter(|total| *total <= MAX_FORMAT_BYTES)
            .ok_or_else(|| Error::Resource("strptime aggregate format byte limit".into()))?;
        formats.push(Format::compile(&text, query)?);
    }
    Ok(Some(formats))
}

#[derive(Debug)]
struct ParseFailure {
    position: usize,
    message: String,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ParseFailure {
    fn new(position: usize, message: impl Into<String>) -> Self {
        Self {
            position,
            message: message.into(),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Display for ParseFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DateOffset {
    None,
    Year,
    Calendar,
    YearDay,
    WeekSun,
    WeekMon,
    Iso,
}

#[derive(Debug)]
struct Parsed {
    year: i32,
    month: u8,
    day: u8,
    hour: i64,
    minute: i64,
    second: i64,
    nanos: i64,
    utc_offset: i64,
    ampm: Option<bool>,
    date_offset: DateOffset,
    week: i64,
    weekday: i64,
    has_weekday: bool,
    year_day: i64,
    iso_year: Option<i32>,
    iso_week: Option<i64>,
    iso_weekday: Option<i64>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for Parsed {
    fn default() -> Self {
        Self {
            year: 1900,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            nanos: 0,
            utc_offset: 0,
            ampm: None,
            date_offset: DateOffset::None,
            week: 0,
            weekday: 0,
            has_weekday: false,
            year_day: 0,
            iso_year: None,
            iso_week: None,
            iso_weekday: None,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse(
    format: &Format,
    text: &str,
    target: &DataType,
    query: &QueryContext,
) -> Result<std::result::Result<Value, ParseFailure>> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    skip_space(bytes, &mut pos, query)?;
    let special = &bytes[pos..];
    for (word, ticks) in [
        (b"infinity".as_slice(), i64::MAX),
        (b"-infinity".as_slice(), -i64::MAX),
        (b"epoch".as_slice(), 0),
    ] {
        if special.len() >= word.len() && special[..word.len()].eq_ignore_ascii_case(word) {
            let mut end = pos + word.len();
            skip_space(bytes, &mut end, query)?;
            if end == bytes.len() {
                return Ok(Ok(Value::Temporal(TemporalValue::from_ticks(
                    target, ticks,
                )?)));
            }
            return Ok(Err(ParseFailure::new(
                end,
                "Special timestamp did not match: trailing characters",
            )));
        }
    }
    let mut parsed = Parsed::default();
    for part in format.parts() {
        query.check()?;
        let result = match part {
            Part::Literal(literal) => match_literal(bytes, &mut pos, literal, query),
            Part::Directive(code, _) => parse_directive(bytes, &mut pos, *code, &mut parsed, query),
        };
        if let Err(error) = result {
            if error.message == "Interrupted" {
                return Err(Error::Interrupted);
            }
            return Ok(Err(error));
        }
    }
    skip_space(bytes, &mut pos, query)?;
    if pos != bytes.len() {
        return Ok(Err(ParseFailure::new(
            pos,
            "Full specifier did not match: trailing characters",
        )));
    }
    if let Some(pm) = parsed.ampm {
        if parsed.hour > 12 {
            return Ok(Err(ParseFailure::new(pos, "Invalid hour with AM/PM")));
        }
        if parsed.hour == 12 {
            parsed.hour = 0;
        }
        if pm {
            parsed.hour += 12;
        }
    }
    let date = match finish_date(&parsed) {
        Ok(date) => date,
        Err(error) => return Ok(Err(error)),
    };
    let precision = target
        .timestamp_precision()
        .ok_or_else(|| Error::Internal("strptime result is not timestamp".into()))?;
    let fraction = if precision == 1_000_000_000 {
        parsed.nanos
    } else {
        (parsed.nanos + 500) / 1000
    };
    let seconds = parsed.hour * 3600 + parsed.minute * 60 + parsed.second - parsed.utc_offset;
    let ticks = i128::from(date.days()) * i128::from(precision * 86_400)
        + i128::from(seconds) * i128::from(precision)
        + i128::from(fraction);
    let ticks = match i64::try_from(ticks) {
        Ok(ticks) if ticks.abs_diff(0) < i64::MAX as u64 => ticks,
        _ => {
            return Ok(Err(ParseFailure::new(
                pos,
                "Date and time not in timestamp range",
            )));
        }
    };
    Ok(Ok(Value::Temporal(TemporalValue::from_ticks(
        target, ticks,
    )?)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn finish_date(parsed: &Parsed) -> std::result::Result<Date, ParseFailure> {
    let date = match parsed.date_offset {
        DateOffset::Iso => {
            let year = parsed.iso_year.unwrap_or(1900);
            let week = parsed.iso_week.unwrap_or(1);
            let weekday = parsed.iso_weekday.unwrap_or(1);
            let jan4 = Date::from_ymd(year, 1, 4)
                .map_err(|error| ParseFailure::new(0, error.to_string()))?;
            let jan4_weekday = (i64::from(jan4.days()) + 3).rem_euclid(7);
            add_days(jan4, -jan4_weekday + (week - 1) * 7 + weekday - 1)?
        }
        DateOffset::WeekSun | DateOffset::WeekMon => {
            let jan1 = Date::from_ymd(parsed.year, 1, 1)
                .map_err(|error| ParseFailure::new(0, error.to_string()))?;
            let monday = i64::from(jan1.days()) - (i64::from(jan1.days()) + 3).rem_euclid(7);
            let mut start = monday - i64::from(parsed.date_offset == DateOffset::WeekSun);
            if start >= i64::from(jan1.days()) {
                start -= 7;
            }
            let weekday = if parsed.has_weekday {
                (parsed.weekday + 7 - i64::from(parsed.date_offset == DateOffset::WeekMon)) % 7
            } else {
                0
            };
            Date::from_days(
                i32::try_from(start + parsed.week * 7 + weekday)
                    .map_err(|_| ParseFailure::new(0, "DATE outside finite range"))?,
            )
            .map_err(|error| ParseFailure::new(0, error.to_string()))?
        }
        DateOffset::YearDay => {
            let jan1 = Date::from_ymd(parsed.year, 1, 1)
                .map_err(|error| ParseFailure::new(0, error.to_string()))?;
            add_days(jan1, parsed.year_day - 1)?
        }
        _ => Date::from_ymd(parsed.year, parsed.month, parsed.day)
            .map_err(|error| ParseFailure::new(0, error.to_string()))?,
    };
    Ok(date)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn add_days(date: Date, offset: i64) -> std::result::Result<Date, ParseFailure> {
    let days = i64::from(date.days()) + offset;
    Date::from_days(
        i32::try_from(days).map_err(|_| ParseFailure::new(0, "DATE outside finite range"))?,
    )
    .map_err(|error| ParseFailure::new(0, error.to_string()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_directive(
    bytes: &[u8],
    pos: &mut usize,
    code: char,
    parsed: &mut Parsed,
    query: &QueryContext,
) -> std::result::Result<(), ParseFailure> {
    let start = *pos;
    let numeric = match code {
        'w' | 'u' => Some(1),
        'd' | 'm' | 'y' | 'H' | 'I' | 'M' | 'S' | 'U' | 'W' | 'V' => Some(2),
        'g' | 'j' => Some(3),
        'Y' | 'G' => Some(4),
        'f' => Some(6),
        'n' => Some(9),
        _ => None,
    };
    if let Some(width) = numeric {
        let (number, digits) = number(bytes, pos, width, query)?;
        let range = |min, max, message: &str| {
            if (min..=max).contains(&number) {
                Ok(())
            } else {
                Err(ParseFailure::new(start, message))
            }
        };
        match code {
            'd' => {
                range(1, 31, "Day out of range, expected a value between 1 and 31")?;
                parsed.day = number as u8;
                parsed.date_offset = DateOffset::Calendar;
            }
            'm' => {
                range(
                    1,
                    12,
                    "Month out of range, expected a value between 1 and 12",
                )?;
                parsed.month = number as u8;
                parsed.date_offset = DateOffset::Calendar;
            }
            'y' => {
                parsed.year = if number >= 69 {
                    1900 + number as i32
                } else {
                    2000 + number as i32
                };
                if matches!(parsed.date_offset, DateOffset::None | DateOffset::Iso) {
                    parsed.date_offset = DateOffset::Year;
                }
            }
            'Y' => {
                parsed.year = number as i32;
                if matches!(parsed.date_offset, DateOffset::None | DateOffset::Iso) {
                    parsed.date_offset = DateOffset::Year;
                }
            }
            'G' => {
                range(0, 9999, "ISO Year out of range")?;
                match parsed.date_offset {
                    DateOffset::Year | DateOffset::Calendar => {}
                    DateOffset::None => parsed.date_offset = DateOffset::Iso,
                    DateOffset::Iso => {
                        if parsed.iso_year.is_some() {
                            return Err(ParseFailure::new(
                                start,
                                "Multiple ISO year offsets specified",
                            ));
                        }
                    }
                    _ => {
                        return Err(ParseFailure::new(
                            start,
                            "Incompatible ISO year offset specified",
                        ));
                    }
                }
                parsed.iso_year = Some(number as i32);
            }
            'H' => {
                range(
                    0,
                    23,
                    "Hour out of range, expected a value between 0 and 23",
                )?;
                parsed.hour = number;
            }
            'I' => {
                range(
                    1,
                    12,
                    "Hour12 out of range, expected a value between 1 and 12",
                )?;
                parsed.hour = number;
            }
            'M' => {
                range(
                    0,
                    59,
                    "Minutes out of range, expected a value between 0 and 59",
                )?;
                parsed.minute = number;
            }
            'S' => {
                range(
                    0,
                    59,
                    "Seconds out of range, expected a value between 0 and 59",
                )?;
                parsed.second = number;
            }
            'n' | 'f' | 'g' => {
                let width = match code {
                    'n' => 9,
                    'f' => 6,
                    _ => 3,
                };
                parsed.nanos =
                    number * 10_i64.pow((width - digits) as u32) * 10_i64.pow((9 - width) as u32);
            }
            'w' => {
                range(
                    0,
                    6,
                    "Weekday out of range, expected a value between 0 and 6",
                )?;
                parsed.weekday = number;
                parsed.has_weekday = true;
            }
            'u' => {
                range(
                    1,
                    7,
                    "ISO weekday offset out of range, expected a value between 1 and 7",
                )?;
                if parsed.iso_weekday.replace(number).is_some() {
                    return Err(ParseFailure::new(
                        start,
                        "Multiple ISO weekday offsets specified",
                    ));
                }
            }
            'U' | 'W' => {
                range(
                    0,
                    53,
                    "Week out of range, expected a value between 0 and 53",
                )?;
                match parsed.date_offset {
                    DateOffset::Calendar => {}
                    DateOffset::None | DateOffset::Year => {
                        parsed.date_offset = if code == 'U' {
                            DateOffset::WeekSun
                        } else {
                            DateOffset::WeekMon
                        };
                    }
                    _ => {
                        return Err(ParseFailure::new(start, "Multiple week offsets specified"));
                    }
                }
                parsed.week = number;
            }
            'V' => {
                range(
                    1,
                    53,
                    "ISO week offset out of range, expected a value between 1 and 53",
                )?;
                match parsed.date_offset {
                    DateOffset::Calendar => {}
                    DateOffset::None => parsed.date_offset = DateOffset::Iso,
                    DateOffset::Iso => {
                        if parsed.iso_week.is_some() {
                            return Err(ParseFailure::new(
                                start,
                                "Multiple ISO week offsets specified",
                            ));
                        }
                    }
                    DateOffset::Year => {
                        return Err(ParseFailure::new(
                            start,
                            "ISO week offsets are incompatible with non-ISO year specifiers. Use '%G' instead",
                        ));
                    }
                    _ => {
                        return Err(ParseFailure::new(
                            start,
                            "Incompatible ISO week offset specified",
                        ));
                    }
                }
                parsed.iso_week = Some(number);
            }
            'j' => {
                range(
                    1,
                    366,
                    "Year day out of range, expected a value between 1 and 366",
                )?;
                match parsed.date_offset {
                    DateOffset::Calendar => {}
                    DateOffset::None | DateOffset::Year => {
                        parsed.date_offset = DateOffset::YearDay;
                    }
                    _ => {
                        return Err(ParseFailure::new(start, "Multiple year offsets specified"));
                    }
                }
                parsed.year_day = number;
            }
            _ => return Err(ParseFailure::new(start, "Unsupported numeric specifier")),
        }
        return Ok(());
    }
    match code {
        'p' => {
            let slice = bytes
                .get(*pos..*pos + 2)
                .ok_or_else(|| ParseFailure::new(*pos, "Expected AM/PM"))?;
            parsed.ampm = if slice.eq_ignore_ascii_case(b"am") {
                Some(false)
            } else if slice.eq_ignore_ascii_case(b"pm") {
                Some(true)
            } else {
                return Err(ParseFailure::new(*pos, "Expected AM/PM"));
            };
            *pos += 2;
        }
        'a' => {
            collection(
                bytes,
                pos,
                &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
            )?;
        }
        'A' => {
            collection(
                bytes,
                pos,
                &[
                    "Sunday",
                    "Monday",
                    "Tuesday",
                    "Wednesday",
                    "Thursday",
                    "Friday",
                    "Saturday",
                ],
            )?;
        }
        'b' | 'h' => {
            parsed.month = collection(
                bytes,
                pos,
                &[
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec",
                ],
            )? as u8
                + 1;
        }
        'B' => {
            parsed.month = collection(
                bytes,
                pos,
                &[
                    "January",
                    "February",
                    "March",
                    "April",
                    "May",
                    "June",
                    "July",
                    "August",
                    "September",
                    "October",
                    "November",
                    "December",
                ],
            )? as u8
                + 1;
        }
        'z' => parsed.utc_offset = utc_offset(bytes, pos)?,
        'Z' => {
            while bytes.get(*pos).is_some_and(u8::is_ascii_whitespace) {
                if (*pos).is_multiple_of(1024) && query.check().is_err() {
                    return Err(ParseFailure::new(*pos, "Interrupted"));
                }
                *pos += 1;
            }
            let start = *pos;
            while bytes.get(*pos).is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'/' | b'+' | b'-' | b':')
            }) {
                if (*pos).is_multiple_of(1024) && query.check().is_err() {
                    return Err(ParseFailure::new(*pos, "Interrupted"));
                }
                *pos += 1;
            }
            if *pos == start {
                return Err(ParseFailure::new(start, "Empty Time Zone name"));
            }
        }
        _ => return Err(ParseFailure::new(start, "Unsupported strptime specifier")),
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn number(
    bytes: &[u8],
    pos: &mut usize,
    width: usize,
    query: &QueryContext,
) -> std::result::Result<(i64, usize), ParseFailure> {
    let start = *pos;
    let mut value = 0_i64;
    while *pos < bytes.len() && *pos - start < width && bytes[*pos].is_ascii_digit() {
        if (*pos).is_multiple_of(1024) && query.check().is_err() {
            return Err(ParseFailure::new(*pos, "Interrupted"));
        }
        value = value * 10 + i64::from(bytes[*pos] - b'0');
        *pos += 1;
    }
    if *pos == start {
        Err(ParseFailure::new(start, "Expected a number"))
    } else {
        Ok((value, *pos - start))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn collection(
    bytes: &[u8],
    pos: &mut usize,
    values: &[&str],
) -> std::result::Result<usize, ParseFailure> {
    for (index, value) in values.iter().enumerate() {
        if bytes
            .get(*pos..*pos + value.len())
            .is_some_and(|slice| slice.eq_ignore_ascii_case(value.as_bytes()))
        {
            *pos += value.len();
            return Ok(index);
        }
    }
    Err(ParseFailure::new(*pos, "Expected a matching name"))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn utc_offset(bytes: &[u8], pos: &mut usize) -> std::result::Result<i64, ParseFailure> {
    let start = *pos;
    let sign = match bytes.get(*pos) {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => {
            return Err(ParseFailure::new(
                start,
                "Expected ±HH[MM] or -HH[:MM[:SS]]",
            ));
        }
    };
    *pos += 1;
    let hour = two_digits(bytes, pos)
        .ok_or_else(|| ParseFailure::new(start, "Expected ±HH[MM] or -HH[:MM[:SS]]"))?;
    let colon = bytes.get(*pos) == Some(&b':');
    if colon {
        *pos += 1;
    }
    let minute = match two_digits(bytes, pos) {
        Some(minute) => minute,
        None if colon => {
            return Err(ParseFailure::new(
                start,
                "Expected ±HH[MM] or -HH[:MM[:SS]]",
            ));
        }
        None => 0,
    };
    if colon && bytes.get(*pos) == Some(&b':') {
        *pos += 1;
        let second = two_digits(bytes, pos)
            .ok_or_else(|| ParseFailure::new(start, "Expected ±HH[MM] or -HH[:MM[:SS]]"))?;
        return Ok(i64::from(sign) * (hour * 3600 + minute * 60 + second));
    }
    Ok(i64::from(sign) * (hour * 3600 + minute * 60))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn two_digits(bytes: &[u8], pos: &mut usize) -> Option<i64> {
    let first = *bytes.get(*pos)?;
    let second = *bytes.get(*pos + 1)?;
    if !first.is_ascii_digit() || !second.is_ascii_digit() {
        return None;
    }
    *pos += 2;
    Some(i64::from(first - b'0') * 10 + i64::from(second - b'0'))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn match_literal(
    bytes: &[u8],
    pos: &mut usize,
    literal: &str,
    query: &QueryContext,
) -> std::result::Result<(), ParseFailure> {
    let literal = literal.as_bytes();
    let mut index = 0;
    while index < literal.len() {
        if index.is_multiple_of(1024) && query.check().is_err() {
            return Err(ParseFailure::new(*pos, "Interrupted"));
        }
        if literal[index].is_ascii_whitespace() {
            if !bytes.get(*pos).is_some_and(u8::is_ascii_whitespace) {
                return Err(ParseFailure::new(*pos, "Space does not match"));
            }
            while bytes.get(*pos).is_some_and(u8::is_ascii_whitespace) {
                *pos += 1;
            }
            while literal.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
            }
        } else if bytes.get(*pos) == Some(&literal[index]) {
            *pos += 1;
            index += 1;
        } else {
            return Err(ParseFailure::new(*pos, "Literal does not match"));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn skip_space(bytes: &[u8], pos: &mut usize, query: &QueryContext) -> Result<()> {
    while bytes.get(*pos).is_some_and(u8::is_ascii_whitespace) {
        if (*pos).is_multiple_of(1024) {
            query.check()?;
        }
        *pos += 1;
    }
    Ok(())
}
