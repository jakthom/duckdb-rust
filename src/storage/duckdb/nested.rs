//! Native nested metadata and child-stream layout. This module does not treat
//! matching scalar displays as evidence that file payloads are compatible.
use super::{
    binary::{Encoder, Reader, corrupt},
    catalog::logical_type_at,
    primitive::write_type,
};
use crate::common::{DataType, Error, NestedType, Result};
use crate::{
    common::{NestedPayload, NestedValue, Value},
    storage::compression::DecoderRegistry,
};
pub(super) mod variant;
mod writer;
pub(super) use writer::child_values;
pub(super) use writer::{write_column, write_statistics};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn physical_fields(metadata: &NestedType) -> Result<Vec<(String, DataType)>> {
    Ok(match metadata {
        NestedType::Struct(fields) => fields.clone(),
        NestedType::Tuple(fields) => fields
            .iter()
            .cloned()
            .map(|ty| (String::new(), ty))
            .collect(),
        NestedType::Union(fields) => std::iter::once((String::new(), DataType::UTinyInt))
            .chain(fields.iter().cloned())
            .collect(),
        _ => {
            return Err(Error::Internal(
                "nested fields require STRUCT or UNION".into(),
            ));
        }
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn list_child(metadata: &NestedType) -> Result<DataType> {
    Ok(match metadata {
        NestedType::List(child) | NestedType::Array { element: child, .. } => child.clone(),
        NestedType::Map { key, value } => NestedType::Struct(vec![
            ("key".into(), key.clone()),
            ("value".into(), value.clone()),
        ])
        .data_type(),
        _ => {
            return Err(Error::Internal(
                "nested child requires LIST, ARRAY or MAP".into(),
            ));
        }
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_statistics(reader: &mut Reader, metadata: &NestedType) -> Result<()> {
    if matches!(metadata, NestedType::Variant) {
        return variant::read_statistics(reader);
    }
    reader.field(200)?;
    match metadata {
        NestedType::List(_) | NestedType::Map { .. } | NestedType::Array { .. } => {
            super::columns::statistics(reader, Some(&list_child(metadata)?))?;
        }
        NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_) => {
            let fields = physical_fields(metadata)?;
            if reader.length()? != fields.len() {
                return Err(corrupt("nested statistics child count"));
            }
            for (_, ty) in fields {
                super::columns::statistics(reader, Some(&ty))?;
            }
        }
        NestedType::Variant => return Err(Error::Unsupported("native VARIANT statistics".into())),
        NestedType::Object(_) => {
            return Err(Error::Unsupported(
                "native internal OBJECT statistics".into(),
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_column(
    blocks: &super::Blocks,
    decoders: &DecoderRegistry,
    reader: &mut Reader,
    data_type: &DataType,
    count: usize,
    row_start: usize,
) -> Result<Vec<Value>> {
    let DataType::Nested(metadata) = data_type else {
        return Err(Error::Internal("nested column metadata".into()));
    };
    if matches!(metadata.as_ref(), NestedType::Variant) {
        return variant::read_column(blocks, decoders, reader, count, row_start);
    }
    let variable = matches!(
        metadata.as_ref(),
        NestedType::List(_) | NestedType::Map { .. }
    );
    let offsets = super::columns::read_segments(
        blocks,
        decoders,
        reader,
        Some(data_type),
        Some(&DataType::UBigInt),
        if variable { count } else { 0 },
        row_start,
    )?;
    if variable && offsets.len() != count {
        return Err(corrupt("LIST offset count mismatch"));
    }
    reader.field(101)?;
    let validity = super::columns::read_column(blocks, decoders, reader, None, count, row_start)?;
    if validity.iter().any(Value::is_null) {
        // Containers have no inline validity to preserve. Pinned development
        // selects EMPTY_VALIDITY only for DICT_FSST scalar base data.
        return Err(corrupt("nested validity cannot preserve absent base mask"));
    }
    reader.field(102)?;
    let mut output = Vec::with_capacity(count);
    match metadata.as_ref() {
        NestedType::List(_) | NestedType::Map { .. } | NestedType::Array { .. } => {
            let endpoints = if let NestedType::Array { length, .. } = metadata.as_ref() {
                (1..=count)
                    .map(|row| {
                        row.checked_mul(*length)
                            .ok_or_else(|| corrupt("ARRAY child count overflow"))
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                offsets
                    .iter()
                    .map(|value| match value {
                        Value::Unsigned(value) => usize::try_from(*value)
                            .map_err(|_| corrupt("LIST child offset overflow")),
                        _ => Err(corrupt("invalid LIST offset")),
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            let total = endpoints.last().copied().unwrap_or(0);
            if total > 16_777_216 {
                return Err(Error::Resource(
                    "nested child column exceeds 16 million values".into(),
                ));
            }
            let children = super::columns::read_column(
                blocks,
                decoders,
                reader,
                Some(&list_child(metadata)?),
                total,
                // C++ IncrementSegmentStart applies the enclosing row-group
                // origin to every descendant, even variable-sized children.
                row_start,
            )?;
            let mut start = 0;
            for (end, valid) in endpoints.into_iter().zip(validity) {
                let values = children.get(start..end).ok_or_else(|| {
                    corrupt("LIST offsets are decreasing or outside child column")
                })?;
                start = end;
                if valid != Value::Boolean(true) {
                    output.push(Value::Null);
                    continue;
                }
                let payload = if matches!(metadata.as_ref(), NestedType::Map { .. }) {
                    let entries = values
                        .iter()
                        .map(|value| {
                            let Value::Nested(value) = value else {
                                return Err(corrupt("MAP child entry is NULL or not STRUCT"));
                            };
                            let NestedPayload::Struct(fields) = &value.payload else {
                                return Err(corrupt("MAP child entry shape"));
                            };
                            if fields.len() != 2 {
                                return Err(corrupt("MAP child field count"));
                            }
                            Ok((fields[0].clone(), fields[1].clone()))
                        })
                        .collect::<Result<_>>()?;
                    NestedPayload::Map(entries)
                } else {
                    NestedPayload::Sequence(values.to_vec())
                };
                output.push(NestedValue::value(data_type.clone(), payload)?);
            }
        }
        NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_) => {
            let fields = physical_fields(metadata)?;
            if reader.length()? != fields.len() {
                return Err(corrupt("STRUCT column child count"));
            }
            let columns = fields
                .iter()
                .map(|(_, ty)| {
                    super::columns::read_column(
                        blocks,
                        decoders,
                        reader,
                        Some(ty),
                        count,
                        row_start,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            for (row, valid) in validity.into_iter().enumerate() {
                if valid != Value::Boolean(true) {
                    output.push(Value::Null);
                    continue;
                }
                let payload = if matches!(metadata.as_ref(), NestedType::Union(_)) {
                    let Value::Unsigned(tag) = columns[0][row] else {
                        return Err(corrupt("UNION tag is NULL or invalid"));
                    };
                    let tag = usize::try_from(tag).map_err(|_| corrupt("UNION tag overflow"))?;
                    let value = columns
                        .get(tag + 1)
                        .ok_or_else(|| corrupt("UNION tag out of range"))?[row]
                        .clone();
                    for (index, column) in columns.iter().enumerate().skip(1) {
                        if index != tag + 1 && !column[row].is_null() {
                            return Err(corrupt("inactive UNION member is non-NULL"));
                        }
                    }
                    NestedPayload::Union { tag, value }
                } else {
                    NestedPayload::Struct(
                        columns.iter().map(|column| column[row].clone()).collect(),
                    )
                };
                output.push(NestedValue::value(data_type.clone(), payload)?);
            }
        }
        NestedType::Variant => return Err(Error::Unsupported("native VARIANT column".into())),
        NestedType::Object(_) => {
            return Err(Error::Unsupported("native internal OBJECT column".into()));
        }
    }
    reader.end()?;
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn type_id(metadata: &NestedType) -> Result<u64> {
    Ok(match metadata {
        NestedType::Struct(_) => 100,
        NestedType::List(_) => 101,
        NestedType::Map { .. } => 102,
        NestedType::Union(_) => 107,
        NestedType::Array { .. } => 108,
        NestedType::Tuple(_) => 110,
        NestedType::Variant => 109,
        NestedType::Object(_) => {
            return Err(Error::Unsupported(
                "native internal OBJECT type layout".into(),
            ));
        }
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write_info(output: &mut Encoder, metadata: &NestedType) -> Result<()> {
    output.field(101);
    output.boolean(true);
    match metadata {
        NestedType::List(child) => {
            output.property(100, 4);
            output.field(200);
            write_type(output, child)?;
        }
        NestedType::Array { element, length } => {
            output.property(100, 9);
            output.field(200);
            write_type(output, element)?;
            output.property(201, *length as u64);
        }
        NestedType::Map { key, value } => {
            output.property(100, 4);
            output.field(200);
            write_type(
                output,
                &NestedType::Struct(vec![
                    ("key".into(), key.clone()),
                    ("value".into(), value.clone()),
                ])
                .data_type(),
            )?;
        }
        NestedType::Struct(fields) | NestedType::Union(fields) => {
            output.property(100, 5);
            let is_union = matches!(metadata, NestedType::Union(_));
            output.property(200, fields.len() as u64 + u64::from(is_union));
            if is_union {
                write_field(output, "", &DataType::UTinyInt)?;
            }
            for (name, ty) in fields {
                write_field(output, name, ty)?;
            }
        }
        NestedType::Tuple(fields) => {
            output.property(100, 5);
            output.property(200, fields.len() as u64);
            for ty in fields {
                write_field(output, "", ty)?;
            }
        }
        NestedType::Variant => {
            output.property(100, 5);
            let fields = variant::fields();
            output.property(200, fields.len() as u64);
            for (name, ty) in fields {
                write_field(output, &name, &ty)?;
            }
        }
        NestedType::Object(_) => {
            return Err(Error::Unsupported(
                "native internal OBJECT type layout".into(),
            ));
        }
    }
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn write_field(output: &mut Encoder, name: &str, ty: &DataType) -> Result<()> {
    output.field(0);
    output.string(name)?;
    output.field(1);
    write_type(output, ty)?;
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_type(reader: &mut Reader, id: u64, depth: usize) -> Result<DataType> {
    reader.field(101)?;
    if !reader.boolean()? {
        return Err(corrupt("missing nested type metadata"));
    }
    reader.field(100)?;
    let kind = reader.unsigned()?;
    let expected = match id {
        100 | 107 | 109 | 110 => 5,
        101 | 102 => 4,
        108 => 9,
        _ => return Err(corrupt("unknown nested type")),
    };
    if kind != expected {
        return Err(corrupt("nested metadata kind mismatch"));
    }
    if reader.optional(101)? && !reader.string()?.is_empty() {
        return Err(Error::Unsupported("aliased nested metadata".into()));
    }
    let metadata = match id {
        101 | 102 | 108 => {
            reader.field(200)?;
            let child = logical_type_at(reader, depth + 1)?;
            match id {
                101 => NestedType::List(child),
                108 => NestedType::Array {
                    element: child,
                    length: usize::try_from(reader.optional_unsigned(201, 0)?)
                        .map_err(|_| corrupt("ARRAY length overflow"))?,
                },
                _ => {
                    let DataType::Nested(child) = child else {
                        return Err(corrupt("MAP child is not STRUCT"));
                    };
                    let NestedType::Struct(fields) = child.as_ref() else {
                        return Err(corrupt("MAP child is not STRUCT"));
                    };
                    if fields.len() != 2 || fields[0].0 != "key" || fields[1].0 != "value" {
                        return Err(corrupt("MAP child fields mismatch"));
                    }
                    NestedType::Map {
                        key: fields[0].1.clone(),
                        value: fields[1].1.clone(),
                    }
                }
            }
        }
        100 | 107 | 109 | 110 => {
            let count = if reader.optional(200)? {
                reader.length()?
            } else {
                0
            };
            if count > 4096 {
                return Err(Error::Resource("nested type exceeds 4096 fields".into()));
            }
            let mut fields = Vec::with_capacity(count);
            for _ in 0..count {
                reader.field(0)?;
                let name = reader.string()?;
                reader.field(1)?;
                let ty = logical_type_at(reader, depth + 1)?;
                reader.end()?;
                fields.push((name, ty));
            }
            if id == 109 {
                if fields != variant::fields() {
                    return Err(corrupt("VARIANT canonical physical metadata mismatch"));
                }
                NestedType::Variant
            } else if id == 107 {
                if fields.first() != Some(&(String::new(), DataType::UTinyInt)) {
                    return Err(corrupt("UNION tag metadata mismatch"));
                }
                fields.remove(0);
                NestedType::Union(fields)
            } else if id == 110
                || !fields.is_empty() && fields.iter().all(|(name, _)| name.is_empty())
            {
                if fields.iter().any(|(name, _)| !name.is_empty()) {
                    return Err(corrupt("TUPLE fields must be unnamed"));
                }
                NestedType::Tuple(fields.into_iter().map(|(_, ty)| ty).collect())
            } else {
                NestedType::Struct(fields)
            }
        }
        _ => return Err(corrupt("unknown nested type")),
    };
    reader.end()?;
    let ty = metadata.data_type();
    crate::common::type_registry::check_metadata(&ty)
        .map_err(|_| corrupt("invalid nested metadata"))?;
    Ok(ty)
}
