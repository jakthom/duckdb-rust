use super::*;
use crate::common::numeric::{decimal, rescale};

#[derive(Debug)]
pub struct ExactNumericCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for ExactNumericCast {
    fn name(&self) -> &'static str {
        "exact-numeric-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        let (a, b) = (&spec.source, &spec.target);
        if !(a.is_numeric() || matches!(a, DataType::Null | DataType::Boolean | DataType::Varchar))
            || !(b.is_numeric() || matches!(b, DataType::Boolean | DataType::Varchar))
        {
            return false;
        }
        if a == b || *a == DataType::Null {
            return true;
        }
        if spec.mode != CastMode::Implicit {
            return true;
        }
        if a.is_decimal() {
            return b.is_decimal() || b.is_floating();
        }
        if a.is_integer() && b.is_decimal() {
            return true;
        }
        if a.is_integer() && b.is_floating() {
            return true;
        }
        if let Some(bits) = a.unsigned_bits() {
            return b.unsigned_bits().is_some_and(|to| to > bits)
                || b.integer_bits().is_some_and(|to| to > bits);
        }
        false
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        self.convert(value, spec, query).map_err(|error| match error {
            Error::Conversion(_) => Error::Conversion(match value {
                Value::Varchar(text) => format!("Could not convert string \"{text}\" to {}", spec.target),
                _ if spec.target.is_decimal() => format!("Could not cast value {value} to {}", spec.target),
                _ => format!("Type {} with value {value} can't be cast because the value is out of range for the destination type {}", spec.source, spec.target),
            }),
            other => other,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExactNumericCast {
    fn convert(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if spec.source == spec.target {
            return Ok(value.clone());
        }
        let target = &spec.target;
        if let DataType::Decimal { width, scale } = target {
            let scaled = match value {
                Value::Decimal {
                    value, scale: from, ..
                } => rescale(*value, *from, *scale)?,
                Value::Integer(n) => rescale(*n, 0, *scale)?,
                Value::Unsigned(n) => {
                    rescale(i128::try_from(*n).map_err(|_| conversion())?, 0, *scale)?
                }
                Value::Boolean(n) => rescale(i128::from(*n), 0, *scale)?,
                Value::Varchar(text) => {
                    let (negative, n) = parse_scaled(text, *scale, query)?;
                    signed(negative, n)?
                }
                Value::Float(_) | Value::Double(_) => {
                    let n = (value.as_f64()? * 10_f64.powi(i32::from(*scale))).round();
                    if !n.is_finite() || n >= -(i128::MIN as f64) || n < i128::MIN as f64 {
                        return Err(conversion());
                    }
                    n as i128
                }
                _ => return Err(conversion()),
            };
            return decimal(scaled, *width, *scale);
        }
        if target.is_integer() {
            let (negative, magnitude) = match value {
                Value::Integer(n) => (*n < 0, n.unsigned_abs()),
                Value::Unsigned(n) => (false, *n),
                Value::Decimal { value, scale, .. } => {
                    let n = rescale(*value, *scale, 0)?;
                    (n < 0, n.unsigned_abs())
                }
                Value::Varchar(text) => parse_integer(text, query)?,
                Value::Boolean(n) => (false, u128::from(*n)),
                Value::Float(_) | Value::Double(_) => {
                    let n = value.as_f64()?.round();
                    if !n.is_finite() || n.abs() >= 2_f64.powi(128) {
                        return Err(conversion());
                    }
                    (n < 0.0, n.abs() as u128)
                }
                _ => return Err(conversion()),
            };
            let result = if target.is_unsigned_integer() {
                if negative && magnitude != 0 {
                    return Err(conversion());
                }
                Value::Unsigned(magnitude)
            } else {
                Value::Integer(signed(negative, magnitude)?)
            };
            return if result.fits_type(target) {
                Ok(result)
            } else {
                Err(conversion())
            };
        }
        match target {
            DataType::Varchar => Ok(Value::Varchar(value.to_string())),
            DataType::Float => {
                let n = value.as_f64()?;
                let out = n as f32;
                if n.is_finite() && !out.is_finite() {
                    return Err(conversion());
                }
                Ok(Value::Float(out))
            }
            DataType::Double => Ok(Value::Double(value.as_f64()?)),
            DataType::Boolean => Ok(Value::Boolean(match value {
                Value::Decimal { value, .. } => *value != 0,
                Value::Unsigned(n) => *n != 0,
                _ => return Err(conversion()),
            })),
            _ => Err(conversion()),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn signed(negative: bool, magnitude: u128) -> Result<i128> {
    if negative && magnitude == (1_u128 << 127) {
        return Ok(i128::MIN);
    }
    let n = i128::try_from(magnitude).map_err(|_| conversion())?;
    Ok(if negative { -n } else { n })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn conversion() -> Error {
    Error::Conversion("numeric value outside target range".into())
}

/// Parse and round a base-ten value without passing through floating point.
/// At most 39 significant result digits are retained, including large exponents.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn parse_scaled(text: &str, scale: u8, query: &QueryContext) -> Result<(bool, u128)> {
    query.check()?;
    let cleaned;
    let text = if text.contains('_') {
        cleaned = strip_separators(text, 10, query)?;
        cleaned.as_str()
    } else {
        text
    };
    let text = text.trim();
    let negative = text.starts_with('-');
    let text = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (
            &text[..index],
            text[index + 1..].parse::<i64>().map_err(|_| conversion())?,
        ),
        None => (text, 0),
    };
    let mut digits = Vec::new();
    let mut fraction = 0_i64;
    let mut point = false;
    for (index, byte) in mantissa.bytes().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if byte == b'.' && !point {
            point = true;
            continue;
        }
        if !byte.is_ascii_digit() {
            return Err(conversion());
        }
        digits.push(byte - b'0');
        if point {
            fraction += 1;
        }
    }
    if digits.is_empty() {
        return Err(conversion());
    }
    let first = digits.iter().position(|&d| d != 0).unwrap_or(digits.len());
    let digits = &digits[first..];
    if digits.is_empty() {
        return Ok((false, 0));
    }
    let shift = exponent
        .saturating_sub(fraction)
        .saturating_add(i64::from(scale));
    let kept = (digits.len() as i64).saturating_add(shift);
    if kept > 39 {
        return Err(conversion());
    }
    let mut value = 0_u128;
    for i in 0..kept.max(0) as usize {
        value = value
            .checked_mul(10)
            .and_then(|n| n.checked_add(u128::from(*digits.get(i).unwrap_or(&0))))
            .ok_or_else(conversion)?;
    }
    if kept >= 0 && digits.get(kept as usize).is_some_and(|&digit| digit >= 5) {
        value = value.checked_add(1).ok_or_else(conversion)?;
    }
    Ok((negative, value))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn strip_separators(text: &str, radix: u32, query: &QueryContext) -> Result<String> {
    let bytes = text.as_bytes();
    let mut cleaned = String::with_capacity(text.len());
    for (index, c) in text.char_indices() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if c == '_' {
            if index == 0
                || index + 1 == bytes.len()
                || !(bytes[index - 1] as char).is_digit(radix)
                || !(bytes[index + 1] as char).is_digit(radix)
            {
                return Err(conversion());
            }
        } else {
            cleaned.push(c);
        }
    }
    Ok(cleaned)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_integer(text: &str, query: &QueryContext) -> Result<(bool, u128)> {
    let text = text.trim();
    for (prefixes, radix) in [(["0x", "0X"], 16), (["0b", "0B"], 2)] {
        if let Some(digits) = text
            .strip_prefix(prefixes[0])
            .or_else(|| text.strip_prefix(prefixes[1]))
        {
            let digits = strip_separators(digits, radix, query)?;
            if !digits.chars().all(|c| c.is_digit(radix)) {
                return Err(conversion());
            }
            return u128::from_str_radix(&digits, radix)
                .map(|n| (false, n))
                .map_err(|_| conversion());
        }
    }
    parse_scaled(text, 0, query)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut CastRegistry) {
    use DataType::*;
    let types = [
        Null,
        Boolean,
        TinyInt,
        SmallInt,
        Integer,
        BigInt,
        HugeInt,
        UTinyInt,
        USmallInt,
        UInteger,
        UBigInt,
        UHugeInt,
        Float,
        Double,
        Varchar,
        Decimal {
            width: 18,
            scale: 3,
        },
    ];
    for source in &types {
        for target in &types {
            if !(source.is_decimal()
                || target.is_decimal()
                || source.is_unsigned_integer()
                || target.is_unsigned_integer())
            {
                continue;
            }
            registry
                .register_family(source.family(), target.family(), Arc::new(ExactNumericCast))
                .expect("unique numeric cast family");
        }
    }
}
