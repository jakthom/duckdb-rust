use super::binary::{corrupt, u32_at, u64_at};
use crate::common::{DataType, Error, Result, Value};
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn width(data_type: &DataType) -> Result<usize> {
    match data_type {
        DataType::Boolean | DataType::TinyInt => Ok(1),
        DataType::SmallInt => Ok(2),
        DataType::Integer | DataType::Float | DataType::Date => Ok(4),
        DataType::BigInt | DataType::Double => Ok(8),
        DataType::HugeInt => Ok(16),
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
        DataType::HugeInt => Ok(50),
        _ => Err(Error::Unsupported(format!(
            "DuckDB storage type {data_type}"
        ))),
    }
}
