//! Binary text and UTF-8 codecs. The malformed UTF-8 walk mirrors the pinned
//! development `Utf8Proc::{MakeValid,RemoveInvalid}` byte-consumption rules.

use crate::{
    common::{Error, Result},
    parallel::QueryContext,
};

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const POLL_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Utf8Sequence {
    Valid(usize),
    InvalidStart,
    Incomplete,
    BadContinuation(usize),
    InvalidCodepoint(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecodeBehavior {
    Strict,
    Replace,
    Ignore,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn decode_binary(text: &str, query: &QueryContext) -> Result<Vec<u8>> {
    query.check()?;
    check_limit(text.len(), "binary input")?;
    let bytes = text.as_bytes();
    let length = bytes
        .len()
        .checked_add(7)
        .ok_or_else(|| Error::Resource("binary output length overflow".into()))?
        / 8;
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| Error::Resource("binary BLOB allocation failed".into()))?;

    let mut index = 0;
    let leading = bytes.len() % 8;
    if leading != 0 {
        output.push(decode_binary_byte(&bytes[..leading], query, &mut index)?);
    }
    for chunk in bytes[leading..].chunks_exact(8) {
        output.push(decode_binary_byte(chunk, query, &mut index)?);
    }
    query.check()?;
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decode_binary_byte(digits: &[u8], query: &QueryContext, index: &mut usize) -> Result<u8> {
    let mut byte = 0_u8;
    for digit in digits {
        if (*index).is_multiple_of(POLL_BYTES) {
            query.check()?;
        }
        *index += 1;
        byte <<= 1;
        match digit {
            b'0' => {}
            b'1' => byte |= 1,
            other => {
                let digit = if other.is_ascii() {
                    char::from(*other)
                } else {
                    char::REPLACEMENT_CHARACTER
                };
                return Err(Error::InvalidInput(format!(
                    "Invalid input for binary digit: {digit}"
                )));
            }
        }
    }
    Ok(byte)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn decode_utf8(
    bytes: &[u8],
    specifier: Option<&str>,
    query: &QueryContext,
) -> Result<String> {
    query.check()?;
    check_limit(bytes.len(), "decode BLOB")?;
    if valid_utf8(bytes, query)? {
        // Pinned DuckDB does not inspect the optional specifier for valid UTF-8.
        return copy_valid(bytes, query);
    }
    let behavior = decode_behavior(specifier.unwrap_or("strict"))?;
    match behavior {
        DecodeBehavior::Strict => Err(invalid_utf8()),
        DecodeBehavior::Replace => replace_invalid(bytes, query),
        DecodeBehavior::Ignore => remove_invalid(bytes, query),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decode_behavior(specifier: &str) -> Result<DecodeBehavior> {
    check_limit(specifier.len(), "decode error behavior")?;
    if specifier.eq_ignore_ascii_case("strict") {
        Ok(DecodeBehavior::Strict)
    } else if specifier.eq_ignore_ascii_case("replace") {
        Ok(DecodeBehavior::Replace)
    } else if specifier.eq_ignore_ascii_case("ignore") {
        Ok(DecodeBehavior::Ignore)
    } else {
        Err(Error::Conversion(format!(
            "decode error behavior specifier \"{specifier}\" not recognized"
        )))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn valid_utf8(bytes: &[u8], query: &QueryContext) -> Result<bool> {
    let mut index = 0;
    let mut next_poll = 0;
    while index < bytes.len() {
        poll(index, &mut next_poll, query)?;
        if bytes[index].is_ascii() {
            index += 1;
            continue;
        }
        match sequence(bytes, index) {
            Utf8Sequence::Valid(end) => index = end,
            _ => return Ok(false),
        }
    }
    query.check()?;
    Ok(true)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn copy_valid(bytes: &[u8], query: &QueryContext) -> Result<String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::Internal("validated decode BLOB became invalid UTF-8".into()))?;
    let mut output = String::new();
    output
        .try_reserve_exact(text.len())
        .map_err(|_| Error::Resource("decode VARCHAR allocation failed".into()))?;
    // `valid_utf8` already polled throughout this input and reserved capacity
    // makes this final owned copy non-growing.
    output.push_str(text);
    query.check()?;
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn replace_invalid(bytes: &[u8], query: &QueryContext) -> Result<String> {
    let mut output = copy_bytes(bytes, "decode VARCHAR", query)?;
    let mut index = 0;
    let mut next_poll = 0;
    while index < output.len() {
        poll(index, &mut next_poll, query)?;
        if output[index].is_ascii() {
            index += 1;
            continue;
        }
        match sequence(&output, index) {
            Utf8Sequence::Valid(end) => index = end,
            Utf8Sequence::InvalidStart | Utf8Sequence::Incomplete => {
                output[index] = b'?';
                index += 1;
            }
            Utf8Sequence::BadContinuation(position) => {
                output[index..=position].fill(b'?');
                index = position + 1;
            }
            Utf8Sequence::InvalidCodepoint(end) => {
                output[index..end].fill(b'?');
                index = end;
            }
        }
    }
    query.check()?;
    String::from_utf8(output)
        .map_err(|_| Error::Internal("decode replacement produced invalid UTF-8".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn remove_invalid(bytes: &[u8], query: &QueryContext) -> Result<String> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(bytes.len())
        .map_err(|_| Error::Resource("decode VARCHAR allocation failed".into()))?;
    let mut index = 0;
    let mut next_poll = 0;
    while index < bytes.len() {
        poll(index, &mut next_poll, query)?;
        if bytes[index].is_ascii() {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        match sequence(bytes, index) {
            Utf8Sequence::Valid(end) => {
                output.extend_from_slice(&bytes[index..end]);
                index = end;
            }
            Utf8Sequence::InvalidStart | Utf8Sequence::Incomplete => index += 1,
            // The mismatching byte was not consumed by the malformed sequence.
            Utf8Sequence::BadContinuation(position) => index = position,
            Utf8Sequence::InvalidCodepoint(end) => index = end,
        }
    }
    query.check()?;
    String::from_utf8(output)
        .map_err(|_| Error::Internal("decode removal produced invalid UTF-8".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn copy_bytes(bytes: &[u8], operation: &str, query: &QueryContext) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(bytes.len())
        .map_err(|_| Error::Resource(format!("{operation} allocation failed")))?;
    for chunk in bytes.chunks(POLL_BYTES) {
        query.check()?;
        output.extend_from_slice(chunk);
    }
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sequence(bytes: &[u8], start: usize) -> Utf8Sequence {
    let first = bytes[start];
    let (extra, mask) = if first & 0xe0 == 0xc0 {
        (1, 0x0000_0780)
    } else if first & 0xf0 == 0xe0 {
        (2, 0x0000_f800)
    } else if first & 0xf8 == 0xf0 {
        (3, 0x1f_00_00)
    } else {
        return Utf8Sequence::InvalidStart;
    };
    let end = start + extra + 1;
    if end > bytes.len() {
        return Utf8Sequence::Incomplete;
    }
    let mut codepoint = u32::from(first & (0x7f >> extra));
    for (offset, byte) in bytes[start + 1..end].iter().copied().enumerate() {
        if byte & 0xc0 != 0x80 {
            return Utf8Sequence::BadContinuation(start + offset + 1);
        }
        codepoint = (codepoint << 6) | u32::from(byte & 0x3f);
    }
    if codepoint & mask == 0 || codepoint > 0x10ffff || codepoint & 0x1fff800 == 0xd800 {
        Utf8Sequence::InvalidCodepoint(end)
    } else {
        Utf8Sequence::Valid(end)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn check_limit(length: usize, operation: &str) -> Result<()> {
    if length > MAX_INPUT_BYTES {
        Err(Error::Resource(format!(
            "{operation} exceeds 16 MiB byte limit"
        )))
    } else {
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn invalid_utf8() -> Error {
    Error::Conversion(
        "Failure in decode: could not convert blob to UTF8 string, the blob contained invalid UTF8 characters.\n\
         Use try(decode(BLOB)) to return NULL and continue instead of returning an error. \
         Specify decode(BLOB, 'replace') to replace invalid characters with '?'. \
         Specify decode(BLOB, 'ignore') to remove invalid characters when encountered."
            .into(),
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn poll(index: usize, next_poll: &mut usize, query: &QueryContext) -> Result<()> {
    if index >= *next_poll {
        query.check()?;
        *next_poll = index.saturating_add(POLL_BYTES);
    }
    Ok(())
}
