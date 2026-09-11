use super::super::writer::{
    Arena, column_bytes, segment_with_statistics, statistics, validity_bytes,
};
use super::*;
use crate::parallel::QueryContext;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn child_values(
    metadata: &NestedType,
    values: &[Value],
    context: &QueryContext,
) -> Result<Vec<(DataType, Vec<Value>)>> {
    context.check()?;
    if values.len() > 16_777_216 {
        return Err(Error::Resource(
            "nested column exceeds 16 million values".into(),
        ));
    }
    let mut children = match metadata {
        NestedType::List(_) | NestedType::Array { .. } | NestedType::Map { .. } => {
            vec![(list_child(metadata)?, Vec::new())]
        }
        NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_) => {
            physical_fields(metadata)?
                .into_iter()
                .map(|(_, ty)| (ty, Vec::with_capacity(values.len())))
                .collect()
        }
        NestedType::Variant => {
            return Ok(vec![(
                variant::unshredded_type(),
                variant::encode_rows(values, context)?,
            )]);
        }
        NestedType::Object(_) => {
            return Err(Error::Unsupported(
                "native internal OBJECT child streams".into(),
            ));
        }
    };
    let data_type = metadata.clone().data_type();
    for value in values {
        context.check()?;
        if !value.fits_type(&data_type) {
            return Err(Error::Conversion(
                "native nested column shape mismatch".into(),
            ));
        }
        let additional = match (metadata, value) {
            (NestedType::Array { length, .. }, _) => *length,
            (NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_), _) => 1,
            (_, Value::Nested(value)) => match &value.payload {
                NestedPayload::Sequence(values) => values.len(),
                NestedPayload::Map(values) => values.len(),
                _ => 0,
            },
            _ => 0,
        };
        for (_, column) in &mut children {
            if column
                .len()
                .checked_add(additional)
                .is_none_or(|count| count > 16_777_216)
            {
                return Err(Error::Resource(
                    "nested child column exceeds 16 million values".into(),
                ));
            }
            column
                .try_reserve(additional)
                .map_err(|_| Error::Resource("nested child allocation failed".into()))?;
        }
        if value.is_null() {
            match metadata {
                NestedType::Array { length, .. } => {
                    let column = &mut children[0].1;
                    let count = column
                        .len()
                        .checked_add(*length)
                        .ok_or_else(|| Error::Resource("ARRAY child count overflow".into()))?;
                    column.resize(count, Value::Null);
                }
                NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_) => {
                    for (_, values) in &mut children {
                        values.push(Value::Null);
                    }
                }
                _ => {}
            }
            continue;
        }
        let Value::Nested(value) = value else {
            return Err(Error::Internal("native nested payload".into()));
        };
        match &value.payload {
            NestedPayload::Sequence(values) => children[0].1.extend(values.iter().cloned()),
            NestedPayload::Struct(values) => {
                for ((_, column), value) in children.iter_mut().zip(values) {
                    column.push(value.clone());
                }
            }
            NestedPayload::Map(entries) => {
                for (key, value) in entries {
                    let entry = NestedValue::value(
                        children[0].0.clone(),
                        NestedPayload::Struct(vec![key.clone(), value.clone()]),
                    )?;
                    children[0].1.push(entry);
                }
            }
            NestedPayload::Union { tag, value } => {
                children[0].1.push(Value::Unsigned(*tag as u128));
                for (index, (_, column)) in children.iter_mut().enumerate().skip(1) {
                    column.push(if index == tag + 1 {
                        value.clone()
                    } else {
                        Value::Null
                    });
                }
            }
            NestedPayload::Variant { .. } => {
                return Err(Error::Unsupported("native VARIANT child streams".into()));
            }
        }
    }
    Ok(children)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn write_statistics(
    output: &mut Encoder,
    metadata: &NestedType,
    values: &[Value],
    context: &QueryContext,
) -> Result<()> {
    let children = child_values(metadata, values, context)?;
    if matches!(metadata, NestedType::Variant) {
        output.property(200, 1); // VariantStatsShreddingState::NOT_SHREDDED
        output.field(225);
        return statistics(output, Some(&children[0].0), &children[0].1, context);
    }
    output.field(200);
    if matches!(
        metadata,
        NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_)
    ) {
        output.unsigned(children.len() as u64);
    }
    for (ty, values) in children {
        statistics(output, Some(&ty), &values, context)?;
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn write_column(
    arena: &mut Arena,
    data_type: &DataType,
    values: &[Value],
    row_start: usize,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    let DataType::Nested(metadata) = data_type else {
        return Err(Error::Internal("native nested column type".into()));
    };
    let children = child_values(metadata, values, context)?;
    let variable = matches!(
        metadata.as_ref(),
        NestedType::List(_) | NestedType::Map { .. }
    );
    let mut output = Encoder::default();
    if variable {
        let mut offset = 0usize;
        let offsets = values
            .iter()
            .map(|value| {
                let length = match value {
                    Value::Null => 0,
                    Value::Nested(value) => match &value.payload {
                        NestedPayload::Sequence(values) => values.len(),
                        NestedPayload::Map(values) => values.len(),
                        _ => return Err(Error::Internal("LIST offsets payload".into())),
                    },
                    _ => return Err(Error::Internal("LIST offsets value".into())),
                };
                offset = offset
                    .checked_add(length)
                    .ok_or_else(|| Error::Resource("LIST child count overflow".into()))?;
                Ok(Value::Unsigned(offset as u128))
            })
            .collect::<Result<Vec<_>>>()?;
        output.property(100, values.len().div_ceil(2048) as u64);
        for (chunk, offsets) in offsets.chunks(2048).enumerate() {
            let start = chunk * 2048;
            output.0.extend(segment_with_statistics(
                arena,
                &DataType::UBigInt,
                offsets,
                row_start + start,
                data_type,
                &values[start..start + offsets.len()],
                context,
            )?);
        }
    } else {
        output.property(100, 0);
    }
    output.field(101);
    output
        .0
        .extend(validity_bytes(arena, values, row_start, context)?);
    output.field(102);
    let structure = matches!(
        metadata.as_ref(),
        NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Union(_)
    );
    if structure {
        output.unsigned(children.len() as u64);
    }
    for (ty, values) in children {
        output
            .0
            .extend(column_bytes(arena, &ty, &values, row_start, context)?);
    }
    output.end();
    Ok(output.0)
}
