//! Constant values used by native catalog metadata outside retained expressions.
use super::{DataType, Reader, Result, Value, corrupt, logical_type};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn value(reader: &mut Reader) -> Result<Value> {
    reader.field(100)?;
    let data_type = logical_type(reader)?;
    reader.field(101)?;
    let value = if reader.boolean()? {
        Value::Null
    } else {
        reader.field(102)?;
        match data_type {
            DataType::Boolean => Value::Boolean(reader.boolean()?),
            DataType::Date => Value::Date(super::super::binary::date(reader.signed()?)?),
            ref t if t.is_temporal() => super::super::temporal::read_metadata(reader, t)?,
            DataType::TinyInt | DataType::SmallInt | DataType::Integer | DataType::BigInt => {
                Value::Integer(reader.signed()? as i128).cast(&data_type)?
            }
            DataType::HugeInt => {
                let upper = reader.signed()? as i128;
                let lower = reader.unsigned()? as i128;
                Value::Integer((upper << 64) | lower)
            }
            DataType::Float => Value::Float(reader.float()?),
            DataType::Double => Value::Double(reader.double()?),
            DataType::Varchar => Value::Varchar(reader.string()?),
            DataType::Bit => {
                crate::common::BitString::from_native(&reader.blob()?, || Ok(()))?.value()
            }
            DataType::Bignum => {
                crate::common::BignumValue::from_native(&reader.blob()?, || Ok(()))?.value()
            }
            DataType::Blob => Value::Blob(
                crate::common::scalar::parse_blob(&reader.string()?, || Ok(()))
                    .map_err(|_| corrupt("invalid serialized BLOB literal"))?,
            ),
            DataType::Uuid | DataType::Enum(_) => {
                super::super::primitive::read_numeric(reader, &data_type)?
            }
            _ if data_type.is_decimal() || data_type.is_unsigned_integer() => {
                super::super::primitive::read_numeric(reader, &data_type)?
            }
            _ => return Err(corrupt("non-NULL value with NULL type")),
        }
    };
    reader.end()?;
    Ok(value)
}
