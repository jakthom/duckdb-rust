use super::super::truncation::calendar_date;
use super::*;

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_PARTS: usize = 65_536;
const MONTHS: [&str; 12] = [
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
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

#[derive(Clone, Debug)]
pub(in crate::function::temporal) enum Part {
    Literal(String),
    Directive(char, bool),
}

#[derive(Clone, Debug)]
pub(in crate::function::temporal) struct Format {
    text: String,
    parts: Vec<Part>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Format {
    pub(in crate::function::temporal) fn compile(text: &str, query: &QueryContext) -> Result<Self> {
        query.check()?;
        if text.len() > MAX_BYTES {
            return Err(Error::Resource("temporal format byte limit".into()));
        }
        let invalid = |reason: String| {
            Error::InvalidInput(format!("Failed to parse format specifier {text}: {reason}"))
        };
        if text.is_empty() {
            return Err(invalid("Empty format string".into()));
        }
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = text.char_indices();
        let mut examined = 0_usize;
        while let Some((_, ch)) = chars.next() {
            if examined.is_multiple_of(1024) {
                query.check()?;
            }
            examined += 1;
            if ch != '%' {
                literal.push(ch);
                continue;
            }
            let Some((_, mut code)) = chars.next() else {
                return Err(invalid("Trailing format character %".into()));
            };
            if code == '%' {
                literal.push('%');
                continue;
            }
            let padded = code != '-' || chars.clone().next().is_none();
            if !padded {
                code = chars.next().expect("checked format suffix").1;
            }
            if (!padded && !"dmyHIMSj".contains(code))
                || (padded && !"aAwudhbBmyYGHIpMSnfgzZjUWVcxXT".contains(code))
            {
                return Err(invalid(format!(
                    "Unrecognized format for strftime/strptime: %{}{code}",
                    if padded { "" } else { "-" }
                )));
            }
            if !literal.is_empty() {
                parts.push(Part::Literal(std::mem::take(&mut literal)));
            }
            if matches!(code, 'c' | 'x' | 'X' | 'T') {
                let expanded = match code {
                    'c' => "%Y-%m-%d %H:%M:%S",
                    'x' => "%Y-%m-%d",
                    _ => "%H:%M:%S",
                };
                parts.extend(Self::compile(expanded, query)?.parts);
            } else {
                parts.push(Part::Directive(code, padded));
            }
            if parts.len() > MAX_PARTS {
                return Err(Error::Resource("temporal format part limit".into()));
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        if parts.len() > MAX_PARTS {
            return Err(Error::Resource("temporal format part limit".into()));
        }
        Ok(Self {
            text: text.into(),
            parts,
        })
    }
    pub(in crate::function::temporal) fn text(&self) -> &str {
        &self.text
    }
    pub(in crate::function::temporal) fn parts(&self) -> &[Part] {
        &self.parts
    }
    pub(in crate::function::temporal) fn has(&self, directive: char) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, Part::Directive(code, _) if *code == directive))
    }
    pub(super) fn render(&self, value: &Value, query: &QueryContext) -> Result<String> {
        query.check()?;
        let (date, nanos) = match value {
            Value::Date(date) => (*date, 0),
            Value::Temporal(value) if value.data_type().timestamp_precision().is_some() => {
                value.check_text_renderable()?;
                let precision = value
                    .data_type()
                    .timestamp_precision()
                    .expect("bound timestamp");
                (
                    value.date()?,
                    value.ticks()?.rem_euclid(precision * 86400) * (1_000_000_000 / precision),
                )
            }
            _ => {
                return Err(Error::Internal(
                    "strftime expected bound temporal value".into(),
                ));
            }
        };
        if !date.is_finite() {
            return Ok(if date.days() < 0 {
                "-infinity"
            } else {
                "infinity"
            }
            .into());
        }
        let (year, month, day) = date
            .to_ymd()
            .ok_or_else(|| Error::Internal("finite formatting date".into()))?;
        let hour = nanos / 3_600_000_000_000;
        let minute = nanos / 60_000_000_000 % 60;
        let second = nanos / 1_000_000_000 % 60;
        let fraction = nanos % 1_000_000_000;
        let dow = (i64::from(date.days()) + 4).rem_euclid(7) as usize;
        let mut output = String::new();
        for part in &self.parts {
            query.check()?;
            let piece = match part {
                Part::Literal(text) => text.clone(),
                Part::Directive(code, padded) => {
                    let number = |value: i64, width: usize| {
                        if *padded {
                            format!("{value:0width$}")
                        } else {
                            value.to_string()
                        }
                    };
                    match code {
                        'a' => DAYS[dow][..3].into(),
                        'A' => DAYS[dow].into(),
                        'w' => dow.to_string(),
                        'u' => ((dow + 6) % 7 + 1).to_string(),
                        'd' => number(i64::from(day), 2),
                        'b' | 'h' => MONTHS[usize::from(month) - 1][..3].into(),
                        'B' => MONTHS[usize::from(month) - 1].into(),
                        'm' => number(i64::from(month), 2),
                        'y' => number(i64::from(year).abs() % 100, 2),
                        'Y' => {
                            if (0..=9999).contains(&year) {
                                format!("{year:04}")
                            } else {
                                year.to_string()
                            }
                        }
                        // Source writes the last four decimal digits of a u32
                        // ISO year even for BCE/large years; retain that witness.
                        'G' => format!("{:04}", (iso_year_week(date)?.0 as u32) % 10_000),
                        'H' => number(hour, 2),
                        'I' => number((hour + 11) % 12 + 1, 2),
                        'p' => if hour >= 12 { "PM" } else { "AM" }.into(),
                        'M' => number(minute, 2),
                        'S' => number(second, 2),
                        'n' => format!("{fraction:09}"),
                        'f' => format!("{:06}", fraction / 1000),
                        'g' => format!("{:03}", fraction / 1_000_000),
                        'z' => "+00".into(),
                        'Z' => String::new(),
                        'j' => number(day_of_year(year, month, day), 3),
                        'U' | 'W' => {
                            let jan1 = calendar_date(year, 1, 1)?;
                            let weekday = (i64::from(jan1.days()) + 3).rem_euclid(7) + 1;
                            let start = if *code == 'U' {
                                7 - weekday
                            } else {
                                (8 - weekday) % 7
                            };
                            let ordinal = day_of_year(year, month, day) - 1;
                            format!(
                                "{:02}",
                                if ordinal < start {
                                    0
                                } else {
                                    (ordinal - start) / 7 + 1
                                }
                            )
                        }
                        'V' => format!("{:02}", iso_year_week(date)?.1),
                        _ => {
                            return Err(Error::Internal(
                                "compiled temporal format directive".into(),
                            ));
                        }
                    }
                }
            };
            if output
                .len()
                .checked_add(piece.len())
                .is_none_or(|len| len > MAX_BYTES)
            {
                return Err(Error::Resource(
                    "formatted temporal output byte limit".into(),
                ));
            }
            output
                .try_reserve(piece.len())
                .map_err(|_| Error::Resource("formatted temporal output allocation".into()))?;
            output.push_str(&piece);
        }
        Ok(output)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn day_of_year(year: i32, month: u8, day: u8) -> i64 {
    let cumulative = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    cumulative[usize::from(month) - 1]
        + i64::from(day)
        + i64::from(month > 2 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn iso_week_one(year: i32) -> Result<i64> {
    let jan1 = i64::from(calendar_date(year, 1, 1)?.days());
    let weekday = (jan1 + 3).rem_euclid(7);
    Ok(jan1 - weekday + if weekday > 3 { 7 } else { 0 })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn iso_year_week(date: Date) -> Result<(i32, i64)> {
    let mut year = date
        .to_ymd()
        .ok_or_else(|| Error::Internal("finite ISO format date".into()))?
        .0;
    let days = i64::from(date.days());
    let mut week = (days - iso_week_one(year)?).div_euclid(7);
    if week < 0 {
        year -= 1;
        week = (days - iso_week_one(year)?).div_euclid(7);
    } else if week >= 52 && days >= iso_week_one(year + 1)? {
        year += 1;
        week = 0;
    }
    Ok((year, week + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn compiled_format_bounds_include_final_literals_and_cancel_long_unicode_scans() -> Result<()> {
        let query = QueryContext::background();
        assert!(matches!(
            Format::compile(&"x".repeat(MAX_BYTES + 1), &query),
            Err(Error::Resource(_))
        ));
        let exact = "%Y".repeat(MAX_PARTS);
        Format::compile(&exact, &query)?;
        assert!(matches!(
            Format::compile(&(exact + "literal"), &query),
            Err(Error::Resource(_))
        ));
        let interrupt = crate::parallel::InterruptHandle::default();
        let query = QueryContext::new(interrupt.clone(), None, 2, 100)?;
        interrupt.interrupt();
        assert!(matches!(
            Format::compile(&format!("a{}", "é".repeat(100_000)), &query),
            Err(Error::Interrupted)
        ));
        assert!(matches!(
            Format::compile("%Y", &query),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
}
