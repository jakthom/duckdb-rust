use crate::common::{Error, Result};

pub(super) fn corrupt(message: impl Into<String>) -> Error {
    Error::Corrupt(message.into())
}

/// Decode a non-NULL DATE in metadata. Unlike physical column slots, metadata
/// cannot use the INT32_MIN NULL sentinel when its validity flag is set.
pub(super) fn date(days: i64) -> Result<crate::common::Date> {
    let days = i32::try_from(days).map_err(|_| corrupt("DATE width overflow"))?;
    crate::common::Date::from_days(days).map_err(|_| corrupt("invalid non-NULL DATE"))
}

/// DuckDB binary objects have typed, ordered fields and a u16 terminator.
/// Unknown fields cannot be skipped without their schema and are rejected.
pub(super) struct Reader {
    data: Vec<u8>,
    pub position: usize,
}

impl Reader {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, position: 0 }
    }
    pub fn finished(&self) -> bool {
        self.position == self.data.len()
    }
    pub fn bytes(&mut self, count: usize) -> Result<&[u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| corrupt("read length overflow"))?;
        let result = self
            .data
            .get(self.position..end)
            .ok_or_else(|| corrupt("truncated serialized data"))?;
        self.position = end;
        Ok(result)
    }
    pub fn byte(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    pub fn boolean(&mut self) -> Result<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(corrupt("invalid Boolean")),
        }
    }
    pub fn fixed_u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.bytes(8)?.try_into().map_err(|_| corrupt("u64"))?,
        ))
    }
    pub fn float(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(
            self.bytes(4)?.try_into().map_err(|_| corrupt("float"))?,
        ))
    }
    pub fn double(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.fixed_u64()?))
    }
    pub fn unsigned(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = self.byte()?;
            if shift == 63 && byte > 1 {
                return Err(corrupt("unsigned LEB128 overflow"));
            }
            value |= u64::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        Err(corrupt("unterminated LEB128"))
    }
    pub fn signed(&mut self) -> Result<i64> {
        let mut value = 0i128;
        for shift in (0..70).step_by(7) {
            let byte = self.byte()?;
            value |= i128::from(byte & 127) << shift;
            if byte & 128 == 0 {
                if byte & 64 != 0 {
                    value |= !0i128 << (shift + 7);
                }
                return i64::try_from(value).map_err(|_| corrupt("signed LEB128 overflow"));
            }
        }
        Err(corrupt("unterminated signed LEB128"))
    }
    pub fn length(&mut self) -> Result<usize> {
        let length = usize::try_from(self.unsigned()?).map_err(|_| corrupt("length overflow"))?;
        if length > 16_777_216 {
            return Err(Error::Resource(
                "serialized collection exceeds 16 million items".into(),
            ));
        }
        Ok(length)
    }
    pub fn blob(&mut self) -> Result<Vec<u8>> {
        let count = self.length()?;
        Ok(self.bytes(count)?.to_vec())
    }
    pub fn string(&mut self) -> Result<String> {
        String::from_utf8(self.blob()?).map_err(|_| corrupt("invalid UTF-8 metadata"))
    }
    pub fn peek(&self) -> Result<u16> {
        let bytes = self
            .data
            .get(self.position..self.position + 2)
            .ok_or_else(|| corrupt("truncated field identifier"))?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }
    pub fn field(&mut self, expected: u16) -> Result<()> {
        let actual = self.peek()?;
        if actual != expected {
            return Err(Error::Unsupported(format!(
                "DuckDB metadata field {actual} at offset {}, expected {expected}",
                self.position
            )));
        }
        self.position += 2;
        Ok(())
    }
    pub fn optional(&mut self, field: u16) -> Result<bool> {
        if self.peek()? == field {
            self.position += 2;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn end(&mut self) -> Result<()> {
        self.field(u16::MAX)
    }
    pub fn optional_unsigned(&mut self, field: u16, default: u64) -> Result<u64> {
        if self.optional(field)? {
            self.unsigned()
        } else {
            Ok(default)
        }
    }
    pub fn pointer(&mut self) -> Result<(u64, usize)> {
        let block = self.optional_unsigned(100, 0)?;
        let offset = self.optional_unsigned(101, 0)?;
        self.end()?;
        Ok((
            block,
            usize::try_from(offset).map_err(|_| corrupt("metadata offset overflow"))?,
        ))
    }
}

pub(super) fn u16_at(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(
            offset
                ..offset
                    .checked_add(2)
                    .ok_or_else(|| corrupt("offset overflow"))?,
        )
        .ok_or_else(|| corrupt("truncated u16"))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

pub(super) fn u32_at(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(
            offset
                ..offset
                    .checked_add(4)
                    .ok_or_else(|| corrupt("offset overflow"))?,
        )
        .ok_or_else(|| corrupt("truncated u32"))?;
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| corrupt("u32"))?,
    ))
}
pub(super) fn u64_at(data: &[u8], offset: usize) -> Result<u64> {
    let bytes = data
        .get(
            offset
                ..offset
                    .checked_add(8)
                    .ok_or_else(|| corrupt("offset overflow"))?,
        )
        .ok_or_else(|| corrupt("truncated u64"))?;
    Ok(u64::from_le_bytes(
        bytes.try_into().map_err(|_| corrupt("u64"))?,
    ))
}

pub(super) fn checksum(data: &[u8]) -> Result<u64> {
    let mut result = 5381;
    for chunk in data.chunks_exact(8) {
        result ^= u64::from_le_bytes(chunk.try_into().map_err(|_| corrupt("checksum word"))?)
            .wrapping_mul(0xbf58476d1ce4e5b9);
    }
    let remainder = &data[data.len() / 8 * 8..];
    if !remainder.is_empty() {
        const M: u64 = 0xc6a4_a793_5bd1_e995;
        let mut hash = 0xe17a_1465 ^ (remainder.len() as u64).wrapping_mul(M);
        for (index, byte) in remainder.iter().enumerate() {
            hash ^= u64::from(*byte) << (index * 8);
        }
        hash = hash.wrapping_mul(M);
        hash ^= hash >> 47;
        hash = hash.wrapping_mul(M);
        hash ^= hash >> 47;
        result ^= hash;
    }
    Ok(result)
}

#[derive(Default)]
pub(super) struct Encoder(pub(super) Vec<u8>);

impl Encoder {
    pub(super) fn field(&mut self, id: u16) {
        self.0.extend(id.to_le_bytes());
    }
    pub(super) fn end(&mut self) {
        self.field(u16::MAX);
    }
    pub(super) fn boolean(&mut self, value: bool) {
        self.0.push(u8::from(value));
    }
    pub(super) fn unsigned(&mut self, mut value: u64) {
        loop {
            let mut byte = (value & 127) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 128;
            }
            self.0.push(byte);
            if value == 0 {
                break;
            }
        }
    }
    pub(super) fn signed(&mut self, mut value: i64) {
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            let last = (value == 0 && byte & 64 == 0) || (value == -1 && byte & 64 != 0);
            self.0.push(if last { byte } else { byte | 128 });
            if last {
                break;
            }
        }
    }
    pub(super) fn string(&mut self, value: &str) -> Result<()> {
        if value.len() > 16_777_216 {
            return Err(Error::Resource(
                "DuckDB metadata string exceeds 16 MiB".into(),
            ));
        }
        self.blob(value.as_bytes());
        Ok(())
    }
    pub(super) fn blob(&mut self, value: &[u8]) {
        self.unsigned(value.len() as u64);
        self.0.extend(value);
    }
    pub(super) fn property(&mut self, id: u16, value: u64) {
        self.field(id);
        self.unsigned(value);
    }
    pub(super) fn pointer(&mut self, pointer: u64) {
        self.property(100, pointer);
        self.end();
    }
}
