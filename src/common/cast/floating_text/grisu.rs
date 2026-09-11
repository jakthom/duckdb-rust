//! Pinned fmt Grisu3 shortest-digit path, translated from DuckDB development
//! 99063af2bd third_party/fmt/include/fmt/format-inl.h. The acceptance decision
//! matters for FLOAT: the reference's declined case falls back in DOUBLE.
//!
//! Copyright (c) 2012 - present, Victor Zverovich
//!
//! Permission is hereby granted, free of charge, to any person obtaining
//! a copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject to
//! the following conditions:
//!
//! The above copyright notice and this permission notice shall be
//! included in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//! EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
//! LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
//! OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
//! WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
use crate::common::{Error, Result};

const SIGNIFICANDS: [u64; 87] = [
    0xfa8fd5a0081c0288,
    0xbaaee17fa23ebf76,
    0x8b16fb203055ac76,
    0xcf42894a5dce35ea,
    0x9a6bb0aa55653b2d,
    0xe61acf033d1a45df,
    0xab70fe17c79ac6ca,
    0xff77b1fcbebcdc4f,
    0xbe5691ef416bd60c,
    0x8dd01fad907ffc3c,
    0xd3515c2831559a83,
    0x9d71ac8fada6c9b5,
    0xea9c227723ee8bcb,
    0xaecc49914078536d,
    0x823c12795db6ce57,
    0xc21094364dfb5637,
    0x9096ea6f3848984f,
    0xd77485cb25823ac7,
    0xa086cfcd97bf97f4,
    0xef340a98172aace5,
    0xb23867fb2a35b28e,
    0x84c8d4dfd2c63f3b,
    0xc5dd44271ad3cdba,
    0x936b9fcebb25c996,
    0xdbac6c247d62a584,
    0xa3ab66580d5fdaf6,
    0xf3e2f893dec3f126,
    0xb5b5ada8aaff80b8,
    0x87625f056c7c4a8b,
    0xc9bcff6034c13053,
    0x964e858c91ba2655,
    0xdff9772470297ebd,
    0xa6dfbd9fb8e5b88f,
    0xf8a95fcf88747d94,
    0xb94470938fa89bcf,
    0x8a08f0f8bf0f156b,
    0xcdb02555653131b6,
    0x993fe2c6d07b7fac,
    0xe45c10c42a2b3b06,
    0xaa242499697392d3,
    0xfd87b5f28300ca0e,
    0xbce5086492111aeb,
    0x8cbccc096f5088cc,
    0xd1b71758e219652c,
    0x9c40000000000000,
    0xe8d4a51000000000,
    0xad78ebc5ac620000,
    0x813f3978f8940984,
    0xc097ce7bc90715b3,
    0x8f7e32ce7bea5c70,
    0xd5d238a4abe98068,
    0x9f4f2726179a2245,
    0xed63a231d4c4fb27,
    0xb0de65388cc8ada8,
    0x83c7088e1aab65db,
    0xc45d1df942711d9a,
    0x924d692ca61be758,
    0xda01ee641a708dea,
    0xa26da3999aef774a,
    0xf209787bb47d6b85,
    0xb454e4a179dd1877,
    0x865b86925b9bc5c2,
    0xc83553c5c8965d3d,
    0x952ab45cfa97a0b3,
    0xde469fbd99a05fe3,
    0xa59bc234db398c25,
    0xf6c69a72a3989f5c,
    0xb7dcbf5354e9bece,
    0x88fcf317f22241e2,
    0xcc20ce9bd35c78a5,
    0x98165af37b2153df,
    0xe2a0b5dc971f303a,
    0xa8d9d1535ce3b396,
    0xfb9b7cd9a4a7443c,
    0xbb764c4ca7a44410,
    0x8bab8eefb6409c1a,
    0xd01fef10a657842c,
    0x9b10a4e5e9913129,
    0xe7109bfba19c0c9d,
    0xac2820d9623bf429,
    0x80444b5e7aa7cf85,
    0xbf21e44003acdd2d,
    0x8e679c2f5e44ff8f,
    0xd433179d9c8cb841,
    0x9e19db92b4e31ba9,
    0xeb96bf6ebadf77d9,
    0xaf87023b9bf0ee6b,
];
const EXPONENTS: [i32; 87] = [
    -1220, -1193, -1166, -1140, -1113, -1087, -1060, -1034, -1007, -980, -954, -927, -901, -874,
    -847, -821, -794, -768, -741, -715, -688, -661, -635, -608, -582, -555, -529, -502, -475, -449,
    -422, -396, -369, -343, -316, -289, -263, -236, -210, -183, -157, -130, -103, -77, -50, -24, 3,
    30, 56, 83, 109, 136, 162, 189, 216, 242, 269, 295, 322, 348, 375, 402, 428, 455, 481, 508,
    534, 561, 588, 614, 641, 667, 694, 720, 747, 774, 800, 827, 853, 880, 907, 933, 960, 986, 1013,
    1039, 1066,
];

#[derive(Clone, Copy)]
struct Fp {
    significand: u64,
    exponent: i32,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Fp {
    fn normalized(self) -> Self {
        let shift = self.significand.leading_zeros();
        Self {
            significand: self.significand << shift,
            exponent: self.exponent - shift as i32,
        }
    }
    fn multiply(self, other: Self) -> Self {
        Self {
            significand: multiply(self.significand, other.significand),
            exponent: self.exponent + other.exponent + 64,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn multiply(left: u64, right: u64) -> u64 {
    let product = u128::from(left) * u128::from(right);
    (product >> 64) as u64 + u64::from(product as u64 & (1 << 63) != 0)
}

/// Positive, finite, nonzero input, including the source's DOUBLE fallback
/// when round-weed cannot choose a result (also for promoted FLOAT input).
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn shortest(value: f64, binary32: bool) -> Result<(String, i32)> {
    let bits = value.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let original = Fp {
        significand: fraction | if biased == 0 { 0 } else { 1 << 52 },
        exponent: biased.max(1) - 1023 - 52,
    };
    let (mut lower, upper) = if binary32 {
        let mut half_ulp = 1_u64 << 28;
        if original.exponent < -178 {
            half_ulp <<= -178 - original.exponent;
        }
        let closer = original.significand == 1 << 52 && original.exponent > -178;
        (
            Fp {
                significand: original.significand - (half_ulp >> u32::from(closer)),
                exponent: original.exponent,
            },
            Fp {
                significand: original.significand + half_ulp,
                exponent: original.exponent,
            }
            .normalized(),
        )
    } else {
        let closer = fraction == 0 && biased > 1;
        let shift = if closer { 2 } else { 1 };
        (
            Fp {
                significand: (original.significand << shift) - 1,
                exponent: original.exponent - shift,
            },
            Fp {
                significand: (original.significand << 1) + 1,
                exponent: original.exponent - 1,
            }
            .normalized(),
        )
    };
    lower.significand <<= lower.exponent - upper.exponent;
    let normalized = original.normalized();
    let min_exponent = -60 - (normalized.exponent + 64);
    let decimal = ((i64::from(min_exponent + 63) * 0x4d104d42 + ((1_i64 << 32) - 1)) >> 32) as i32;
    let index = ((decimal + 348 - 1) / 8 + 1) as usize;
    let cached = Fp {
        significand: *SIGNIFICANDS
            .get(index)
            .ok_or_else(|| Error::Internal("Grisu cached power index".into()))?,
        exponent: EXPONENTS[index],
    };
    let cache_decimal = -348 + index as i32 * 8;
    let scaled = normalized.multiply(cached);
    let lower = multiply(lower.significand, cached.significand) - 1;
    let upper = multiply(upper.significand, cached.significand) + 1;
    let mut handler = Shortest {
        digits: Vec::with_capacity(17),
        difference: upper - scaled.significand,
    };
    let unit_exponent = (-scaled.exponent) as u32;
    let one = 1_u64 << unit_exponent;
    let mut integral = upper >> unit_exponent;
    let mut fractional = upper & (one - 1);
    let mut error = upper - lower;
    let mut exponent = integral.ilog10() as i32 + 1;
    while exponent > 0 {
        let divisor = 10_u64.pow((exponent - 1) as u32);
        let digit = (integral / divisor) as u8;
        integral %= divisor;
        exponent -= 1;
        let remainder = (integral << unit_exponent) + fractional;
        match handler.digit(
            digit,
            divisor << unit_exponent,
            remainder,
            error,
            exponent,
            true,
        ) {
            Decision::More => (),
            Decision::Fallback => {
                return fallback(
                    original,
                    exponent + handler.digits.len() as i32 - cache_decimal - 1,
                );
            }
            Decision::Done => return Ok((handler.finish(), exponent - cache_decimal)),
        }
    }
    loop {
        fractional *= 10;
        error *= 10;
        let digit = (fractional >> unit_exponent) as u8;
        fractional &= one - 1;
        exponent -= 1;
        match handler.digit(digit, one, fractional, error, exponent, false) {
            Decision::More => (),
            Decision::Fallback => {
                return fallback(
                    original,
                    exponent + handler.digits.len() as i32 - cache_decimal - 1,
                );
            }
            Decision::Done => return Ok((handler.finish(), exponent - cache_decimal)),
        }
    }
}

enum Decision {
    More,
    Done,
    Fallback,
}

struct Shortest {
    digits: Vec<u8>,
    difference: u64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Shortest {
    fn digit(
        &mut self,
        digit: u8,
        divisor: u64,
        mut remainder: u64,
        error: u64,
        exponent: i32,
        integral: bool,
    ) -> Decision {
        self.digits.push(b'0' + digit);
        if remainder >= error {
            return Decision::More;
        }
        let unit = if integral {
            1
        } else {
            10_u64.pow((-exponent) as u32)
        };
        let up = (self.difference - 1) * unit;
        while remainder < up
            && error - remainder >= divisor
            && (remainder + divisor < up || up - remainder >= remainder + divisor - up)
        {
            *self.digits.last_mut().unwrap() -= 1;
            remainder += divisor;
        }
        let down = (self.difference + 1) * unit;
        if remainder < down
            && error - remainder >= divisor
            && (remainder + divisor < down || down - remainder > remainder + divisor - down)
        {
            return Decision::Fallback;
        }
        if 2 * unit <= remainder && remainder <= error - 4 * unit {
            Decision::Done
        } else {
            Decision::Fallback
        }
    }
    fn finish(self) -> String {
        // Every stored byte is an ASCII decimal digit.
        self.digits.into_iter().map(char::from).collect()
    }
}

/// A bounded exact integer for binary64 text conversion. Forty 32-bit words
/// exceed the numerator/denominator bounds of every finite binary64 input and
/// its decimal estimate (including 10^324 times a 55-bit significand). This is
/// a formatting-local workspace, not a restriction on database numeric values.
#[derive(Clone)]
struct Wide {
    words: [u32; 40],
    len: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Wide {
    fn new(value: u64) -> Self {
        let mut words = [0; 40];
        words[0] = value as u32;
        words[1] = (value >> 32) as u32;
        Self {
            words,
            len: if words[1] == 0 { 1 } else { 2 },
        }
    }
    fn push(&mut self, value: u32) -> Result<()> {
        let target = self
            .words
            .get_mut(self.len)
            .ok_or_else(|| Error::Internal("floating fallback integer capacity".into()))?;
        *target = value;
        self.len += 1;
        Ok(())
    }
    fn multiply(&mut self, factor: u64) -> Result<()> {
        let mut carry = 0_u128;
        for word in &mut self.words[..self.len] {
            let result = u128::from(*word) * u128::from(factor) + carry;
            *word = result as u32;
            carry = result >> 32;
        }
        while carry != 0 {
            self.push(carry as u32)?;
            carry >>= 32;
        }
        Ok(())
    }
    fn shift(&mut self, bits: i32) -> Result<()> {
        let bits = usize::try_from(bits)
            .map_err(|_| Error::Internal("floating fallback negative shift".into()))?;
        let words = bits / 32;
        if self.len + words > self.words.len() {
            return Err(Error::Internal("floating fallback shift capacity".into()));
        }
        self.words.copy_within(0..self.len, words);
        self.words[..words].fill(0);
        self.len += words;
        if bits % 32 != 0 {
            self.multiply(1_u64 << (bits % 32))?;
        }
        Ok(())
    }
    fn power10(exponent: i32) -> Result<Self> {
        let exponent = usize::try_from(exponent)
            .map_err(|_| Error::Internal("floating fallback decimal exponent".into()))?;
        let mut result = Self::new(1);
        for _ in 0..exponent {
            result.multiply(10)?;
        }
        Ok(result)
    }
    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        self.len.cmp(&other.len).then_with(|| {
            self.words[..self.len]
                .iter()
                .rev()
                .cmp(other.words[..other.len].iter().rev())
        })
    }
    fn add(&mut self, other: &Self) -> Result<()> {
        let length = self.len.max(other.len);
        let mut carry = 0_u64;
        for index in 0..length {
            let a = if index < self.len {
                self.words[index]
            } else {
                0
            };
            let b = if index < other.len {
                other.words[index]
            } else {
                0
            };
            let value = u64::from(a) + u64::from(b) + carry;
            self.words[index] = value as u32;
            carry = value >> 32;
        }
        self.len = length;
        if carry != 0 {
            self.push(carry as u32)?;
        }
        Ok(())
    }
    fn subtract(&mut self, other: &Self) {
        let mut borrow = 0_u64;
        for index in 0..self.len {
            let left = u64::from(self.words[index]);
            let right = u64::from(if index < other.len {
                other.words[index]
            } else {
                0
            }) + borrow;
            self.words[index] = left.wrapping_sub(right) as u32;
            borrow = u64::from(left < right);
        }
        while self.len > 1 && self.words[self.len - 1] == 0 {
            self.len -= 1;
        }
    }
    fn digit(&mut self, denominator: &Self) -> u8 {
        let mut digit = 0;
        while !self.compare(denominator).is_lt() {
            self.subtract(denominator);
            digit += 1;
        }
        digit
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fallback(value: Fp, mut exponent: i32) -> Result<(String, i32)> {
    // Preserve the source's FPP fallback, including its asymmetric power-of-two
    // branch. In value.e >= 0, it shifts the denominator by one even when the
    // significand shifts by two. Do not silently repair that observable rule.
    let shift = if value.significand == 1 << 52 && value.exponent > -1074 {
        2
    } else {
        1
    };
    let significand = value.significand << shift;
    let (mut numerator, denominator, mut lower, mut upper) = if value.exponent >= 0 {
        let mut numerator = Wide::new(significand);
        numerator.shift(value.exponent)?;
        let mut lower = Wide::new(1);
        lower.shift(value.exponent)?;
        let mut upper = lower.clone();
        if shift != 1 {
            upper.shift(1)?;
        }
        let mut denominator = Wide::power10(exponent)?;
        denominator.shift(1)?;
        (numerator, denominator, lower, upper)
    } else if exponent < 0 {
        let mut numerator = Wide::power10(-exponent)?;
        let lower = numerator.clone();
        let mut upper = lower.clone();
        if shift != 1 {
            upper.shift(1)?;
        }
        numerator.multiply(significand)?;
        let mut denominator = Wide::new(1);
        denominator.shift(shift - value.exponent)?;
        (numerator, denominator, lower, upper)
    } else {
        let numerator = Wide::new(significand);
        let mut denominator = Wide::power10(exponent)?;
        denominator.shift(shift - value.exponent)?;
        (
            numerator,
            denominator,
            Wide::new(1),
            Wide::new(if shift == 1 { 1 } else { 2 }),
        )
    };
    let even = value.significand.is_multiple_of(2);
    let mut digits = Vec::with_capacity(17);
    loop {
        let digit = numerator.digit(&denominator);
        let low = numerator.compare(&lower);
        let low = low.is_lt() || (even && low.is_eq());
        let mut high = numerator.clone();
        high.add(&upper)?;
        let high = high.compare(&denominator);
        let high = high.is_gt() || (even && high.is_eq());
        digits.push(b'0' + digit);
        if low || high {
            if !low {
                *digits.last_mut().unwrap() += 1;
            } else if high {
                let mut twice = numerator.clone();
                twice.multiply(2)?;
                let order = twice.compare(&denominator);
                if order.is_gt() || (order.is_eq() && !digit.is_multiple_of(2)) {
                    *digits.last_mut().unwrap() += 1;
                }
            }
            exponent -= digits.len() as i32 - 1;
            return Ok((digits.into_iter().map(char::from).collect(), exponent));
        }
        numerator.multiply(10)?;
        lower.multiply(10)?;
        upper.multiply(10)?;
    }
}
