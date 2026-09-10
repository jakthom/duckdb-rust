use super::binary::{corrupt, u32_at, u64_at};
use crate::common::{DataType, Error, Result, Value};
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn width(data_type: &DataType) -> Result<usize> {
    match data_type {
        DataType::Boolean | DataType::TinyInt | DataType::UTinyInt => Ok(1),
        DataType::SmallInt | DataType::USmallInt => Ok(2),
        DataType::Integer | DataType::UInteger | DataType::Float | DataType::Date => Ok(4),
        DataType::BigInt | DataType::UBigInt | DataType::Double => Ok(8),
        DataType::HugeInt | DataType::UHugeInt | DataType::Uuid => Ok(16),
        DataType::Decimal { width, .. } => Ok(match width {
            1..=4 => 2,
            5..=9 => 4,
            10..=18 => 8,
            19..=38 => 16,
            _ => return Err(corrupt("invalid decimal width")),
        }),
        _ => Err(Error::Unsupported(format!(
            "fixed-width storage for {data_type}"
        ))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn integer(data: &[u8], offset: usize, width: usize) -> Result<i128> {
    let data = data
        .get(
            offset
                ..offset
                    .checked_add(width)
                    .ok_or_else(|| corrupt("integer offset overflow"))?,
        )
        .ok_or_else(|| corrupt("truncated integer"))?;
    let mut bytes = if data.last().is_some_and(|v| v & 128 != 0) {
        [255; 16]
    } else {
        [0; 16]
    };
    bytes[..width].copy_from_slice(data);
    Ok(i128::from_le_bytes(bytes))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn scalar(data: &[u8], offset: usize, data_type: &DataType) -> Result<Value> {
    match data_type {
        DataType::Boolean => match data.get(offset) {
            Some(0) => Ok(Value::Boolean(false)),
            Some(1) => Ok(Value::Boolean(true)),
            Some(_) => Ok(Value::Null),
            None => Err(corrupt("truncated Boolean")),
        },
        DataType::Float => Ok(Value::Float(f32::from_bits(u32_at(data, offset)?))),
        DataType::Double => Ok(Value::Double(f64::from_bits(u64_at(data, offset)?))),
        _ => integer_value(integer(data, offset, width(data_type)?)?, data_type),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Numeric codecs operate on physical integers. Reconstruct the logical value
/// here; column validity is applied by the caller after segment decoding.
pub(super) fn integer_value(value: i128, data_type: &DataType) -> Result<Value> {
    if *data_type == DataType::Uuid {
        return Ok(Value::Uuid((value as u128) ^ (1_u128 << 127)));
    }
    if let Some(bits) = data_type.unsigned_bits() {
        return Ok(Value::Unsigned(if bits == 128 {
            value as u128
        } else {
            (value as u128) & ((1_u128 << bits) - 1)
        }));
    }
    if let DataType::Decimal { width, scale } = data_type {
        return Ok(Value::Decimal {
            value,
            width: *width,
            scale: *scale,
        });
    }
    if *data_type == DataType::Date {
        let days = i32::try_from(value).map_err(|_| corrupt("DATE width overflow"))?;
        if days == i32::MIN {
            return Ok(Value::Null);
        }
        return super::binary::date(i64::from(days)).map(Value::Date);
    }
    Ok(Value::Integer(value))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn type_id(data_type: &DataType) -> Result<u64> {
    match data_type {
        DataType::Boolean => Ok(10),
        DataType::TinyInt => Ok(11),
        DataType::SmallInt => Ok(12),
        DataType::Integer => Ok(13),
        DataType::BigInt => Ok(14),
        DataType::Date => Ok(15),
        DataType::Float => Ok(22),
        DataType::Double => Ok(23),
        DataType::Varchar => Ok(25),
        DataType::Blob => Ok(26),
        DataType::Uuid => Ok(54),
        DataType::HugeInt => Ok(50),
        DataType::UTinyInt => Ok(28),
        DataType::USmallInt => Ok(29),
        DataType::UInteger => Ok(30),
        DataType::UBigInt => Ok(31),
        DataType::UHugeInt => Ok(49),
        DataType::Decimal { .. } => Ok(21),
        _ => Err(Error::Unsupported(format!(
            "DuckDB storage type {data_type}"
        ))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write_type(output: &mut super::binary::Encoder, data_type: &DataType) -> Result<()> {
    crate::common::type_registry::check_metadata(data_type)?;
    output.property(100, type_id(data_type)?);
    if let DataType::Decimal { width, scale } = data_type {
        output.field(101);
        output.boolean(true);
        output.property(100, 2); // DECIMAL_TYPE_INFO
        output.property(200, u64::from(*width));
        if *scale != 0 {
            output.property(201, u64::from(*scale));
        }
        output.end();
    }
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write_numeric(
    output: &mut super::binary::Encoder,
    value: &Value,
    data_type: &DataType,
) -> Result<()> {
    if data_type.is_unsigned_integer() {
        let Value::Unsigned(n) = value else {
            return Err(corrupt("unsigned numeric metadata"));
        };
        if *data_type == DataType::UHugeInt {
            output.unsigned((n >> 64) as u64);
        }
        output.unsigned(*n as u64);
    } else {
        let n = match value {
            Value::Decimal { value, .. } => *value,
            Value::Uuid(value) => (*value ^ (1_u128 << 127)) as i128,
            _ => value.as_i128()?,
        };
        if width(data_type)? == 16 {
            output.signed((n >> 64) as i64);
            output.unsigned(n as u64);
        } else {
            output.signed(i64::try_from(n).map_err(|_| corrupt("numeric metadata overflow"))?);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_numeric(
    reader: &mut super::binary::Reader,
    data_type: &DataType,
) -> Result<Value> {
    if data_type.is_unsigned_integer() {
        let upper = if *data_type == DataType::UHugeInt {
            u128::from(reader.unsigned()?) << 64
        } else {
            0
        };
        return Ok(Value::Unsigned(upper | u128::from(reader.unsigned()?)));
    }
    let n = if width(data_type)? == 16 {
        (i128::from(reader.signed()?) << 64) | i128::from(reader.unsigned()?)
    } else {
        i128::from(reader.signed()?)
    };
    integer_value(n, data_type)
}
