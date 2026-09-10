//! Recursive native WAL vectors. Offsets belong to the serialized child vector;
//! dictionary slices can overlap and are not required to be contiguous.
use super::super::{
    binary::{Encoder, Reader, corrupt},
    nested::{child_values, list_child, physical_fields},
};
use crate::{
    common::{DataType, Error, NestedPayload, NestedType, NestedValue, Result, Value},
    parallel::QueryContext,
};

pub(super) const MAX_CELLS: usize = 16_777_216;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn charge(remaining: &mut usize, count: usize, context: &QueryContext) -> Result<()> {
    context.check_rows(count)?;
    *remaining = remaining
        .checked_sub(count)
        .ok_or_else(|| Error::Resource("WAL nested vectors exceed 16 million cells".into()))?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write(
    e: &mut Encoder,
    metadata: &NestedType,
    values: &[Value],
    depth: usize,
    remaining: &mut usize,
    context: &QueryContext,
) -> Result<()> {
    let children = child_values(metadata, values)?;
    match metadata {
        NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_) => {
            e.property(103, children.len() as u64);
            for (ty, values) in children {
                super::writer::vector(e, &ty, &values, depth + 1, remaining, context)?;
                e.end();
            }
        }
        NestedType::List(_) | NestedType::Map { .. } => {
            let (ty, children) = &children[0];
            e.property(104, children.len() as u64);
            e.property(105, values.len() as u64);
            let mut offset = 0usize;
            for value in values {
                context.check()?;
                let length = match value {
                    Value::Null => 0,
                    Value::Nested(value) => match &value.payload {
                        NestedPayload::Sequence(values) => values.len(),
                        NestedPayload::Map(entries) => entries.len(),
                        _ => return Err(Error::Internal("WAL LIST payload".into())),
                    },
                    _ => return Err(Error::Internal("WAL LIST value".into())),
                };
                e.property(100, if value.is_null() { 0 } else { offset as u64 });
                e.property(101, length as u64);
                e.end();
                offset = offset
                    .checked_add(length)
                    .ok_or_else(|| Error::Resource("WAL LIST offset overflow".into()))?;
            }
            e.field(106);
            super::writer::vector(e, ty, children, depth + 1, remaining, context)?;
            e.end();
        }
        NestedType::Array { length, .. } => {
            e.property(103, *length as u64);
            e.field(104);
            super::writer::vector(
                e,
                &children[0].0,
                &children[0].1,
                depth + 1,
                remaining,
                context,
            )?;
            e.end();
        }
        NestedType::Variant => return Err(Error::Unsupported("native VARIANT WAL vector".into())),
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(
    reader: &mut Reader,
    data_type: &DataType,
    count: usize,
    validity: Option<&[u8]>,
    depth: usize,
    remaining: &mut usize,
    context: &QueryContext,
) -> Result<Vec<Value>> {
    let DataType::Nested(metadata) = data_type else {
        return Err(Error::Internal("WAL nested metadata".into()));
    };
    let valid = |i: usize| validity.is_none_or(|mask| mask[i / 8] & (1 << (i % 8)) != 0);
    let mut output = Vec::with_capacity(count);
    match metadata.as_ref() {
        NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_) => {
            let fields = physical_fields(metadata)?;
            reader.field(103)?;
            if reader.length()? != fields.len() {
                return Err(corrupt("WAL STRUCT child count mismatch"));
            }
            let mut columns = Vec::with_capacity(fields.len());
            for (_, ty) in fields {
                columns.push(super::chunk::vector(
                    reader,
                    &ty,
                    count,
                    depth + 1,
                    remaining,
                    context,
                )?);
                reader.end()?;
            }
            for i in 0..count {
                context.check()?;
                if !valid(i) {
                    output.push(Value::Null);
                    continue;
                }
                let payload = if matches!(metadata.as_ref(), NestedType::Union(_)) {
                    let Value::Unsigned(tag) = columns[0][i] else {
                        return Err(corrupt("WAL UNION invalid NULL tag"));
                    };
                    let tag =
                        usize::try_from(tag).map_err(|_| corrupt("WAL UNION tag overflow"))?;
                    let value = columns
                        .get(tag + 1)
                        .ok_or_else(|| corrupt("WAL UNION tag out of range"))?[i]
                        .clone();
                    if columns
                        .iter()
                        .enumerate()
                        .skip(1)
                        .any(|(index, column)| index != tag + 1 && !column[i].is_null())
                    {
                        return Err(corrupt("WAL UNION inactive member is non-NULL"));
                    }
                    NestedPayload::Union { tag, value }
                } else {
                    NestedPayload::Struct(columns.iter().map(|column| column[i].clone()).collect())
                };
                output.push(
                    NestedValue::value(data_type.clone(), payload)
                        .map_err(super::recovery_error)?,
                );
            }
        }
        NestedType::List(_) | NestedType::Map { .. } | NestedType::Array { .. } => {
            let (total, entries) = if let NestedType::Array { length, .. } = metadata.as_ref() {
                reader.field(103)?;
                if reader.length()? != *length {
                    return Err(corrupt("WAL ARRAY size does not match type"));
                }
                let total = count
                    .checked_mul(*length)
                    .ok_or_else(|| corrupt("WAL ARRAY size overflow"))?;
                reader.field(104)?;
                (
                    total,
                    (0..count)
                        .map(|i| (i * length, *length))
                        .collect::<Vec<_>>(),
                )
            } else {
                reader.field(104)?;
                let total = reader.length()?;
                reader.field(105)?;
                if reader.length()? != count {
                    return Err(corrupt("WAL LIST entry count mismatch"));
                }
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    context.check()?;
                    reader.field(100)?;
                    let offset = reader.length()?;
                    reader.field(101)?;
                    let length = reader.length()?;
                    if offset.checked_add(length).is_none_or(|end| end > total) {
                        return Err(corrupt("WAL LIST entry outside child vector"));
                    }
                    entries.push((offset, length));
                    reader.end()?;
                }
                reader.field(106)?;
                (total, entries)
            };
            let children = super::chunk::vector(
                reader,
                &list_child(metadata)?,
                total,
                depth + 1,
                remaining,
                context,
            )?;
            reader.end()?;
            for (i, (offset, length)) in entries.into_iter().enumerate() {
                context.check()?;
                if !valid(i) {
                    output.push(Value::Null);
                    continue;
                }
                // Overlapping slices can amplify a short serialized child
                // into many owned row payloads; charge those copies as well.
                charge(remaining, length, context)?;
                let values = children
                    .get(offset..offset + length)
                    .ok_or_else(|| corrupt("WAL nested entry outside child vector"))?;
                let payload = if matches!(metadata.as_ref(), NestedType::Map { .. }) {
                    let entries = values
                        .iter()
                        .map(|value| {
                            let Value::Nested(value) = value else {
                                return Err(corrupt("WAL MAP entry is NULL"));
                            };
                            let NestedPayload::Struct(fields) = &value.payload else {
                                return Err(corrupt("WAL MAP entry shape"));
                            };
                            if fields.len() != 2 {
                                return Err(corrupt("WAL MAP entry arity"));
                            }
                            Ok((fields[0].clone(), fields[1].clone()))
                        })
                        .collect::<Result<_>>()?;
                    NestedPayload::Map(entries)
                } else {
                    NestedPayload::Sequence(values.to_vec())
                };
                output.push(
                    NestedValue::value(data_type.clone(), payload)
                        .map_err(super::recovery_error)?,
                );
            }
        }
        NestedType::Variant => return Err(Error::Unsupported("native VARIANT WAL vector".into())),
    }
    Ok(output)
}
