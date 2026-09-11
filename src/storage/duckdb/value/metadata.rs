use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn fields(metadata: &NestedType) -> Result<Vec<(String, DataType)>> {
    if matches!(metadata, NestedType::Variant) {
        let DataType::Nested(ty) = super::super::nested::variant::wal::data_type() else {
            unreachable!()
        };
        let NestedType::Struct(fields) = ty.as_ref() else {
            unreachable!()
        };
        return Ok(fields.clone());
    }
    super::super::nested::physical_fields(metadata)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write(
    output: &mut Encoder,
    ty: &DataType,
    depth: usize,
    state: &mut State<'_>,
) -> Result<()> {
    write_at(output, ty, depth, state, true)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write_for_cast(
    output: &mut Encoder,
    ty: &DataType,
    state: &mut State<'_>,
) -> Result<()> {
    write_at(output, ty, 0, state, false)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn write_at(
    output: &mut Encoder,
    ty: &DataType,
    depth: usize,
    state: &mut State<'_>,
    literal: bool,
) -> Result<()> {
    state.visit(depth)?;
    if literal {
        state.version_type(ty)?;
    }
    crate::common::type_registry::check_metadata(ty)?;
    if *ty == DataType::Null {
        output.property(100, 1);
        output.end();
        return Ok(());
    }
    let DataType::Nested(metadata) = ty else {
        if let DataType::Enum(metadata) = ty {
            state.charge_nodes(metadata.labels.len())?;
            output.property(100, 104);
            output.field(101);
            output.boolean(true);
            output.property(100, 6);
            output.property(200, metadata.labels.len() as u64);
            output.property(201, metadata.labels.len() as u64);
            for label in &metadata.labels {
                state.write_blob(output, label.as_bytes())?;
            }
            output.end();
            output.end();
            return Ok(());
        }
        return super::super::primitive::write_type(output, ty);
    };
    output.property(100, super::super::nested::type_id(metadata)?);
    output.field(101);
    output.boolean(true);
    match metadata.as_ref() {
        NestedType::List(_) | NestedType::Map { .. } | NestedType::Array { .. } => {
            output.property(
                100,
                if matches!(metadata.as_ref(), NestedType::Array { .. }) {
                    9
                } else {
                    4
                },
            );
            output.field(200);
            write_at(
                output,
                &super::super::nested::list_child(metadata)?,
                depth + 1,
                state,
                literal,
            )?;
            if let NestedType::Array { length, .. } = metadata.as_ref() {
                output.property(201, *length as u64);
            }
        }
        _ => {
            let fields = fields(metadata)?;
            output.property(100, 5);
            output.property(200, fields.len() as u64);
            for (name, child) in fields {
                output.field(0);
                state.write_blob(output, name.as_bytes())?;
                output.field(1);
                write_at(output, &child, depth + 1, state, literal)?;
                output.end();
            }
        }
    }
    output.end();
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(reader: &mut Reader, depth: usize, state: &mut State<'_>) -> Result<DataType> {
    read_at(reader, depth, state, &mut 4096, true)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_for_cast(reader: &mut Reader, state: &mut State<'_>) -> Result<DataType> {
    read_at(reader, 0, state, &mut 4096, false)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn read_at(
    reader: &mut Reader,
    depth: usize,
    state: &mut State<'_>,
    type_nodes: &mut usize,
    literal: bool,
) -> Result<DataType> {
    state.visit(depth)?;
    *type_nodes = type_nodes
        .checked_sub(1)
        .ok_or_else(|| Error::Resource("native literal type exceeds 4096 nodes".into()))?;
    reader.field(100)?;
    let id = reader.unsigned()?;
    if literal && (id == 109 && state.version < 68 || id == 110 && state.version < 69) {
        return Err(Error::Unsupported(format!(
            "native literal type ID {id} at storage version {}",
            state.version
        )));
    }
    let ty = match id {
        1 => DataType::Null,
        10 => DataType::Boolean,
        11 => DataType::TinyInt,
        12 => DataType::SmallInt,
        13 => DataType::Integer,
        14 => DataType::BigInt,
        15 => DataType::Date,
        16 => DataType::Time,
        17 => DataType::TimestampS,
        18 => DataType::TimestampMs,
        19 => DataType::Timestamp,
        20 => DataType::TimestampNs,
        22 => DataType::Float,
        23 => DataType::Double,
        25 => DataType::Varchar,
        26 => DataType::Blob,
        27 => DataType::Interval,
        28 => DataType::UTinyInt,
        29 => DataType::USmallInt,
        30 => DataType::UInteger,
        31 => DataType::UBigInt,
        32 => DataType::TimestampTz,
        33 => DataType::TimestampTzNs,
        34 => DataType::TimeTz,
        35 => DataType::TimeNs,
        36 => DataType::Bit,
        39 => DataType::Bignum,
        49 => DataType::UHugeInt,
        50 => DataType::HugeInt,
        54 => DataType::Uuid,
        21 => {
            info(reader, 2, state)?;
            let width = u8::try_from(reader.optional_unsigned(200, 0)?)
                .map_err(|_| corrupt("DECIMAL width overflow"))?;
            let scale = u8::try_from(reader.optional_unsigned(201, 0)?)
                .map_err(|_| corrupt("DECIMAL scale overflow"))?;
            reader.end()?;
            DataType::Decimal { width, scale }
        }
        104 => {
            info(reader, 6, state)?;
            reader.field(200)?;
            let count = reader.length()?;
            state.charge_nodes(count)?;
            reader.field(201)?;
            if reader.length()? != count {
                return Err(corrupt("ENUM dictionary count mismatch"));
            }
            let mut labels = reserve(count)?;
            for _ in 0..count {
                labels.push(state.string(reader)?);
            }
            reader.end()?;
            DataType::enumeration(labels).map_err(wire_error)?
        }
        100 | 101 | 102 | 107 | 108 | 109 | 110 => {
            nested(reader, id, depth, state, type_nodes, literal)?
        }
        _ => {
            return Err(Error::Unsupported(format!(
                "native literal logical type {id}"
            )));
        }
    };
    reader.end()?;
    crate::common::type_registry::check_metadata(&ty).map_err(wire_error)?;
    Ok(ty)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn info(reader: &mut Reader, kind: u64, state: &mut State<'_>) -> Result<()> {
    reader.field(101)?;
    if !reader.boolean()? {
        return Err(corrupt("missing native literal type information"));
    }
    reader.field(100)?;
    if reader.unsigned()? != kind {
        return Err(corrupt("native literal type information kind mismatch"));
    }
    if reader.optional(101)? && !state.string(reader)?.is_empty() {
        return Err(Error::Unsupported("aliased native literal type".into()));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nested(
    reader: &mut Reader,
    id: u64,
    depth: usize,
    state: &mut State<'_>,
    type_nodes: &mut usize,
    literal: bool,
) -> Result<DataType> {
    info(
        reader,
        match id {
            101 | 102 => 4,
            108 => 9,
            _ => 5,
        },
        state,
    )?;
    let metadata = match id {
        101 | 102 | 108 => {
            reader.field(200)?;
            let child = read_at(reader, depth + 1, state, type_nodes, literal)?;
            match id {
                101 => NestedType::List(child),
                108 => NestedType::Array {
                    element: child,
                    length: reader
                        .optional_unsigned(201, 0)?
                        .try_into()
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
                        return Err(corrupt("MAP child metadata mismatch"));
                    }
                    NestedType::Map {
                        key: fields[0].1.clone(),
                        value: fields[1].1.clone(),
                    }
                }
            }
        }
        _ => {
            let count = if reader.optional(200)? {
                reader.length()?
            } else {
                0
            };
            if count > *type_nodes {
                return Err(Error::Resource(
                    "native Value type exceeds 4096 fields".into(),
                ));
            }
            state.charge_nodes(count)?;
            let mut fields = reserve(count)?;
            for _ in 0..count {
                reader.field(0)?;
                let name = state.string(reader)?;
                reader.field(1)?;
                let ty = read_at(reader, depth + 1, state, type_nodes, literal)?;
                reader.end()?;
                fields.push((name, ty));
            }
            if id == 109 {
                if fields != self::fields(&NestedType::Variant)? {
                    return Err(corrupt("VARIANT physical type mismatch"));
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
                    return Err(corrupt("named TUPLE fields"));
                }
                // Legacy unnamed STRUCT has observable TUPLE identity on the
                // development reader. Reading it does not assert old writers
                // can publish that identity without the version-69 type ID.
                NestedType::Tuple(fields.into_iter().map(|(_, ty)| ty).collect())
            } else {
                NestedType::Struct(fields)
            }
        }
    };
    reader.end()?;
    Ok(metadata.data_type())
}
