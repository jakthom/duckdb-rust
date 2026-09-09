//! Constant defaults use DuckDB's parsed-expression/value metadata, separate
//! from physical column encoding. Dynamic expressions remain unsupported.
use super::{DataType, Error, Reader, Result, Value, corrupt, logical_type};

pub(super) fn read(reader: &mut Reader, depth: usize) -> Result<Value> {
    if depth > 128 {
        return Err(Error::Resource(
            "default expression nesting exceeds 128".into(),
        ));
    }
    reader.field(100)?;
    let class = reader.unsigned()?;
    reader.field(101)?;
    let kind = reader.unsigned()?;
    if reader.optional(102)? && !reader.string()?.is_empty() {
        return Err(Error::Unsupported(
            "aliased DuckDB default expression".into(),
        ));
    }
    if reader.optional(103)? {
        reader.unsigned()?; // Source query location, not an evaluation input.
    }
    let value = match (class, kind) {
        (7, 75) => {
            reader.field(200)?;
            value(reader)?
        }
        (3, 12) => {
            reader.field(200)?;
            if !reader.boolean()? {
                return Err(corrupt("cast default has no child"));
            }
            let value = read(reader, depth + 1)?;
            reader.field(201)?;
            let target = logical_type(reader)?;
            let try_cast = reader.optional(202)? && reader.boolean()?;
            match value.cast(&target) {
                Ok(value) => value,
                Err(Error::Conversion(_)) if try_cast => Value::Null,
                Err(error) => return Err(error),
            }
        }
        _ => {
            return Err(Error::Unsupported(format!(
                "DuckDB default expression class {class}, kind {kind}"
            )));
        }
    };
    reader.end()?;
    Ok(value)
}

fn value(reader: &mut Reader) -> Result<Value> {
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
            _ => return Err(corrupt("non-NULL value with NULL type")),
        }
    };
    reader.end()?;
    Ok(value)
}
