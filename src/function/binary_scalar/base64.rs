//! BLOB base64 follows pinned common/types/blob.cpp: canonical encoding but
//! permissive final-quartet padding and ignored unused bits when decoding.
use crate::{
    common::{Error, Result},
    parallel::QueryContext,
};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode(bytes: &[u8], query: &QueryContext) -> Result<String> {
    query.check()?;
    let length = bytes
        .len()
        .div_ceil(3)
        .checked_mul(4)
        .ok_or_else(|| Error::Resource("base64 output length overflow".into()))?;
    let mut output = String::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| Error::Resource("base64 text allocation failed".into()))?;
    for (index, chunk) in bytes.chunks(3).enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for digit in 0..4 {
            output.push(if digit > chunk.len() {
                '='
            } else {
                char::from(ALPHABET[((bits >> (18 - digit * 6)) & 63) as usize])
            });
        }
    }
    query.check()?;
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn decode(text: &str, query: &QueryContext) -> Result<Vec<u8>> {
    query.check()?;
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(Error::Conversion(format!(
            "Could not decode string \"{text}\" as base64: length must be a multiple of 4"
        )));
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    // The reference considers '=' at the penultimate byte sufficient to trim
    // two output bytes, even if the last byte is not '=' (for example AA=B).
    let padding = if bytes[bytes.len() - 2] == b'=' {
        2
    } else {
        usize::from(bytes[bytes.len() - 1] == b'=')
    };
    let length = bytes.len() / 4 * 3 - padding;
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| Error::Resource("base64 blob allocation failed".into()))?;
    for (index, quartet) in bytes.chunks_exact(4).enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        let final_quartet = index == bytes.len() / 4 - 1;
        let mut bits = 0_u32;
        for (digit, byte) in quartet.iter().copied().enumerate() {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' if final_quartet && digit >= 2 => 0,
                _ => {
                    return Err(Error::Conversion(format!(
                        "Could not decode string \"{text}\" as base64: invalid byte value '{byte}' at position {}",
                        index * 4 + digit
                    )));
                }
            };
            bits = (bits << 6) | u32::from(value);
        }
        for shift in [16, 8, 0] {
            if output.len() < length {
                output.push((bits >> shift) as u8);
            }
        }
    }
    query.check()?;
    Ok(output)
}
