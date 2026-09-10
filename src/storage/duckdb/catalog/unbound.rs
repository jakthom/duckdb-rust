//! Development default CASTs can retain a parsed type instead of a resolved
//! logical id. Decode only known parameterless built-ins; never execute stored
//! expressions or interpret unknown/named types as NULL.
use super::{DataType, Error, Reader, Result, corrupt};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(reader: &mut Reader) -> Result<DataType> {
    reader.field(101)?;
    if !reader.boolean()? {
        return Err(corrupt("UNBOUND type without metadata"));
    }
    reader.field(100)?;
    if reader.unsigned()? != 7 {
        return Err(corrupt("invalid UNBOUND type info"));
    }
    if reader.optional(101)? && !reader.string()?.is_empty() {
        return Err(Error::Unsupported("aliased UNBOUND type".into()));
    }
    if reader.optional(102)? && reader.length()? != 0 {
        return Err(Error::Unsupported("legacy UNBOUND modifiers".into()));
    }
    if reader.optional(103)? && reader.boolean()? {
        return Err(Error::Unsupported("extension UNBOUND type".into()));
    }
    if !reader.optional(204)? || !reader.boolean()? {
        return Err(Error::Unsupported("legacy named UNBOUND type".into()));
    }
    let name = type_expression(reader)?;
    reader.end()?;
    match name.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => Ok(DataType::Boolean),
        "tinyint" | "int1" => Ok(DataType::TinyInt),
        "smallint" | "int2" | "short" => Ok(DataType::SmallInt),
        "integer" | "int" | "int4" | "signed" => Ok(DataType::Integer),
        "bigint" | "int8" | "long" => Ok(DataType::BigInt),
        "hugeint" | "int128" => Ok(DataType::HugeInt),
        "utinyint" => Ok(DataType::UTinyInt),
        "usmallint" => Ok(DataType::USmallInt),
        "uinteger" => Ok(DataType::UInteger),
        "ubigint" => Ok(DataType::UBigInt),
        "uhugeint" => Ok(DataType::UHugeInt),
        "float" | "real" | "float4" => Ok(DataType::Float),
        "double" | "float8" => Ok(DataType::Double),
        "varchar" | "text" | "string" => Ok(DataType::Varchar),
        "blob" | "bytea" | "binary" | "varbinary" => Ok(DataType::Blob),
        "bit" | "bitstring" => Ok(DataType::Bit),
        "uuid" | "guid" => Ok(DataType::Uuid),
        "date" => Ok(DataType::Date),
        "time" => Ok(DataType::Time),
        "time_ns" => Ok(DataType::TimeNs),
        "timetz" | "time with time zone" => Ok(DataType::TimeTz),
        "timestamp" | "datetime" => Ok(DataType::Timestamp),
        "timestamp_s" | "timestamp_sec" => Ok(DataType::TimestampS),
        "timestamp_ms" => Ok(DataType::TimestampMs),
        "timestamp_ns" => Ok(DataType::TimestampNs),
        "timestamptz" | "timestamp with time zone" => Ok(DataType::TimestampTz),
        "timestamptz_ns" => Ok(DataType::TimestampTzNs),
        "interval" => Ok(DataType::Interval),
        _ => Err(Error::Unsupported(format!("unresolved native type {name}"))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn type_expression(reader: &mut Reader) -> Result<String> {
    reader.field(100)?;
    let class = reader.unsigned()?;
    reader.field(101)?;
    if class != 21 || reader.unsigned()? != 207 {
        return Err(Error::Unsupported(
            "non-type expression in UNBOUND metadata".into(),
        ));
    }
    if reader.optional(102)? && !reader.string()?.is_empty() {
        return Err(Error::Unsupported("aliased native type expression".into()));
    }
    if reader.optional(103)? {
        reader.unsigned()?;
    }
    if reader.optional(104)? {
        u32::try_from(reader.unsigned()?).map_err(|_| corrupt("type source span overflow"))?;
    }
    for field in [200, 201] {
        if reader.optional(field)? && !reader.string()?.is_empty() {
            return Err(Error::Unsupported("qualified native named type".into()));
        }
    }
    let mut name = if reader.optional(202)? {
        reader.string()?
    } else {
        String::new()
    };
    if reader.optional(203)? && reader.length()? != 0 {
        return Err(Error::Unsupported(
            "parameterized native type expression".into(),
        ));
    }
    if reader.optional(204)? {
        reader.field(100)?;
        let count = reader.length()?;
        if !(1..=3).contains(&count) {
            return Err(corrupt("invalid native type name path"));
        }
        for index in 0..count {
            let part = reader.string()?;
            if index + 1 == count {
                if !name.is_empty() && !name.eq_ignore_ascii_case(&part) {
                    return Err(corrupt("native type name fields disagree"));
                }
                name = part;
            } else if !part.is_empty() {
                return Err(Error::Unsupported("qualified native named type".into()));
            }
        }
        reader.end()?;
    }
    reader.end()?;
    if name.is_empty() {
        return Err(corrupt("native type expression has no name"));
    }
    Ok(name)
}
