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
    /// The caller proves all column prefixes fit the SQL accumulator. Select
    /// the physical loader and machine-width proof once per column, while
    /// retaining bounded cancellation checks and the checked wide fallback.
    pub(super) fn column_sum(
        self,
        values: &[Value],
        query: &crate::parallel::QueryContext,
    ) -> Result<i128> {
        let narrow = self
            .maximum_magnitude()
            .checked_mul(values.len().min(1024) as i128)
            .is_some_and(|bound| bound <= i64::MAX as i128);
        if narrow {
            return match self {
                Self::Signed(_) => proven_column(values, query, |value| match value {
                    Value::Integer(value) => Some(*value as i64),
                    _ => None,
                }),
                Self::Unsigned(_) => proven_column(values, query, |value| match value {
                    Value::Unsigned(value) => Some(*value as i64),
                    _ => None,
                }),
                Self::Decimal { .. } => proven_column(values, query, |value| match value {
                    Value::Decimal { value, .. } => Some(*value as i64),
                    _ => None,
                }),
            };
        }
        let mut sum = 0_i128;
        for block in values.chunks(1024) {
            query.check()?;
            sum += self.block_sum(block);
        }
        query.check()?;
        Ok(sum)
    }
    /// DECIMAL widths through 18 use a signed 64-bit physical vector in both
    /// pins. The caller already proved every SQL prefix fits the DECIMAL(38,s)
    /// result, so independent machine lanes can reduce the compact payload
    /// before one exact widening step per block.
    pub(super) fn column_sum_decimal_i64(
        self,
        values: &[i64],
        query: &crate::parallel::QueryContext,
    ) -> Result<i128> {
        let Self::Decimal { .. } = self else {
            return Err(Error::Internal(
                "physical DECIMAL coefficients used by another SUM domain".into(),
            ));
        };
        let narrow = self
            .maximum_magnitude()
            .checked_mul(values.len().min(1024) as i128)
            .is_some_and(|bound| bound <= i64::MAX as i128);
        let mut sum = 0_i128;
        for block in values.chunks(1024) {
            query.check()?;
            sum += if narrow {
                sum_proven_i64(block)
            } else {
                sum_i64_wide(block)
            };
        }
        query.check()?;
        Ok(sum)
    }
    /// BIGINT's authoritative physical lane is a contiguous i64 slice. Keep
    /// SUM on that lane rather than reconstructing `Value::Integer` for each
    /// row through the generic encoding seam.
    pub(super) fn column_sum_signed_i64(
        self,
        values: &[i64],
        ordered: bool,
        query: &crate::parallel::QueryContext,
    ) -> Result<i128> {
        let Self::Signed(64) = self else {
            return Err(Error::Internal(
                "BIGINT physical coefficients used by another SUM domain".into(),
            ));
        };
        if ordered && ordered_signed_i64_sum_fits(values) {
            return sum_ordered_signed_i64_wrapping(values, query);
        }
        let mut sum = 0_i128;
        for block in values.chunks(1024) {
            query.check()?;
            sum += sum_i64_wide(block);
        }
        query.check()?;
        Ok(sum)
    }
    /// Reduce a selection over an immediate all-valid flat BIGINT parent
    /// without reconstructing owned Values. The selection indexes the parent's
    /// logical view, so a sliced parent is already represented by `values`.
    /// A compact parent is reduced by counting its selected entries once;
    /// larger parents retain one exact wide addition per selected row. The
    /// caller's state bound proves every SQL prefix fits the accumulator.
    pub(super) fn selected_bigint_sum(
        self,
        values: &[i64],
        selection: &[usize],
        query: &crate::parallel::QueryContext,
    ) -> Result<Option<i128>> {
        if !matches!(self, Self::Signed(64)) || selection.is_empty() {
            return Ok(None);
        }
        let contribution = if values.len() <= 256 {
            let mut counts = vec![0_usize; values.len()];
            for (offset, &index) in selection.iter().enumerate() {
                if offset % 1024 == 0 {
                    query.check()?;
                }
                let count = counts.get_mut(index).ok_or_else(|| {
                    Error::Internal("dictionary selection outside BIGINT parent".into())
                })?;
                *count += 1;
            }
            values
                .iter()
                .zip(counts)
                .try_fold(0_i128, |sum, (&value, count)| {
                    i128::try_from(count)
                        .ok()
                        .and_then(|count| i128::from(value).checked_mul(count))
                        .and_then(|value| sum.checked_add(value))
                        .ok_or_else(|| Error::Execution("sum overflow".into()))
                })?
        } else {
            let mut sum = 0_i128;
            for (offset, &index) in selection.iter().enumerate() {
                if offset % 1024 == 0 {
                    query.check()?;
                }
                sum = sum
                    .checked_add(i128::from(*values.get(index).ok_or_else(|| {
                        Error::Internal("dictionary selection outside BIGINT parent".into())
                    })?))
                    .ok_or_else(|| Error::Execution("sum overflow".into()))?;
            }
            sum
        };
        query.check()?;
        Ok(Some(contribution))
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

/// A monotone all-valid BIGINT vector with these endpoint bounds has every
/// logical prefix inside i64.  The zero bound covers positive-only and
/// negative-only prefixes, which need not lie between the two full sums.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn ordered_signed_i64_sum_fits(values: &[i64]) -> bool {
    let Some((&first, &last)) = values.first().zip(values.last()) else {
        return false;
    };
    if first > last {
        return false;
    }
    let Some(count) = i128::try_from(values.len()).ok() else {
        return false;
    };
    let Some(lower) = count.checked_mul(i128::from(first)) else {
        return false;
    };
    let Some(upper) = count.checked_mul(i128::from(last)) else {
        return false;
    };
    i64::try_from(lower.min(0)).is_ok() && i64::try_from(upper.max(0)).is_ok()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn sum_ordered_signed_i64_wrapping(
    values: &[i64],
    query: &crate::parallel::QueryContext,
) -> Result<i128> {
    let mut sum = 0_i64;
    for block in values.chunks(1024) {
        query.check()?;
        for &value in block {
            sum = sum.wrapping_add(value);
        }
    }
    query.check()?;
    Ok(i128::from(sum))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn proven_column(
    values: &[Value],
    query: &crate::parallel::QueryContext,
    integer: impl Fn(&Value) -> Option<i64>,
) -> Result<i128> {
    let mut sum = 0_i128;
    for block in values.chunks(1024) {
        query.check()?;
        sum += super::sum_proven_narrow(block, &integer);
    }
    query.check()?;
    Ok(sum)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn sum_proven_i64(values: &[i64]) -> i128 {
    let mut lanes = [0_i64; 4];
    let mut blocks = values.chunks_exact(4);
    for block in &mut blocks {
        for (lane, value) in lanes.iter_mut().zip(block) {
            *lane += *value;
        }
    }
    let mut sum: i64 = lanes.into_iter().sum();
    sum += blocks.remainder().iter().sum::<i64>();
    i128::from(sum)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn sum_i64_wide(values: &[i64]) -> i128 {
    let mut lanes = [0_i128; 4];
    let mut blocks = values.chunks_exact(4);
    for block in &mut blocks {
        for (lane, value) in lanes.iter_mut().zip(block) {
            *lane += i128::from(*value);
        }
    }
    lanes.into_iter().sum::<i128>()
        + blocks
            .remainder()
            .iter()
            .map(|value| i128::from(*value))
            .sum::<i128>()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        common::{DataType, Error, Value, vector::Vector},
        parallel::{InterruptHandle, QueryContext},
    };

    use super::{SumKernel, ordered_signed_i64_sum_fits};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn ordered_flat_bigint_sum_proves_prefixes_and_declines_extrema() -> crate::Result<()> {
        let query = QueryContext::background();
        let kernel = SumKernel::Signed(64);
        for (values, expected) in [
            (&[-4, -2, -1][..], -7_i128),
            (&[2, 3, 7][..], 12_i128),
            (&[-4, -1, 2, 8][..], 5_i128),
        ] {
            assert!(ordered_signed_i64_sum_fits(values));
            assert_eq!(
                kernel.column_sum_signed_i64(values, true, &query)?,
                expected
            );
            assert_eq!(
                kernel.column_sum_signed_i64(values, false, &query)?,
                expected
            );
        }
        assert!(!ordered_signed_i64_sum_fits(&[]));
        assert_eq!(kernel.column_sum_signed_i64(&[], true, &query)?, 0);
        let extremes = [i64::MAX, i64::MAX];
        assert!(!ordered_signed_i64_sum_fits(&extremes));
        assert_eq!(
            kernel.column_sum_signed_i64(&extremes, true, &query)?,
            i128::from(i64::MAX) * 2
        );
        let minimums = [i64::MIN, i64::MIN];
        assert!(!ordered_signed_i64_sum_fits(&minimums));
        assert_eq!(
            kernel.column_sum_signed_i64(&minimums, true, &query)?,
            i128::from(i64::MIN) * 2
        );
        assert_eq!(
            kernel.column_sum_signed_i64(&[i64::MIN, i64::MAX], true, &query)?,
            -1
        );
        let unknown_order = [0, i64::MAX, i64::MAX, 0];
        assert_eq!(
            kernel.column_sum_signed_i64(&unknown_order, false, &query)?,
            i128::from(i64::MAX) * 2
        );
        assert!(!ordered_signed_i64_sum_fits(&[3, 2]));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn ordered_flat_bigint_sum_honors_preexisting_interruption() {
        let interrupt = InterruptHandle::default();
        interrupt.interrupt();
        let query = QueryContext::new(interrupt, None, 2048, usize::MAX).expect("query context");
        let values = vec![1_i64; 1025];
        assert!(matches!(
            SumKernel::Signed(64).column_sum_signed_i64(&values, true, &query),
            Err(Error::Interrupted)
        ));
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn selected_bigint_lane_keeps_repeated_nonmonotonic_and_sliced_parent_indices()
    -> crate::Result<()> {
        let query = QueryContext::background();
        let parent = Arc::new(Vector::flat(
            DataType::BigInt,
            vec![
                Value::Integer(100),
                Value::Integer(3),
                Value::Integer(7),
                Value::Integer(-2),
                Value::Integer(9),
                Value::Integer(100),
            ],
        )?);
        let sliced = Arc::new(parent.slice(1, 4)?);
        let selected = sliced.select(vec![2, 0, 2, 1])?;
        let (selected_parent, indices) = selected.dictionary().expect("selection encoding");
        assert_eq!(selected_parent.flat_bigints(), Some(&[3, 7, -2, 9][..]));
        assert_eq!(
            SumKernel::Signed(64).selected_bigint_sum(
                selected_parent.flat_bigints().expect("BIGINT lane"),
                indices,
                &query,
            )?,
            Some(6),
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn selected_bigint_lane_keeps_signed_min_and_declines_nested_nullable_shapes()
    -> crate::Result<()> {
        let query = QueryContext::background();
        assert_eq!(
            SumKernel::Signed(64).selected_bigint_sum(&[i64::MIN], &[0], &query)?,
            Some(i128::from(i64::MIN))
        );
        let nullable = Vector::flat(DataType::BigInt, vec![Value::Integer(1), Value::Null])?;
        assert!(!nullable.all_valid());
        assert!(nullable.flat_bigints().is_none());
        let parent = Arc::new(Vector::flat(DataType::BigInt, vec![Value::Integer(1)])?);
        let nested = Arc::new(parent.select(vec![0])?).select(vec![0])?;
        let (nested_parent, _) = nested.dictionary().expect("nested selection");
        assert!(nested_parent.flat_bigints().is_none());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn selected_bigint_lane_checks_cancellation_before_physical_reduction() {
        let interrupt = InterruptHandle::default();
        interrupt.interrupt();
        let query = QueryContext::new(interrupt, None, 1, 16).expect("query context");
        assert!(
            SumKernel::Signed(64)
                .selected_bigint_sum(&[1], &[0], &query)
                .is_err()
        );
    }
}
