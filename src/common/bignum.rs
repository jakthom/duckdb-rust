//! Arbitrary-width signed integers with logical little-endian magnitude limbs.
//! Native complemented headers are a boundary encoding, not the representation.
//! Negative zero is retained: pinned development constructs it from negative
//! fractional FLOAT/DOUBLE values and distinguishes it in ordinary BIGNUM keys.
use std::{cmp::Ordering, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{Error, Result, Value};

const MAX_BYTES: usize = 0x7f_ffff;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BignumValue {
    negative: bool,
    limbs: Vec<u32>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BignumValue {
    pub fn from_parts(negative: bool, limbs: Vec<u32>) -> Result<Self> {
        let result = Self { negative, limbs };
        result.validate()?;
        Ok(result)
    }
    pub fn validate(&self) -> Result<()> {
        if self.limbs.is_empty()
            || (self.limbs.len() > 1 && self.limbs.last() == Some(&0))
            || self.byte_len() > MAX_BYTES
        {
            return Err(Error::Conversion(
                "invalid BIGNUM magnitude shape or length".into(),
            ));
        }
        Ok(())
    }
    pub fn is_negative(&self) -> bool {
        self.negative
    }
    pub fn is_zero(&self) -> bool {
        self.limbs == [0]
    }
    pub fn byte_len(&self) -> usize {
        self.limbs
            .len()
            .saturating_sub(1)
            .saturating_mul(4)
            .saturating_add(self.limbs.last().map_or(0, |v| {
                ((32 - v.leading_zeros()).max(1) as usize).div_ceil(8)
            }))
    }
    pub fn from_u128(value: u128) -> Self {
        let mut limbs = vec![
            value as u32,
            (value >> 32) as u32,
            (value >> 64) as u32,
            (value >> 96) as u32,
        ];
        while limbs.len() > 1 && limbs.last() == Some(&0) {
            limbs.pop();
        }
        Self {
            negative: false,
            limbs,
        }
    }
    pub fn from_i128(value: i128) -> Self {
        let mut result = Self::from_u128(value.unsigned_abs());
        result.negative = value < 0;
        result
    }
    pub fn from_f64(value: f64) -> Result<Self> {
        if !value.is_finite() {
            return Err(Error::Conversion(
                "Cannot convert non-finite floating value to BIGNUM".into(),
            ));
        }
        if value == 0.0 {
            return Ok(Self::from_u128(0));
        }
        let mut limbs = Vec::new();
        let mut magnitude = value.abs();
        let mut position = 0;
        while magnitude > 0.0 {
            if position % 4 == 0 {
                limbs.push(0);
            }
            let quotient = (magnitude / 256.0).floor();
            let byte = (magnitude - quotient * 256.0) as u8;
            limbs[position / 4] |= u32::from(byte) << (8 * (position % 4));
            position += 1;
            magnitude = quotient;
        }
        Self::from_parts(value < 0.0, limbs)
    }
    pub fn parse(text: &str, mut check: impl FnMut() -> Result<()>) -> Result<Self> {
        check()?;
        let invalid = || Error::Conversion(format!("Could not convert string '{text}' to Bignum"));
        let (negative, body) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (integer, fractional) = body.split_once('.').unwrap_or((body, ""));
        if integer.is_empty() && fractional.is_empty() {
            return Err(invalid());
        }
        for (index, digit) in integer.bytes().chain(fractional.bytes()).enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            if !digit.is_ascii_digit() {
                return Err(invalid());
            }
        }
        let mut result = Self::from_u128(0);
        for (index, chunk) in integer.as_bytes().chunks(9).enumerate() {
            if index % 128 == 0 {
                check()?;
            }
            let mut coefficient = 0_u32;
            for &digit in chunk {
                coefficient = coefficient * 10 + u32::from(digit - b'0');
            }
            result.multiply_add(10_u32.pow(chunk.len() as u32), coefficient, &mut check)?;
        }
        if round_fraction(fractional.as_bytes(), &mut check)? {
            result.multiply_add(1, 1, &mut check)?;
        }
        result.negative = negative && !result.is_zero();
        Ok(result)
    }
    fn multiply_add(
        &mut self,
        factor: u32,
        add: u32,
        check: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let mut carry = u64::from(add);
        for (index, limb) in self.limbs.iter_mut().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            let result = u64::from(*limb) * u64::from(factor) + carry;
            *limb = result as u32;
            carry = result >> 32;
        }
        if carry != 0 {
            self.limbs
                .try_reserve(1)
                .map_err(|_| Error::Resource("cannot grow BIGNUM".into()))?;
            self.limbs.push(carry as u32);
        }
        if self.byte_len() > MAX_BYTES {
            return Err(Error::OutOfRange(
                "BIGNUM exceeds maximum magnitude byte count".into(),
            ));
        }
        Ok(())
    }
    pub fn to_decimal(&self, mut check: impl FnMut() -> Result<()>) -> Result<String> {
        check()?;
        self.validate()?;
        let mut limbs = self.limbs.clone();
        let mut chunks = Vec::new();
        loop {
            check()?;
            let mut carry = 0_u64;
            for (index, limb) in limbs.iter_mut().rev().enumerate() {
                if index % 1024 == 0 {
                    check()?;
                }
                let word = (carry << 32) | u64::from(*limb);
                *limb = (word / 1_000_000_000) as u32;
                carry = word % 1_000_000_000;
            }
            chunks
                .try_reserve(1)
                .map_err(|_| Error::Resource("cannot format BIGNUM".into()))?;
            chunks.push(carry as u32);
            while limbs.last() == Some(&0) {
                limbs.pop();
            }
            if limbs.is_empty() {
                break;
            }
        }
        let mut text = String::new();
        let capacity = chunks
            .len()
            .checked_mul(9)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| Error::Resource("BIGNUM text size overflow".into()))?;
        text.try_reserve_exact(capacity)
            .map_err(|_| Error::Resource("cannot allocate BIGNUM text".into()))?;
        if self.negative {
            text.push('-');
        }
        text.push_str(&chunks.pop().unwrap().to_string());
        for (index, chunk) in chunks.iter().rev().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            use fmt::Write;
            write!(text, "{chunk:09}")
                .map_err(|_| Error::Internal("BIGNUM decimal formatting".into()))?;
        }
        Ok(text)
    }
    pub fn to_f64(&self, mut check: impl FnMut() -> Result<()>) -> Result<f64> {
        check()?;
        self.validate()?;
        let mut result = 0.0;
        for position in 0..self.byte_len() {
            if position % 1024 == 0 {
                check()?;
            }
            let byte = (self.limbs[position / 4] >> (8 * (position % 4))) as u8;
            result += f64::from(byte) * 256_f64.powi(position as i32);
        }
        if self.negative {
            result = -result;
        }
        if !result.is_finite() {
            return Err(Error::Conversion(format!(
                "Could not convert bignum '{}' to Double",
                self.to_decimal(check)?
            )));
        }
        Ok(result)
    }
    /// Development integral casts first accumulate into a wrapping 128-bit
    /// temporary, even for wider magnitudes. Keep this explicit at the cast
    /// boundary; arithmetic/comparisons must never use this narrowing helper.
    pub fn low_u128(&self) -> u128 {
        self.limbs
            .iter()
            .take(4)
            .enumerate()
            .fold(0, |value, (index, limb)| {
                value | (u128::from(*limb) << (32 * index))
            })
    }
    fn compare_magnitude(
        &self,
        right: &Self,
        check: &mut impl FnMut() -> Result<()>,
    ) -> Result<Ordering> {
        let order = self.limbs.len().cmp(&right.limbs.len());
        if !order.is_eq() {
            return Ok(order);
        }
        for (index, (left, right)) in self
            .limbs
            .iter()
            .rev()
            .zip(right.limbs.iter().rev())
            .enumerate()
        {
            if index % 1024 == 0 {
                check()?;
            }
            let order = left.cmp(right);
            if !order.is_eq() {
                return Ok(order);
            }
        }
        Ok(Ordering::Equal)
    }
    pub fn compare(&self, right: &Self, mut check: impl FnMut() -> Result<()>) -> Result<Ordering> {
        check()?;
        self.validate()?;
        right.validate()?;
        if self.negative != right.negative {
            return Ok(right.negative.cmp(&self.negative));
        }
        let order = self.compare_magnitude(right, &mut check)?;
        Ok(if self.negative {
            order.reverse()
        } else {
            order
        })
    }
    pub fn negated(&self) -> Self {
        let mut result = self.clone();
        if !self.is_zero() || self.negative {
            result.negative = !self.negative;
        }
        result
    }
    pub fn add(&self, right: &Self, mut check: impl FnMut() -> Result<()>) -> Result<Self> {
        check()?;
        self.validate()?;
        right.validate()?;
        let order = self.compare_magnitude(right, &mut check)?;
        if self.negative != right.negative && order.is_eq() {
            return Ok(Self::from_u128(0));
        }
        let (large, small) = if order.is_lt() {
            (right, self)
        } else {
            (self, right)
        };
        let mut limbs = Vec::new();
        limbs
            .try_reserve_exact(large.limbs.len() + 1)
            .map_err(|_| Error::Resource("cannot allocate BIGNUM sum".into()))?;
        let mut carry = 0_u64;
        for (index, &large_limb) in large.limbs.iter().enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            let small_limb = u64::from(small.limbs.get(index).copied().unwrap_or(0));
            if self.negative == right.negative {
                let result = u64::from(large_limb) + small_limb + carry;
                limbs.push(result as u32);
                carry = result >> 32;
            } else {
                let subtract = small_limb + carry;
                limbs.push(u64::from(large_limb).wrapping_sub(subtract) as u32);
                carry = u64::from(u64::from(large_limb) < subtract);
            }
        }
        if self.negative == right.negative && carry != 0 {
            limbs.push(carry as u32);
        }
        while limbs.len() > 1 && limbs.last() == Some(&0) {
            limbs.pop();
        }
        let result = Self {
            negative: large.negative,
            limbs,
        };
        if result.byte_len() > MAX_BYTES {
            return Err(Error::OutOfRange(
                "BIGNUM arithmetic exceeds maximum magnitude byte count".into(),
            ));
        }
        Ok(result)
    }
    pub fn to_native(&self, mut check: impl FnMut() -> Result<()>) -> Result<Vec<u8>> {
        check()?;
        self.validate()?;
        let size = self.byte_len();
        let header = (size as u32) | 0x80_0000;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size + 3)
            .map_err(|_| Error::Resource("cannot allocate native BIGNUM".into()))?;
        bytes.extend_from_slice(&header.to_be_bytes()[1..]);
        for position in (0..size).rev() {
            if position % 1024 == 0 {
                check()?;
            }
            bytes.push((self.limbs[position / 4] >> (8 * (position % 4))) as u8);
        }
        if self.negative {
            for (index, byte) in bytes.iter_mut().enumerate() {
                if index % 1024 == 0 {
                    check()?;
                }
                *byte = !*byte;
            }
        }
        Ok(bytes)
    }
    pub fn from_native(bytes: &[u8], mut check: impl FnMut() -> Result<()>) -> Result<Self> {
        check()?;
        if bytes.len() < 4 {
            return Err(Error::Corrupt(
                "BIGNUM native payload is shorter than header and magnitude".into(),
            ));
        }
        let negative = bytes[0] & 0x80 == 0;
        let decode = |byte: u8| if negative { !byte } else { byte };
        let size = u32::from_be_bytes([
            0,
            decode(bytes[0]) & 0x7f,
            decode(bytes[1]),
            decode(bytes[2]),
        ]) as usize;
        if size != bytes.len() - 3 || (size > 1 && decode(bytes[3]) == 0) {
            return Err(Error::Corrupt(
                "BIGNUM native header length or leading magnitude differs".into(),
            ));
        }
        let mut limbs = Vec::new();
        limbs
            .try_reserve_exact(size.div_ceil(4))
            .map_err(|_| Error::Resource("cannot allocate BIGNUM limbs".into()))?;
        for (position, &byte) in bytes[3..].iter().rev().enumerate() {
            if position % 1024 == 0 {
                check()?;
            }
            if position % 4 == 0 {
                limbs.push(0);
            }
            limbs[position / 4] |= u32::from(decode(byte)) << (8 * (position % 4));
        }
        Self::from_parts(negative, limbs)
            .map_err(|_| Error::Corrupt("invalid BIGNUM native magnitude".into()))
    }
    pub fn value(self) -> Value {
        Value::Bignum(Arc::new(self))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn round_fraction(digits: &[u8], check: &mut impl FnMut() -> Result<()>) -> Result<bool> {
    // Retain the pinned VARCHAR parser's bounded decimal accumulator, including
    // the observed overflow-tail rule (0.400000000000000000001 rounds to 1).
    let mut coefficient = 0_u64;
    let mut count = 0_u16;
    for (index, &digit) in digits.iter().enumerate() {
        if index % 1024 == 0 {
            check()?;
        }
        let digit = u64::from(digit - b'0');
        let Some(next) = coefficient
            .checked_mul(10)
            .and_then(|n| n.checked_add(digit))
        else {
            for (index, &remaining) in digits[index..].iter().enumerate() {
                if index % 1024 == 0 {
                    check()?;
                }
                if remaining != b'0' {
                    return Ok(true);
                }
            }
            break;
        };
        coefficient = next;
        count = count.wrapping_add(1);
    }
    while coefficient > 10 {
        coefficient /= 10;
        count = count.wrapping_sub(1);
    }
    Ok(count == 1 && coefficient >= 5)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for BignumValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_decimal(|| Ok(())).map_err(|_| fmt::Error)?)
    }
}
