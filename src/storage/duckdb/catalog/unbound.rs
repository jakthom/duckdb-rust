//! Development default CASTs can retain a parsed type instead of a resolved
//! logical id. Decode known parameterless built-ins and literal timestamp
//! precision only; never execute stored expressions or erase unknown metadata.
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
    let (name, precision) = type_expression(reader)?;
    reader.end()?;
    if let Some(precision) = precision {
        return match (name.to_ascii_lowercase().as_str(), precision) {
            ("timestamp" | "datetime", 0) => Ok(DataType::TimestampS),
            ("timestamp" | "datetime", 1..=3) => Ok(DataType::TimestampMs),
            ("timestamp" | "datetime", 4..=6) => Ok(DataType::Timestamp),
            ("timestamp" | "datetime", 7..=9) => Ok(DataType::TimestampNs),
            _ => Err(Error::Unsupported(
                "parameterized native type expression".into(),
            )),
        };
    }
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
        "bignum" | "varint" => Ok(DataType::Bignum),
        "uuid" | "guid" => Ok(DataType::Uuid),
        "date" => Ok(DataType::Date),
        "time" => Ok(DataType::Time),
        "time_ns" => Ok(DataType::TimeNs),
        "timetz" | "time with time zone" => Ok(DataType::TimeTz),
        "timestamp" | "datetime" | "timestamp_us" => Ok(DataType::Timestamp),
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
fn type_expression(reader: &mut Reader) -> Result<(String, Option<u8>)> {
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
    let child_count = if reader.optional(203)? {
        reader.length()?
    } else {
        0
    };
    let precision = match child_count {
        0 => None,
        1 if matches!(name.to_ascii_lowercase().as_str(), "timestamp" | "datetime") => {
            if !reader.boolean()? {
                return Err(corrupt("missing native temporal precision expression"));
            }
            Some(precision_literal(reader)?)
        }
        _ => {
            return Err(Error::Unsupported(
                "parameterized native type expression".into(),
            ));
        }
    };
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
    Ok((name, precision))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn precision_literal(reader: &mut Reader) -> Result<u8> {
    reader.field(100)?;
    let class = reader.unsigned()?;
    reader.field(101)?;
    if class != 7 || reader.unsigned()? != 75 {
        return Err(Error::Unsupported(
            "non-literal native temporal precision".into(),
        ));
    }
    if reader.optional(102)? && !reader.string()?.is_empty() {
        return Err(Error::Unsupported(
            "aliased native temporal precision".into(),
        ));
    }
    if reader.optional(103)? {
        reader.unsigned()?;
    }
    if reader.optional(104)? {
        u32::try_from(reader.unsigned()?).map_err(|_| corrupt("precision source span overflow"))?;
    }
    reader.field(200)?;
    reader.field(100)?; // Literal value's logical type.
    reader.field(100)?; // Logical type id; no recursive type interpretation.
    let kind = reader.unsigned()?;
    if !matches!(kind, 11..=14 | 28..=31 | 49 | 50) {
        return Err(Error::Unsupported(
            "non-integral native temporal precision".into(),
        ));
    }
    reader.end()?;
    reader.field(101)?;
    if reader.boolean()? {
        return Err(corrupt("native temporal precision cannot be NULL"));
    }
    reader.field(102)?;
    let value = match kind {
        11..=14 => u64::try_from(reader.signed()?).ok(),
        28..=31 => Some(reader.unsigned()?),
        49 | 50 => {
            let upper_is_zero = if kind == 49 {
                reader.unsigned()? == 0
            } else {
                reader.signed()? == 0
            };
            let lower = reader.unsigned()?;
            upper_is_zero.then_some(lower)
        }
        _ => unreachable!("checked integral logical id"),
    };
    reader.end()?;
    reader.end()?;
    match value {
        Some(value) if value <= 9 => Ok(value as u8),
        _ => Err(corrupt(
            "native temporal precision must be an integer in 0..=9",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::duckdb::binary::Encoder;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn metadata(
        name: &str,
        precision: Option<i64>,
        children: u64,
        literal: bool,
    ) -> Result<Vec<u8>> {
        let mut out = Encoder::default();
        out.field(101);
        out.boolean(true);
        out.property(100, 7);
        out.field(204);
        out.boolean(true);
        out.property(100, 21);
        out.property(101, 207);
        out.field(202);
        out.string(name)?;
        out.property(203, children);
        if children == 1 {
            out.boolean(true);
            out.property(100, if literal { 7 } else { 3 });
            out.property(101, if literal { 75 } else { 12 });
            out.field(200);
            out.field(100);
            out.property(100, 13); // INTEGER logical type
            out.end();
            out.field(101);
            out.boolean(precision.is_none());
            if let Some(precision) = precision {
                out.field(102);
                out.signed(precision);
            }
            out.end();
            out.end();
        }
        out.field(204);
        out.property(100, 3);
        out.string("")?;
        out.string("")?;
        out.string(name)?;
        out.end();
        out.end();
        out.end();
        Ok(out.0)
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn temporal_alias_metadata_decodes_only_literal_valid_precision() -> Result<()> {
        for name in ["timestamp", "DATETIME"] {
            for (precision, expected) in [
                (0, DataType::TimestampS),
                (1, DataType::TimestampMs),
                (3, DataType::TimestampMs),
                (4, DataType::Timestamp),
                (6, DataType::Timestamp),
                (7, DataType::TimestampNs),
                (9, DataType::TimestampNs),
            ] {
                let mut reader = Reader::new(metadata(name, Some(precision), 1, true)?);
                assert_eq!(read(&mut reader)?, expected);
                assert!(reader.finished());
            }
        }
        let mut reader = Reader::new(metadata("TIMESTAMP_US", None, 0, true)?);
        assert_eq!(read(&mut reader)?, DataType::Timestamp);
        assert!(reader.finished());
        for precision in [None, Some(-1), Some(10), Some(256)] {
            assert!(matches!(
                read(&mut Reader::new(metadata("datetime", precision, 1, true)?)),
                Err(Error::Corrupt(_))
            ));
        }
        for (name, children, literal) in [
            ("application_type", 0, true),
            ("TIMESTAMP_US", 1, true),
            ("varchar", 1, true),
            ("datetime", 2, true),
            ("datetime", 1, false),
        ] {
            assert!(matches!(
                read(&mut Reader::new(metadata(
                    name,
                    Some(3),
                    children,
                    literal
                )?)),
                Err(Error::Unsupported(_))
            ));
        }
        let valid = metadata("datetime", Some(3), 1, true)?;
        for length in 0..valid.len() {
            assert!(read(&mut Reader::new(valid[..length].to_vec())).is_err());
        }
        Ok(())
    }
}
