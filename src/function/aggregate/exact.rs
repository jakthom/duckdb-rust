//! Physical coefficient kernels owned by the built-in SUM adapter. Selection
//! does not change the aggregate interface or bypass registered type semantics.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum SumKernel {
    Signed(u8),
    Unsigned(u8),
    Decimal { width: u8, scale: u8 },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SumKernel {
    pub(super) fn bind(input: &DataType) -> Option<Self> {
        if let Some(bits) = input.integer_bits().filter(|bits| *bits <= 64) {
            Some(Self::Signed(bits))
        } else if let Some(bits) = input.unsigned_bits().filter(|bits| *bits <= 64) {
            Some(Self::Unsigned(bits))
        } else if let DataType::Decimal {
            width: width @ 1..=18,
            scale,
        } = input
        {
            Some(Self::Decimal {
                width: *width,
                scale: *scale,
            })
        } else {
            None
        }
    }
    pub(super) fn result_type(self) -> DataType {
        match self {
            Self::Decimal { scale, .. } => DataType::Decimal { width: 38, scale },
            _ => DataType::HugeInt,
        }
    }
    #[inline]
    pub(super) fn coefficient(self, value: &Value) -> Option<i128> {
        match (self, value) {
            (Self::Signed(_), Value::Integer(value)) => Some(*value),
            (Self::Unsigned(_), Value::Unsigned(value)) => Some(*value as i128),
            (Self::Decimal { .. }, Value::Decimal { value, .. }) => Some(*value),
            (_, Value::Null) => None,
            _ => unreachable!("validated SUM input column"),
        }
    }
    pub(super) fn value(self, value: i128) -> Value {
        match self {
            Self::Decimal { scale, .. } => Value::Decimal {
                value,
                width: 38,
                scale,
            },
            _ => Value::Integer(value),
        }
    }
    pub(super) fn maximum_magnitude(self) -> i128 {
        match self {
            Self::Signed(bits) => 1_i128 << (bits - 1),
            Self::Unsigned(bits) => (1_i128 << bits) - 1,
            Self::Decimal { width, .. } => {
                crate::common::numeric::DECIMAL_POWERS[usize::from(width)] as i128 - 1
            }
        }
    }
    pub(super) fn valid_sum(self, value: i128) -> bool {
        match self {
            Self::Decimal { .. } => value.unsigned_abs() < 10_u128.pow(38),
            _ => true,
        }
    }
    /// All prefixes/frames of this many valid inputs fit the result domain.
    pub(super) fn supports_count(self, rows: usize) -> bool {
        i128::try_from(rows)
            .ok()
            .and_then(|rows| self.maximum_magnitude().checked_mul(rows))
            .is_some_and(|bound| self.valid_sum(bound))
    }
    /// Caller proves every prefix fits the result domain. Decode the physical
    /// kind once per block, retaining checked machine-width partial sums.
    pub(super) fn block_sum(self, values: &[Value]) -> i128 {
        if self
            .maximum_magnitude()
            .checked_mul(values.len() as i128)
            .is_some_and(|bound| bound <= i64::MAX as i128)
        {
            // The declared input domain proves every machine-width lane and
            // partial sum fits. Keep the ordinary checked fallback for widths
            // whose metadata alone cannot establish this stronger bound.
            return match self {
                Self::Signed(_) => super::sum_proven_narrow(values, |value| match value {
                    Value::Integer(value) => Some(*value as i64),
                    _ => None,
                }),
                Self::Unsigned(_) => super::sum_proven_narrow(values, |value| match value {
                    Value::Unsigned(value) => Some(*value as i64),
                    _ => None,
                }),
                Self::Decimal { .. } => super::sum_proven_narrow(values, |value| match value {
                    Value::Decimal { value, .. } => Some(*value as i64),
                    _ => None,
                }),
            };
        }
        let narrow = match self {
            Self::Signed(_) => super::sum_narrow(values, |value| match value {
                Value::Integer(value) => Some(*value as i64),
                _ => unreachable!("validated non-NULL signed SUM column"),
            }),
            Self::Decimal { .. } => super::sum_narrow(values, |value| match value {
                Value::Decimal { value, .. } => Some(*value as i64),
                _ => unreachable!("validated non-NULL narrow decimal SUM column"),
            }),
            Self::Unsigned(_) => super::sum_narrow(values, |value| match value {
                Value::Unsigned(value) => i64::try_from(*value).ok(),
                _ => unreachable!("validated non-NULL unsigned SUM column"),
            }),
        };
        narrow.unwrap_or_else(|| {
            values
                .iter()
                .map(|value| self.coefficient(value).unwrap())
                .sum()
        })
    }
}
