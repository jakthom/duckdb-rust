//! Logical BIT strings, independent of DuckDB's leading one-filled padding.
//! Bytes are MSB-first with zero-filled trailing padding; length is significant.
use std::{cmp::Ordering, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{Error, Result, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitString {
    bytes: Vec<u8>,
    length: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BitString {
    pub fn from_parts(bytes: Vec<u8>, length: usize) -> Result<Self> {
        let value = Self { bytes, length };
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        let padding = (8 - self.length % 8) % 8;
        if self.bytes.len() != self.length.div_ceil(8)
            || (padding != 0
                && self
                    .bytes
                    .last()
                    .is_none_or(|last| last & ((1 << padding) - 1) != 0))
        {
            return Err(Error::Conversion(
                "invalid packed BIT length or padding".into(),
            ));
        }
        Ok(())
    }
    pub fn length(&self) -> usize {
        self.length
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn get(&self, index: usize) -> Result<bool> {
        if index >= self.length {
            return Err(Error::OutOfRange("bit index outside valid range".into()));
        }
        Ok(self
            .bytes
            .get(index / 8)
            .ok_or_else(|| Error::Conversion("invalid packed BIT payload".into()))?
            & (1 << (7 - index % 8))
            != 0)
    }
    pub fn with_bit(&self, index: usize, bit: bool) -> Result<Self> {
        self.validate()?;
        self.get(index)?;
        let mut result = self.clone();
        let mask = 1 << (7 - index % 8);
        if bit {
            result.bytes[index / 8] |= mask;
        } else {
            result.bytes[index / 8] &= !mask;
        }
        Ok(result)
    }
    pub fn count(&self, mut check: impl FnMut() -> Result<()>) -> Result<usize> {
        check()?;
        self.validate()?;
        let mut count = 0;
        for (index, byte) in self.bytes.iter().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            count += byte.count_ones() as usize;
        }
        Ok(count)
    }
    pub fn to_text(&self, mut check: impl FnMut() -> Result<()>) -> Result<String> {
        check()?;
        self.validate()?;
        let mut output = String::new();
        output
            .try_reserve_exact(self.length)
            .map_err(|_| Error::Resource("cannot allocate BIT text".into()))?;
        for index in 0..self.length {
            if index % 1024 == 0 {
                check()?;
            }
            output.push(if self.get(index)? { '1' } else { '0' });
        }
        Ok(output)
    }
    pub fn extend(&self, length: usize, mut check: impl FnMut() -> Result<()>) -> Result<Self> {
        check()?;
        self.validate()?;
        let shift = length.checked_sub(self.length).ok_or_else(|| {
            Error::InvalidInput("Length must be equal or larger than input string".into())
        })?;
        let mut output = Self::zeroed(length)?;
        for (index, byte) in self.bytes.iter().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            let target = index + shift / 8;
            output.bytes[target] |= byte >> (shift % 8);
            if shift % 8 != 0 && target + 1 < output.bytes.len() {
                output.bytes[target + 1] |= byte << (8 - shift % 8);
            }
        }
        Ok(output)
    }
    pub fn zeroed(length: usize) -> Result<Self> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length.div_ceil(8))
            .map_err(|_| Error::Resource("cannot allocate BIT value".into()))?;
        bytes.resize(length.div_ceil(8), 0);
        Ok(Self { bytes, length })
    }
    pub fn shift(
        &self,
        count: usize,
        left: bool,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        self.validate()?;
        let mut output = Self::zeroed(self.length)?;
        if count >= self.length {
            return Ok(output);
        }
        let (whole, part) = (count / 8, count % 8);
        for (index, byte) in output.bytes.iter_mut().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            if left {
                let source = index + whole;
                *byte = self.bytes.get(source).map_or(0, |b| b << part);
                if part != 0 {
                    *byte |= self.bytes.get(source + 1).map_or(0, |b| b >> (8 - part));
                }
            } else if let Some(source) = index.checked_sub(whole) {
                *byte = self.bytes[source] >> part;
                if part != 0 && source != 0 {
                    *byte |= self.bytes[source - 1] << (8 - part);
                }
            }
        }
        output.clear_padding();
        Ok(output)
    }
    pub fn bitwise(
        &self,
        other: &Self,
        operation: fn(u8, u8) -> u8,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        self.validate()?;
        other.validate()?;
        if self.length != other.length {
            return Err(Error::InvalidInput(
                "Cannot combine BIT strings of different sizes".into(),
            ));
        }
        let mut output = self.clone();
        for (index, (a, b)) in output.bytes.iter_mut().zip(&other.bytes).enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            *a = operation(*a, *b);
        }
        output.clear_padding();
        Ok(output)
    }
    pub fn invert(&self, mut check: impl FnMut() -> Result<()>) -> Result<Self> {
        check()?;
        self.validate()?;
        let mut output = self.clone();
        for (index, byte) in output.bytes.iter_mut().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            *byte = !*byte;
        }
        output.clear_padding();
        Ok(output)
    }
    fn clear_padding(&mut self) {
        if !self.length.is_multiple_of(8)
            && let Some(last) = self.bytes.last_mut()
        {
            *last &= u8::MAX << (8 - self.length % 8);
        }
    }
    pub fn compare(&self, other: &Self) -> Ordering {
        self.bytes
            .cmp(&other.bytes)
            .then(self.length.cmp(&other.length))
    }
    pub fn parse(text: &str, mut check: impl FnMut() -> Result<()>) -> Result<Self> {
        check()?;
        if text.is_empty() {
            return Self::from_parts(vec![0], 1);
        }
        if let Some(hex) = text.strip_prefix('x') {
            if hex.is_empty() {
                return Err(Error::Conversion("Cannot cast empty string to BIT".into()));
            }
            let length = hex
                .len()
                .checked_mul(4)
                .ok_or_else(|| Error::Resource("BIT length overflow".into()))?;
            let mut bytes = vec![0; length.div_ceil(8)];
            for (index, digit) in hex.bytes().enumerate() {
                if index % 1024 == 0 {
                    check()?;
                }
                let digit = super::scalar::hex_digit(digit)
                    .ok_or_else(|| Error::Conversion("invalid hexadecimal BIT digit".into()))?;
                bytes[index / 2] |= digit << if index % 2 == 0 { 4 } else { 0 };
            }
            return Self::from_parts(bytes, length);
        }
        let mut bytes = vec![0; text.len().div_ceil(8)];
        for (index, bit) in text.bytes().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            match bit {
                b'0' => (),
                b'1' => bytes[index / 8] |= 1 << (7 - index % 8),
                _ => return Err(Error::Conversion("invalid character in BIT string".into())),
            }
        }
        Self::from_parts(bytes, text.len())
    }
    pub fn from_blob(bytes: Vec<u8>) -> Result<Self> {
        let length = bytes
            .len()
            .checked_mul(8)
            .ok_or_else(|| Error::Resource("BIT length overflow".into()))?;
        Self::from_parts(bytes, length)
    }
    pub fn to_blob(&self, mut check: impl FnMut() -> Result<()>) -> Result<Vec<u8>> {
        check()?;
        self.validate()?;
        let padding = (8 - self.length % 8) % 8;
        if padding == 0 {
            return Ok(self.bytes.clone());
        }
        let mut output = Vec::with_capacity(self.bytes.len());
        for (index, byte) in self.bytes.iter().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            output.push(
                (byte >> padding)
                    | if index == 0 {
                        0
                    } else {
                        self.bytes[index - 1] << (8 - padding)
                    },
            );
        }
        Ok(output)
    }
    pub fn to_native(&self, check: impl FnMut() -> Result<()>) -> Result<Vec<u8>> {
        let mut output = self.to_blob(check)?;
        let padding = (8 - self.length % 8) % 8;
        if padding != 0 {
            output[0] |= u8::MAX << (8 - padding);
        }
        output.insert(0, padding as u8);
        Ok(output)
    }
    pub fn from_native(bytes: &[u8], mut check: impl FnMut() -> Result<()>) -> Result<Self> {
        check()?;
        let (&padding, bytes) = bytes
            .split_first()
            .ok_or_else(|| Error::Corrupt("missing native BIT padding".into()))?;
        if padding > 7 || (bytes.is_empty() && padding != 0) {
            return Err(Error::Corrupt("invalid native BIT padding".into()));
        }
        let length = bytes
            .len()
            .checked_mul(8)
            .and_then(|len| len.checked_sub(usize::from(padding)))
            .ok_or_else(|| Error::Corrupt("native BIT length overflow".into()))?;
        if padding == 0 {
            return Self::from_parts(bytes.to_vec(), length);
        }
        let mask = u8::MAX << (8 - padding);
        if bytes[0] & mask != mask {
            return Err(Error::Corrupt(
                "native BIT padding is not one-filled".into(),
            ));
        }
        let mut output = Vec::with_capacity(bytes.len());
        for (index, byte) in bytes.iter().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            output.push(
                (byte << padding) | bytes.get(index + 1).map_or(0, |next| next >> (8 - padding)),
            );
        }
        Self::from_parts(output, length)
    }
    pub fn value(self) -> Value {
        Value::Bit(Arc::new(self))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for BitString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.validate().map_err(|_| fmt::Error)?;
        for index in 0..self.length {
            f.write_str(if self.bytes[index / 8] & (1 << (7 - index % 8)) != 0 {
                "1"
            } else {
                "0"
            })?;
        }
        Ok(())
    }
}
