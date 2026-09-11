use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn value(
    reader: &mut Reader,
    inherited: Option<&DataType>,
    depth: usize,
    state: &mut State<'_>,
) -> Result<(DataType, Value)> {
    state.visit(depth)?;
    let ty = if reader.optional(100)? {
        let ty = metadata::read(reader, depth, state)?;
        if inherited.is_some_and(|expected| *expected != ty) {
            return Err(corrupt(
                "native Value explicit child type differs from parent",
            ));
        }
        ty
    } else {
        inherited
            .cloned()
            .ok_or_else(|| corrupt("native root Value omits its type"))?
    };
    let selected = state.binding(&ty)?;
    reader.field(101)?;
    let result = if reader.boolean()? {
        Value::Null
    } else {
        reader.field(102)?;
        if let DataType::Nested(metadata) = &ty {
            nested(reader, &ty, metadata, depth, state)?
        } else {
            scalar(reader, &ty, state)?
        }
    };
    reader.end()?;
    // Local physical errors are corruption, but a selected adapter's fatal or
    // resource error is not a recoverable conversion failure.
    if !result.fits_type(&ty) {
        return Err(corrupt("native Value payload does not fit declared type"));
    }
    selected.validate(&result, state.query)?;
    Ok((ty, result))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn scalar(reader: &mut Reader, ty: &DataType, state: &mut State<'_>) -> Result<Value> {
    use crate::common::{BignumValue, BitString, TemporalValue};
    Ok(match ty {
        DataType::Null => return Err(corrupt("non-NULL SQLNULL literal")),
        DataType::Boolean => Value::Boolean(reader.boolean()?),
        DataType::Float => Value::Float(reader.float()?),
        DataType::Double => Value::Double(reader.double()?),
        DataType::Date => Value::Date(super::super::binary::date(reader.signed()?)?),
        // Native Value uses INT64 for TIMETZ; vector bytes are a different codec.
        DataType::TimeTz => Value::Temporal(
            TemporalValue::from_packed_time_tz(reader.signed()? as u64).map_err(wire_error)?,
        ),
        ty if ty.is_temporal() => {
            super::super::temporal::read_metadata(reader, ty).map_err(wire_error)?
        }
        DataType::Varchar => Value::Varchar(state.string(reader)?),
        DataType::Blob => {
            let text = state.string(reader)?;
            state.charge_bytes(text.len())?;
            Value::Blob(
                crate::common::scalar::parse_blob(&text, || state.query.check())
                    .map_err(wire_error)?,
            )
        }
        DataType::Bit => {
            let bytes = state.blob(reader)?;
            state.charge_bytes(bytes.len())?;
            BitString::from_native(&bytes, || state.query.check())
                .map_err(wire_error)?
                .value()
        }
        DataType::Bignum => {
            let bytes = state.blob(reader)?;
            state.charge_bytes(bytes.len())?;
            BignumValue::from_native(&bytes, || state.query.check())
                .map_err(wire_error)?
                .value()
        }
        DataType::Decimal { width, scale } => Value::Decimal {
            value: signed(reader, ty)?,
            width: *width,
            scale: *scale,
        },
        DataType::Uuid | DataType::Enum(_) => super::super::primitive::read_numeric(reader, ty)?,
        ty if ty.is_unsigned_integer() => super::super::primitive::read_numeric(reader, ty)?,
        ty if ty.is_signed_integer() => Value::Integer(signed(reader, ty)?),
        _ => return Err(Error::Unsupported(format!("native Value scalar {ty}"))),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn signed(reader: &mut Reader, ty: &DataType) -> Result<i128> {
    Ok(if super::super::primitive::width(ty)? == 16 {
        (i128::from(reader.signed()?) << 64) | i128::from(reader.unsigned()?)
    } else {
        i128::from(reader.signed()?)
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nested(
    reader: &mut Reader,
    ty: &DataType,
    metadata: &NestedType,
    depth: usize,
    state: &mut State<'_>,
) -> Result<Value> {
    reader.field(100)?;
    let count = reader.length()?;
    // Charge the entire collection before reserving it; recursion still charges
    // each child visit and its metadata, including repeated legacy type fields.
    state.charge_nodes(count)?;
    let mut children;
    let sequence = matches!(
        metadata,
        NestedType::List(_) | NestedType::Map { .. } | NestedType::Array { .. }
    );
    if sequence {
        if let NestedType::Array { length, .. } = metadata
            && count != *length
        {
            return Err(corrupt("native ARRAY literal child count"));
        }
        children = reserve(count)?;
        let child = super::super::nested::list_child(metadata)?;
        for _ in 0..count {
            children.push(value(reader, Some(&child), depth + 1, state)?.1);
        }
    } else {
        let fields = metadata::fields(metadata)?;
        if count != fields.len() {
            return Err(corrupt("native record literal child count"));
        }
        children = reserve(count)?;
        for (_, child) in fields {
            children.push(value(reader, Some(&child), depth + 1, state)?.1);
        }
    }
    reader.end()?;
    let payload = match metadata {
        NestedType::List(_) | NestedType::Array { .. } => NestedPayload::Sequence(children),
        NestedType::Struct(_) | NestedType::Tuple(_) => NestedPayload::Struct(children),
        NestedType::Map { .. } => {
            let mut entries = reserve(count)?;
            for child in children {
                state.query.check()?;
                let Value::Nested(child) = child else {
                    return Err(corrupt("native MAP literal entry is NULL"));
                };
                let NestedPayload::Struct(fields) = &child.payload else {
                    return Err(corrupt("native MAP literal entry is not STRUCT"));
                };
                let [key, value] = fields.as_slice() else {
                    return Err(corrupt("native MAP literal entry arity"));
                };
                if key.is_null() {
                    return Err(corrupt("native MAP literal NULL key"));
                }
                entries.push((key.clone(), value.clone()));
            }
            NestedPayload::Map(entries)
        }
        NestedType::Union(_) => {
            let Some(Value::Unsigned(tag)) = children.first() else {
                return Err(corrupt("native UNION literal NULL tag"));
            };
            let tag = usize::try_from(*tag).map_err(|_| corrupt("native UNION tag overflow"))?;
            let value = children
                .get(tag + 1)
                .ok_or_else(|| corrupt("native UNION tag out of range"))?
                .clone();
            for (index, child) in children.iter().skip(1).enumerate() {
                if index != tag && !child.is_null() {
                    return Err(corrupt("native UNION inactive member is non-NULL"));
                }
            }
            NestedPayload::Union { tag, value }
        }
        NestedType::Variant => {
            let physical = NestedValue::value(
                super::super::nested::variant::wal::data_type(),
                NestedPayload::Struct(children),
            )
            .map_err(wire_error)?;
            let selected = state.binding(ty)?;
            return super::super::nested::variant::value::decode(
                &physical,
                &selected,
                depth,
                &mut state.nodes,
                &mut state.bytes,
                state.query,
            );
        }
        NestedType::Object(_) => return Err(Error::Unsupported("native OBJECT literal".into())),
    };
    NestedValue::value(ty.clone(), payload).map_err(wire_error)
}
