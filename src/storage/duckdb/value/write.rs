use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn value(
    output: &mut Encoder,
    ty: &DataType,
    value: &Value,
    root: bool,
    depth: usize,
    state: &mut State<'_>,
) -> Result<()> {
    state.visit(depth)?;
    state.version_type(ty)?;
    header(output, ty, value.is_null(), root, depth, state)?;
    if !value.is_null() {
        output.field(102);
        if let DataType::Nested(metadata) = ty {
            nested(output, ty, metadata, value, depth, state)?;
        } else {
            scalar(output, ty, value, state)?;
        }
    }
    output.end();
    state.binding(ty)?.validate(value, state.query)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn header(
    output: &mut Encoder,
    ty: &DataType,
    null: bool,
    root: bool,
    depth: usize,
    state: &mut State<'_>,
) -> Result<()> {
    if root || state.version < 65 {
        output.field(100);
        metadata::write(output, ty, depth, state)?;
    }
    output.field(101);
    output.boolean(null);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn scalar(output: &mut Encoder, ty: &DataType, value: &Value, state: &mut State<'_>) -> Result<()> {
    if !value.fits_type(ty) {
        return Err(Error::Conversion(
            "native scalar literal differs from declared type".into(),
        ));
    }
    match value {
        Value::Boolean(value) => output.boolean(*value),
        Value::Date(value) => output.signed(i64::from(value.days())),
        Value::Temporal(value) if *ty == DataType::TimeTz => {
            output.signed(value.packed_time_tz()? as i64)
        }
        Value::Temporal(value) => super::super::temporal::write_metadata(output, *value)?,
        Value::Float(value) => output.0.extend(value.to_le_bytes()),
        Value::Double(value) => output.0.extend(value.to_le_bytes()),
        Value::Varchar(value) => state.write_blob(output, value.as_bytes())?,
        Value::Blob(value) => {
            // Source Blob::ToString uses four ASCII bytes for each escaped byte.
            let length = value.iter().try_fold(0_usize, |length, byte| {
                state.query.check()?;
                length
                    .checked_add(if regular_blob(*byte) { 1 } else { 4 })
                    .ok_or_else(|| Error::Resource("BLOB literal text length overflow".into()))
            })?;
            if length > MAX_NODES {
                return Err(Error::Resource(
                    "native BLOB literal exceeds 16 MiB text".into(),
                ));
            }
            state.charge_bytes(length)?;
            output.unsigned(length as u64);
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            for byte in value {
                state.query.check()?;
                if regular_blob(*byte) {
                    output.0.push(*byte);
                } else {
                    output.0.extend([
                        b'\\',
                        b'x',
                        HEX[(byte >> 4) as usize],
                        HEX[(byte & 15) as usize],
                    ]);
                }
            }
        }
        Value::Bit(value) => {
            state.charge_bytes(value.bytes().len().saturating_add(1))?;
            state.write_blob(output, &value.to_native(|| state.query.check())?)?;
        }
        Value::Bignum(value) => {
            state.charge_bytes(value.byte_len().saturating_add(3))?;
            state.write_blob(output, &value.to_native(|| state.query.check())?)?;
        }
        Value::Integer(_)
        | Value::Unsigned(_)
        | Value::Decimal { .. }
        | Value::Enum(_)
        | Value::Uuid(_) => super::super::primitive::write_numeric(output, value, ty)?,
        _ => return Err(Error::Unsupported(format!("native literal scalar {ty}"))),
    }
    state.query.check()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn regular_blob(byte: u8) -> bool {
    (32..=126).contains(&byte) && !matches!(byte, b'\\' | b'\'' | b'"')
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nested(
    output: &mut Encoder,
    ty: &DataType,
    metadata: &NestedType,
    value: &Value,
    depth: usize,
    state: &mut State<'_>,
) -> Result<()> {
    let Value::Nested(nested) = value else {
        return Err(Error::Conversion("native nested literal shape".into()));
    };
    if nested.data_type != *ty {
        return Err(Error::Conversion(
            "native nested literal metadata mismatch".into(),
        ));
    }
    output.field(100);
    match (metadata, &nested.payload) {
        (
            NestedType::List(child) | NestedType::Array { element: child, .. },
            NestedPayload::Sequence(children),
        ) => {
            if let NestedType::Array { length, .. } = metadata
                && children.len() != *length
            {
                return Err(Error::Conversion("native ARRAY literal child count".into()));
            }
            state.charge_nodes(children.len())?;
            output.unsigned(children.len() as u64);
            for child_value in children {
                self::value(output, child, child_value, false, depth + 1, state)?;
            }
        }
        (NestedType::Struct(_) | NestedType::Tuple(_), NestedPayload::Struct(children)) => {
            let fields = metadata::fields(metadata)?;
            if fields.len() != children.len() {
                return Err(Error::Conversion(
                    "native record literal child count".into(),
                ));
            }
            state.charge_nodes(children.len())?;
            output.unsigned(children.len() as u64);
            for ((_, child), child_value) in fields.iter().zip(children) {
                self::value(output, child, child_value, false, depth + 1, state)?;
            }
        }
        (
            NestedType::Map {
                key,
                value: value_type,
            },
            NestedPayload::Map(entries),
        ) => {
            state.charge_nodes(entries.len())?;
            let entry_type = super::super::nested::list_child(metadata)?;
            output.unsigned(entries.len() as u64);
            for (key_value, value_value) in entries {
                state.visit(depth + 1)?;
                if key_value.is_null() {
                    return Err(Error::Conversion("native MAP literal NULL key".into()));
                }
                header(output, &entry_type, false, false, depth + 1, state)?;
                output.field(102);
                output.property(100, 2);
                state.charge_nodes(2)?;
                self::value(output, key, key_value, false, depth + 2, state)?;
                self::value(output, value_type, value_value, false, depth + 2, state)?;
                output.end();
                output.end();
            }
        }
        (NestedType::Union(fields), NestedPayload::Union { tag, value }) => {
            if *tag >= fields.len() {
                return Err(Error::Conversion("native UNION literal tag".into()));
            }
            state.charge_nodes(fields.len() + 1)?;
            output.unsigned(fields.len() as u64 + 1);
            self::value(
                output,
                &DataType::UTinyInt,
                &Value::Unsigned(*tag as u128),
                false,
                depth + 1,
                state,
            )?;
            for (index, (_, child)) in fields.iter().enumerate() {
                self::value(
                    output,
                    child,
                    if index == *tag { value } else { &Value::Null },
                    false,
                    depth + 1,
                    state,
                )?;
            }
        }
        (NestedType::Variant, NestedPayload::Variant { .. }) => {
            let selected = state.binding(ty)?;
            let physical = super::super::nested::variant::value::encode(
                value,
                &selected,
                depth,
                &mut state.nodes,
                &mut state.bytes,
                state.query,
            )?;
            let Value::Nested(physical) = physical else {
                return Err(Error::Internal("canonical literal VARIANT is NULL".into()));
            };
            let NestedPayload::Struct(children) = &physical.payload else {
                return Err(Error::Internal("canonical literal VARIANT shape".into()));
            };
            let fields = metadata::fields(metadata)?;
            state.charge_nodes(children.len())?;
            output.unsigned(children.len() as u64);
            for ((_, child), child_value) in fields.iter().zip(children) {
                self::value(output, child, child_value, false, depth + 1, state)?;
            }
        }
        _ => {
            return Err(Error::Conversion(
                "native nested literal payload mismatch".into(),
            ));
        }
    }
    output.end();
    Ok(())
}
