//! Resolve serialized `LogicalTypeId::UNBOUND` metadata into the concrete type
//! represented by its retained `TypeExpression`. The wire-only UNBOUND wrapper
//! never escapes this module as a table or expression `DataType`.
use super::{DataType, Error, Reader, Result, corrupt};
use crate::common::NestedType;

const MAX_TYPE_NODES: usize = 4096;
const MAX_EXPRESSION_NODES: usize = 16_384;
const MAX_IDENTIFIER_BYTES: usize = 16 * 1024 * 1024;

/// The Value codec supplies its existing cancellation and materialization
/// budget here. Catalog-column decoding uses the same finite shape limits even
/// though its older boundary has no query context.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) trait Limits {
    fn visit(&mut self, depth: usize) -> Result<()>;
    fn count(&mut self, count: usize) -> Result<()>;
    fn string(&mut self, reader: &mut Reader) -> Result<String>;
}

struct CatalogLimits {
    nodes: usize,
    bytes: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CatalogLimits {
    fn new() -> Self {
        Self {
            nodes: MAX_EXPRESSION_NODES,
            bytes: MAX_IDENTIFIER_BYTES,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Limits for CatalogLimits {
    fn visit(&mut self, depth: usize) -> Result<()> {
        if depth > 64 {
            return Err(Error::Resource(
                "native type expression nesting exceeds 64".into(),
            ));
        }
        self.nodes = self
            .nodes
            .checked_sub(1)
            .ok_or_else(|| Error::Resource("native type expression node limit".into()))?;
        Ok(())
    }

    fn count(&mut self, count: usize) -> Result<()> {
        if count > self.nodes {
            return Err(Error::Resource(
                "native type expression child count exceeds remaining nodes".into(),
            ));
        }
        Ok(())
    }

    fn string(&mut self, reader: &mut Reader) -> Result<String> {
        let count = reader.length()?;
        self.bytes = self
            .bytes
            .checked_sub(count)
            .ok_or_else(|| Error::Resource("native type identifier byte limit".into()))?;
        String::from_utf8(reader.bytes(count)?.to_vec())
            .map_err(|_| corrupt("invalid native type identifier UTF-8"))
    }
}

#[derive(Debug)]
enum Argument {
    Type {
        alias: Option<String>,
        data_type: DataType,
    },
    Integer {
        alias: Option<String>,
        value: i128,
    },
    Text {
        alias: Option<String>,
        value: String,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(reader: &mut Reader) -> Result<DataType> {
    let mut limits = CatalogLimits::new();
    let mut type_nodes = MAX_TYPE_NODES - 1; // The enclosing logical id is the root.
    read_with_limits(reader, 0, &mut type_nodes, &mut limits)
}

/// Decode an UNBOUND logical type embedded in native Value metadata. The
/// caller has already charged the root logical type node.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn read_with_limits(
    reader: &mut Reader,
    depth: usize,
    type_nodes: &mut usize,
    limits: &mut impl Limits,
) -> Result<DataType> {
    limits.visit(depth)?;
    reader.field(101)?;
    if !reader.boolean()? {
        return Err(corrupt("UNBOUND type without metadata"));
    }
    reader.field(100)?;
    if reader.unsigned()? != 7 {
        return Err(corrupt("invalid UNBOUND type info"));
    }
    if reader.optional(101)? && !limits.string(reader)?.is_empty() {
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
    let (alias, argument) = expression(reader, depth + 1, type_nodes, limits, false)?;
    reader.end()?;
    if alias.is_some() {
        return Err(Error::Unsupported("aliased native type expression".into()));
    }
    let Argument::Type { data_type, .. } = argument else {
        return Err(Error::Unsupported(
            "non-type expression in UNBOUND metadata".into(),
        ));
    };
    Ok(data_type)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn expression(
    reader: &mut Reader,
    depth: usize,
    type_nodes: &mut usize,
    limits: &mut impl Limits,
    charge_type: bool,
) -> Result<(Option<String>, Argument)> {
    limits.visit(depth)?;
    reader.field(100)?;
    let class = reader.unsigned()?;
    reader.field(101)?;
    let kind = reader.unsigned()?;
    let alias = optional_name(reader, 102, limits)?;
    if reader.optional(103)? {
        reader.unsigned()?;
    }
    if reader.optional(104)? {
        u32::try_from(reader.unsigned()?)
            .map_err(|_| corrupt("type expression source span overflow"))?;
    }
    let argument = match (class, kind) {
        (21, 207) => {
            if charge_type {
                *type_nodes = type_nodes.checked_sub(1).ok_or_else(|| {
                    Error::Resource("native literal type exceeds 4096 nodes".into())
                })?;
            }
            Argument::Type {
                alias: alias.clone(),
                data_type: type_expression(reader, depth, type_nodes, limits)?,
            }
        }
        (7, 75) => literal(reader, depth, alias.clone(), limits)?,
        _ => {
            return Err(Error::Unsupported(
                "non-type, non-literal native type parameter".into(),
            ));
        }
    };
    reader.end()?;
    Ok((alias, argument))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn type_expression(
    reader: &mut Reader,
    depth: usize,
    type_nodes: &mut usize,
    limits: &mut impl Limits,
) -> Result<DataType> {
    let catalog = optional_name(reader, 200, limits)?;
    let schema = optional_name(reader, 201, limits)?;
    let mut name = optional_name(reader, 202, limits)?.unwrap_or_default();
    let child_count = if reader.optional(203)? {
        reader.length()?
    } else {
        0
    };
    limits.count(child_count)?;
    let mut arguments = reserve(child_count)?;
    for _ in 0..child_count {
        if !reader.boolean()? {
            return Err(corrupt("NULL native type parameter"));
        }
        arguments.push(expression(reader, depth + 1, type_nodes, limits, true)?.1);
    }
    let mut path_qualified = false;
    if reader.optional(204)? {
        reader.field(100)?;
        let count = reader.length()?;
        limits.count(count)?;
        if count == 0 {
            return Err(corrupt("empty native type name path"));
        }
        for index in 0..count {
            let part = limits.string(reader)?;
            if index + 1 == count {
                if !name.is_empty() && !name.eq_ignore_ascii_case(&part) {
                    return Err(corrupt("native type name fields disagree"));
                }
                name = part;
            } else if !part.is_empty() {
                path_qualified = true;
            }
        }
        reader.end()?;
    }
    if name.is_empty() {
        return Err(corrupt("native type expression has no name"));
    }
    if catalog.is_some() || schema.is_some() || path_qualified {
        return Err(Error::Unsupported("qualified native named type".into()));
    }
    bind(&name, arguments)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn literal(
    reader: &mut Reader,
    depth: usize,
    alias: Option<String>,
    limits: &mut impl Limits,
) -> Result<Argument> {
    reader.field(200)?;
    limits.visit(depth + 1)?;
    reader.field(100)?;
    reader.field(100)?;
    let data_type = reader.unsigned()?;
    reader.end()?;
    reader.field(101)?;
    if reader.boolean()? {
        return Err(corrupt("NULL native type parameter"));
    }
    reader.field(102)?;
    let argument = match data_type {
        11..=14 | 50 => {
            let high = i128::from(reader.signed()?);
            let value = if data_type == 50 {
                (high << 64) | i128::from(reader.unsigned()?)
            } else {
                high
            };
            Argument::Integer { alias, value }
        }
        28..=31 | 49 => {
            let value = if data_type == 49 {
                let high = u128::from(reader.unsigned()?);
                let low = u128::from(reader.unsigned()?);
                i128::try_from((high << 64) | low).map_err(|_| {
                    Error::Unsupported("unsigned native type parameter exceeds HUGEINT".into())
                })?
            } else {
                let low = u128::from(reader.unsigned()?);
                i128::try_from(low).map_err(|_| {
                    Error::Unsupported("unsigned native type parameter exceeds HUGEINT".into())
                })?
            };
            Argument::Integer { alias, value }
        }
        25 => Argument::Text {
            alias,
            value: limits.string(reader)?,
        },
        _ => {
            return Err(Error::Unsupported(
                "non-integral, non-text native type parameter".into(),
            ));
        }
    };
    reader.end()?;
    Ok(argument)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bind(name: &str, arguments: Vec<Argument>) -> Result<DataType> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "list" => match arguments.as_slice() {
            [
                Argument::Type {
                    alias: None,
                    data_type,
                },
            ] => Ok(NestedType::List(data_type.clone()).data_type()),
            _ => unsupported_parameters(name),
        },
        "array" => match arguments.as_slice() {
            [
                Argument::Type {
                    alias: None,
                    data_type,
                },
                Argument::Integer { alias: None, value },
            ] => Ok(NestedType::Array {
                element: data_type.clone(),
                length: usize::try_from(*value)
                    .map_err(|_| corrupt("native ARRAY length overflow"))?,
            }
            .data_type()),
            _ => unsupported_parameters(name),
        },
        "map" => match arguments.as_slice() {
            [
                Argument::Type {
                    alias: None,
                    data_type: key,
                },
                Argument::Type {
                    alias: None,
                    data_type: value,
                },
            ] => Ok(NestedType::Map {
                key: key.clone(),
                value: value.clone(),
            }
            .data_type()),
            _ => unsupported_parameters(name),
        },
        "tuple" => Ok(NestedType::Tuple(type_arguments(name, arguments)?).data_type()),
        "struct" | "union" => {
            let fields = named_type_arguments(name, arguments)?;
            Ok(if lower == "struct" {
                NestedType::Struct(fields)
            } else {
                NestedType::Union(fields)
            }
            .data_type())
        }
        "variant" if arguments.is_empty() => Ok(NestedType::Variant.data_type()),
        "decimal" | "dec" | "numeric" => decimal(name, &arguments),
        "enum" => enumeration(name, arguments),
        "timestamp" | "datetime" if arguments.len() == 1 => {
            let precision = integer_argument(name, &arguments[0])?;
            match precision {
                0 => Ok(DataType::TimestampS),
                1..=3 => Ok(DataType::TimestampMs),
                4..=6 => Ok(DataType::Timestamp),
                7..=9 => Ok(DataType::TimestampNs),
                _ => Err(corrupt(
                    "native temporal precision must be an integer in 0..=9",
                )),
            }
        }
        "varchar" | "char" | "bpchar" | "string" | "text" if arguments.len() <= 1 => {
            if let Some(argument) = arguments.first() {
                integer_argument(name, argument)?;
            }
            Ok(DataType::Varchar)
        }
        _ if !arguments.is_empty() => unsupported_parameters(name),
        "null" => Ok(DataType::Null),
        "bool" | "boolean" => Ok(DataType::Boolean),
        "tinyint" | "int1" => Ok(DataType::TinyInt),
        "smallint" | "int2" | "short" | "int16" => Ok(DataType::SmallInt),
        "integer" | "int" | "int4" | "signed" | "int32" => Ok(DataType::Integer),
        "bigint" | "int8" | "long" | "int64" => Ok(DataType::BigInt),
        "hugeint" | "int128" => Ok(DataType::HugeInt),
        "utinyint" | "uint8" => Ok(DataType::UTinyInt),
        "usmallint" | "uint16" => Ok(DataType::USmallInt),
        "uinteger" | "uint32" => Ok(DataType::UInteger),
        "ubigint" | "uint64" => Ok(DataType::UBigInt),
        "uhugeint" | "uint128" => Ok(DataType::UHugeInt),
        "float" | "real" | "float4" | "float32" => Ok(DataType::Float),
        "double" | "float8" | "float64" => Ok(DataType::Double),
        "varchar" | "text" | "string" | "char" | "bpchar" => Ok(DataType::Varchar),
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
fn type_arguments(name: &str, arguments: Vec<Argument>) -> Result<Vec<DataType>> {
    arguments
        .into_iter()
        .map(|argument| match argument {
            Argument::Type {
                alias: None,
                data_type,
            } => Ok(data_type),
            _ => unsupported_parameters(name),
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn named_type_arguments(name: &str, arguments: Vec<Argument>) -> Result<Vec<(String, DataType)>> {
    arguments
        .into_iter()
        .map(|argument| match argument {
            Argument::Type {
                alias: Some(alias),
                data_type,
            } if !alias.is_empty() => Ok((alias, data_type)),
            _ => unsupported_parameters(name),
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decimal(name: &str, arguments: &[Argument]) -> Result<DataType> {
    let (width, scale) = match arguments {
        [] => (18, 3),
        [width] => (integer_argument(name, width)?, 0),
        [width, scale] => (
            integer_argument(name, width)?,
            integer_argument(name, scale)?,
        ),
        _ => return unsupported_parameters(name),
    };
    Ok(DataType::Decimal {
        width: u8::try_from(width).map_err(|_| corrupt("native DECIMAL width overflow"))?,
        scale: u8::try_from(scale).map_err(|_| corrupt("native DECIMAL scale overflow"))?,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn enumeration(name: &str, arguments: Vec<Argument>) -> Result<DataType> {
    let labels = arguments
        .into_iter()
        .map(|argument| match argument {
            Argument::Text { alias: None, value } => Ok(value),
            _ => unsupported_parameters(name),
        })
        .collect::<Result<Vec<_>>>()?;
    if labels.is_empty() {
        return unsupported_parameters(name);
    }
    DataType::enumeration(labels).map_err(|error| match error {
        Error::Resource(_) => error,
        _ => corrupt("invalid native ENUM parameters"),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn integer_argument(name: &str, argument: &Argument) -> Result<i128> {
    match argument {
        Argument::Integer { alias: None, value } => Ok(*value),
        _ => unsupported_parameters(name),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn optional_name(
    reader: &mut Reader,
    field: u16,
    limits: &mut impl Limits,
) -> Result<Option<String>> {
    if !reader.optional(field)? {
        return Ok(None);
    }
    let name = limits.string(reader)?;
    Ok((!name.is_empty()).then_some(name))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn reserve<T>(count: usize) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate native type parameters".into()))?;
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unsupported_parameters<T>(name: &str) -> Result<T> {
    Err(Error::Unsupported(format!(
        "parameterized native type expression {name}"
    )))
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
        let mut output = Encoder::default();
        output.field(101);
        output.boolean(true);
        output.property(100, 7);
        output.field(204);
        output.boolean(true);
        output.property(100, 21);
        output.property(101, 207);
        output.field(202);
        output.string(name)?;
        output.property(203, children);
        if children == 1 {
            output.boolean(true);
            output.property(100, if literal { 7 } else { 3 });
            output.property(101, if literal { 75 } else { 12 });
            output.field(200);
            output.field(100);
            output.property(100, 13);
            output.end();
            output.field(101);
            output.boolean(precision.is_none());
            if let Some(precision) = precision {
                output.field(102);
                output.signed(precision);
            }
            output.end();
            output.end();
        }
        output.field(204);
        output.property(100, 3);
        output.string("")?;
        output.string("")?;
        output.string(name)?;
        output.end();
        output.end();
        output.end();
        Ok(output.0)
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
        let mut reader = Reader::new(metadata("VARCHAR", Some(42), 1, true)?);
        assert_eq!(read(&mut reader)?, DataType::Varchar);
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
            ("datetime", 1, false),
        ] {
            let result = read(&mut Reader::new(metadata(
                name,
                Some(3),
                children,
                literal,
            )?));
            assert!(
                matches!(result, Err(Error::Unsupported(_))),
                "{name}: {result:?}"
            );
        }
        assert!(matches!(
            read(&mut Reader::new(metadata("datetime", Some(3), 2, true)?)),
            Err(Error::Corrupt(_))
        ));
        let valid = metadata("datetime", Some(3), 1, true)?;
        for length in 0..valid.len() {
            assert!(read(&mut Reader::new(valid[..length].to_vec())).is_err());
        }
        Ok(())
    }
}
