//! Exact numeric representations and metadata. Runtime semantics are selected
//! through the ordinary type, cast and operator registries.
use super::{DataType, Error, Result, Value};
use std::fmt;

// Exact decimal bounds are immutable metadata, not per-value exponentiation.
pub(crate) const DECIMAL_POWERS: [u128; 39] = {
    let mut powers = [1; 39];
    let mut index = 1;
    while index < powers.len() {
        powers[index] = powers[index - 1] * 10;
        index += 1;
    }
    powers
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DataType {
    pub fn unsigned_bits(&self) -> Option<u8> {
        match self {
            Self::UTinyInt => Some(8),
            Self::USmallInt => Some(16),
            Self::UInteger => Some(32),
            Self::UBigInt => Some(64),
            Self::UHugeInt => Some(128),
            _ => None,
        }
    }
    pub fn is_unsigned_integer(&self) -> bool {
        self.unsigned_bits().is_some()
    }
    pub fn is_decimal(&self) -> bool {
        matches!(self, Self::Decimal { .. })
    }
    pub fn decimal_properties(&self) -> Option<(u8, u8)> {
        use DataType::*;
        Some(match self {
            Decimal { width, scale } => (*width, *scale),
            TinyInt | UTinyInt => (3, 0),
            SmallInt | USmallInt => (5, 0),
            Integer | UInteger => (10, 0),
            BigInt => (19, 0),
            UBigInt => (20, 0),
            HugeInt | UHugeInt => (38, 0),
            Boolean => (1, 0),
            Null => (0, 0),
            _ => return None,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn common_type(left: &DataType, right: &DataType) -> Result<DataType> {
    use DataType::*;
    if *left == Double || *right == Double {
        return Ok(Double);
    }
    if *left == Bignum || *right == Bignum {
        return if left.is_decimal() || right.is_decimal() {
            Err(Error::Bind(
                "BIGNUM and DECIMAL require an explicit common cast".into(),
            ))
        } else {
            Ok(Bignum)
        };
    }
    if *left == Float || *right == Float {
        return Ok(Float);
    }
    if left.is_decimal() || right.is_decimal() {
        let (a, sa) = left
            .decimal_properties()
            .ok_or_else(|| Error::Bind("invalid decimal coercion".into()))?;
        let (b, sb) = right
            .decimal_properties()
            .ok_or_else(|| Error::Bind("invalid decimal coercion".into()))?;
        let integral = (a - sa).max(b - sb);
        let mut scale = sa.max(sb);
        if left.is_decimal() && right.is_decimal() {
            scale = scale.min(38 - integral);
        }
        return Ok(Decimal {
            width: (integral + scale).min(38),
            scale,
        });
    }
    if left.is_unsigned_integer() && right.is_unsigned_integer() {
        return Ok(if left.unsigned_bits() > right.unsigned_bits() {
            left.clone()
        } else {
            right.clone()
        });
    }
    if left.is_signed_integer() && right.is_signed_integer() {
        return Ok(if left.integer_bits() > right.integer_bits() {
            left.clone()
        } else {
            right.clone()
        });
    }
    let (signed, unsigned) = if left.is_unsigned_integer() {
        (right, left)
    } else {
        (left, right)
    };
    let signed_bits = signed
        .integer_bits()
        .ok_or_else(|| Error::Bind("invalid numeric coercion".into()))?;
    let unsigned_bits = unsigned
        .unsigned_bits()
        .ok_or_else(|| Error::Bind("invalid numeric coercion".into()))?;
    for (bits, data_type) in [
        (8, TinyInt),
        (16, SmallInt),
        (32, Integer),
        (64, BigInt),
        (128, HugeInt),
    ] {
        if bits >= signed_bits && bits > unsigned_bits {
            return Ok(data_type);
        }
    }
    Ok(Double)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn decimal(value: i128, width: u8, scale: u8) -> Result<Value> {
    let result = Value::Decimal {
        value,
        width,
        scale,
    };
    if !result.fits_type(&DataType::Decimal { width, scale }) {
        return Err(Error::Conversion(format!(
            "decimal value outside DECIMAL({width},{scale})"
        )));
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn rescale(value: i128, from: u8, to: u8) -> Result<i128> {
    let factor = *DECIMAL_POWERS
        .get(usize::from(from.abs_diff(to)))
        .ok_or_else(|| Error::Conversion("invalid decimal scale".into()))? as i128;
    if to >= from {
        value
            .checked_mul(factor)
            .ok_or_else(|| Error::Conversion("decimal scale overflow".into()))
    } else {
        let divisor = factor;
        let quotient = value / divisor;
        let remainder = value % divisor;
        // Half away from zero, without overflowing at either signed boundary.
        Ok(quotient
            + if remainder.unsigned_abs() >= (divisor / 2) as u128 {
                value.signum()
            } else {
                0
            })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn format_decimal(value: i128, scale: u8, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if scale == 0 {
        return write!(f, "{value}");
    }
    let magnitude = value.unsigned_abs();
    let divisor = 10_u128.checked_pow(u32::from(scale)).ok_or(fmt::Error)?;
    if value < 0 {
        write!(f, "-")?;
    }
    write!(
        f,
        "{}.{:0width$}",
        magnitude / divisor,
        magnitude % divisor,
        width = usize::from(scale)
    )
}

/// Compare exact decimal values without rescaling the entire coefficient
/// (which can overflow) or converting significant digits to floating point.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn compare_decimal(a: i128, sa: u8, b: i128, sb: u8) -> Result<std::cmp::Ordering> {
    if sa > 38 || sb > 38 {
        return Err(Error::Conversion("invalid decimal scale".into()));
    }
    if sa == sb {
        return Ok(a.cmp(&b));
    }
    if a.signum() != b.signum() {
        return Ok(a.signum().cmp(&b.signum()));
    }
    let parts = |n: i128, scale: u8| {
        let power = 10_u128.pow(u32::from(scale));
        (
            n.unsigned_abs() / power,
            (n.unsigned_abs() % power) * 10_u128.pow(u32::from(38 - scale)),
        )
    };
    let order = parts(a, sa).cmp(&parts(b, sb));
    Ok(if a < 0 { order.reverse() } else { order })
}
