//! SQL floating text is separate from diagnostic Display. Pinned fmt's default
//! layout uses fixed notation for decimal exponents -4..16, retains a fixed
//! integer's `.0`, and signs/pads scientific exponents to at least two digits.
use crate::common::{Error, Result};
mod grisu;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn float(value: f32) -> Result<String> {
    if !value.is_finite() {
        return Ok(nonfinite(value.is_sign_negative(), value.is_nan()));
    }
    let (digits, exponent) = digits(f64::from(value.abs()), true)?;
    render(value.is_sign_negative(), &digits, exponent)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn double(value: f64) -> Result<String> {
    if !value.is_finite() {
        return Ok(nonfinite(value.is_sign_negative(), value.is_nan()));
    }
    let (digits, exponent) = digits(value.abs(), false)?;
    render(value.is_sign_negative(), &digits, exponent)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nonfinite(negative: bool, nan: bool) -> String {
    format!(
        "{}{}",
        if negative { "-" } else { "" },
        if nan { "nan" } else { "inf" }
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn digits(value: f64, binary32: bool) -> Result<(String, i32)> {
    if value == 0.0 {
        return Ok(("0".into(), 0));
    }
    grisu::shortest(value, binary32)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn render(negative: bool, digits: &str, coefficient_exponent: i32) -> Result<String> {
    let exponent = coefficient_exponent + digits.len() as i32 - 1;
    let mut output = String::with_capacity(32);
    if negative {
        output.push('-');
    }
    if !(-4..16).contains(&exponent) {
        output.push_str(&digits[..1]);
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        output.push('e');
        output.push(if exponent < 0 { '-' } else { '+' });
        use std::fmt::Write;
        write!(output, "{:02}", exponent.unsigned_abs())
            .map_err(|_| Error::Internal("floating exponent formatting".into()))?;
    } else {
        let full_exponent = exponent + 1;
        if full_exponent <= 0 {
            output.push_str("0.");
            output.extend(std::iter::repeat_n('0', (-full_exponent) as usize));
            output.push_str(digits);
        } else if full_exponent as usize >= digits.len() {
            output.push_str(digits);
            output.extend(std::iter::repeat_n(
                '0',
                full_exponent as usize - digits.len(),
            ));
            output.push_str(".0");
        } else {
            let (integer, fractional) = digits.split_at(full_exponent as usize);
            output.push_str(integer);
            output.push('.');
            output.push_str(fractional);
        }
    }
    Ok(output)
}
