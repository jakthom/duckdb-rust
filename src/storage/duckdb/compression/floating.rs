use super::super::binary::{corrupt, u32_at, u64_at};
use crate::{
    common::{DataType, Error, Result, Value},
    storage::compression::SegmentType,
};

#[derive(Clone, Copy)]
pub(super) enum Floating {
    Single,
    Double,
}

impl Floating {
    pub fn for_segment(kind: SegmentType<'_>) -> Result<Self> {
        match kind {
            SegmentType::Values(DataType::Float) => Ok(Self::Single),
            SegmentType::Values(DataType::Double) => Ok(Self::Double),
            _ => Err(Error::Unsupported(
                "floating-point compression segment type".into(),
            )),
        }
    }
    pub fn bits(self) -> usize {
        match self {
            Self::Single => 32,
            Self::Double => 64,
        }
    }
    pub fn value(self, bits: u64) -> Value {
        match self {
            Self::Single => Value::Float(f32::from_bits(bits as u32)),
            Self::Double => Value::Double(f64::from_bits(bits)),
        }
    }
    pub fn read(self, bytes: &[u8], offset: usize) -> Result<Value> {
        Ok(self.value(match self {
            Self::Single => u64::from(u32_at(bytes, offset)?),
            Self::Double => u64_at(bytes, offset)?,
        }))
    }
    pub fn combine(self, left: u16, right: u64, right_width: usize) -> Result<Value> {
        if usize::from(left) >> (self.bits() - right_width) != 0 {
            return Err(corrupt("floating dictionary value exceeds its width"));
        }
        Ok(self.value((u64::from(left) << right_width) | right))
    }
}
