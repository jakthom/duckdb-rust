//! Owned BLOB and UUID conversion primitives. These carry real binary values;
//! display encoding is never used as the engine's equality or storage payload.
use std::fmt;

use super::{Error, Result};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn parse_blob(text: &str, mut check: impl FnMut() -> Result<()>) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if index % 1024 == 0 {
            check()?;
        }
        match bytes[index] {
            b'\\' => {
                let escape = bytes.get(index + 1..index.saturating_add(4));
                let Some([b'x', a, b]) = escape else {
                    return Err(Error::Conversion("invalid hexadecimal BLOB escape".into()));
                };
                let (Some(a), Some(b)) = (hex_digit(*a), hex_digit(*b)) else {
                    return Err(Error::Conversion("invalid hexadecimal BLOB escape".into()));
                };
                result.push((a << 4) | b);
                index += 4;
            }
            byte @ 0..=127 => {
                result.push(byte);
                index += 1;
            }
            _ => {
                return Err(Error::Conversion(
                    "non-ASCII BLOB bytes must use hex escapes".into(),
                ));
            }
        }
    }
    check()?;
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn parse_uuid(text: &str, mut check: impl FnMut() -> Result<()>) -> Result<u128> {
    let text = if let Some(inner) = text.strip_prefix('{') {
        inner
            .strip_suffix('}')
            .ok_or_else(|| Error::Conversion("invalid UUID braces".into()))?
    } else {
        text
    };
    let mut result = 0;
    let mut count = 0;
    for (index, byte) in text.bytes().enumerate() {
        if index % 1024 == 0 {
            check()?;
        }
        if byte == b'-' {
            continue;
        }
        let digit =
            hex_digit(byte).ok_or_else(|| Error::Conversion("invalid UUID digit".into()))?;
        if count == 32 {
            return Err(Error::Conversion(
                "UUID requires 32 hexadecimal digits".into(),
            ));
        }
        result = (result << 4) | u128::from(digit);
        count += 1;
    }
    check()?;
    if count != 32 {
        return Err(Error::Conversion(
            "UUID requires 32 hexadecimal digits".into(),
        ));
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn format_blob(bytes: &[u8], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for byte in bytes {
        if (32..=126).contains(byte) && !matches!(byte, b'\\' | b'\'' | b'"') {
            write!(f, "{}", char::from(*byte))?;
        } else {
            write!(f, "\\x{byte:02X}")?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn format_uuid(value: u128, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
        f,
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        value >> 96,
        (value >> 80) & 0xffff,
        (value >> 64) & 0xffff,
        (value >> 48) & 0xffff,
        value & 0xffffffffffff
    )
}
