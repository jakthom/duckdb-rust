//! Resolve typed shredded values and one-based leftover references without
//! confusing a missing OBJECT field with a present field whose value is NULL.
use super::{payload::*, *};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn validate(ty: &DataType) -> Result<()> {
    if let DataType::Nested(metadata) = ty {
        let NestedType::Struct(fields) = metadata.as_ref() else {
            return Err(corrupt(
                "nested shredded VARIANT metadata is missing its wrapper",
            ));
        };
        if fields.is_empty()
            || fields.len() > 2
            || fields[0].0 != "typed_value"
            || fields.len() == 2 && fields[1] != ("untyped_value_index".into(), DataType::UInteger)
        {
            return Err(corrupt("invalid shredded VARIANT wrapper metadata"));
        }
        return validate_content(&fields[0].1);
    }
    validate_content(ty)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_content(ty: &DataType) -> Result<()> {
    match ty {
        DataType::Nested(metadata) => match metadata.as_ref() {
            NestedType::List(child) => validate(child),
            NestedType::Struct(fields) => {
                for (_, ty) in fields {
                    validate(ty)?;
                }
                Ok(())
            }
            _ => Err(corrupt("invalid VARIANT shredded nested metadata")),
        },
        DataType::Null | DataType::Enum(_) | DataType::Extension(_) => {
            Err(corrupt("invalid VARIANT shredded scalar metadata"))
        }
        _ => Ok(()),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn decode(
    ty: &DataType,
    value: &Value,
    unshredded: Option<&Unshredded<'_>>,
    depth: usize,
    budget: &mut Budget,
) -> Result<Option<Typed>> {
    budget.nodes(1)?;
    if depth > 64 {
        return Err(Error::Resource(
            "native shredded VARIANT nesting exceeds 64".into(),
        ));
    }
    if value.is_null() {
        return Ok(Some((DataType::Null, Value::Null)));
    }
    if let DataType::Nested(metadata) = ty {
        let NestedType::Struct(fields) = metadata.as_ref() else {
            return Err(corrupt(
                "nested shredded VARIANT is missing its typed_value wrapper",
            ));
        };
        if fields.is_empty()
            || fields.len() > 2
            || fields[0].0 != "typed_value"
            || fields.len() == 2 && fields[1] != ("untyped_value_index".into(), DataType::UInteger)
        {
            return Err(corrupt("invalid shredded VARIANT wrapper metadata"));
        }
        let values = record(value)?;
        if values.len() != fields.len() {
            return Err(corrupt("shredded VARIANT wrapper shape"));
        }
        let typed = &values[0];
        let overlay = values.get(1).unwrap_or(&Value::Null);
        if typed.is_null() {
            if overlay.is_null() {
                return Ok(Some((DataType::Null, Value::Null)));
            }
            let position = index(overlay)?;
            if position == 0 {
                return Ok(None);
            }
            return Ok(Some(
                unshredded
                    .ok_or_else(|| corrupt("VARIANT leftover reference has no unshredded row"))?
                    .decode_from(position - 1, depth + 1, budget)?,
            ));
        }
        return Ok(Some(content(
            &fields[0].1,
            typed,
            overlay,
            unshredded,
            depth + 1,
            budget,
        )?));
    }
    Ok(Some(primitive(ty, value, budget)?))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn content(
    ty: &DataType,
    value: &Value,
    overlay: &Value,
    unshredded: Option<&Unshredded<'_>>,
    depth: usize,
    budget: &mut Budget,
) -> Result<Typed> {
    if depth > 64 {
        return Err(Error::Resource(
            "native shredded VARIANT nesting exceeds 64".into(),
        ));
    }
    let DataType::Nested(metadata) = ty else {
        return primitive(ty, value, budget);
    };
    match metadata.as_ref() {
        NestedType::List(child) => {
            let values = sequence(value)?;
            budget.nodes(values.len())?;
            let mut children = Vec::new();
            children
                .try_reserve_exact(values.len())
                .map_err(|_| Error::Resource("cannot allocate shredded VARIANT array".into()))?;
            for value in values {
                children.push(
                    decode(child, value, unshredded, depth + 1, budget)?
                        .unwrap_or((DataType::Null, Value::Null)),
                );
            }
            array_value(children)
        }
        NestedType::Struct(fields) => {
            let values = record(value)?;
            if values.len() != fields.len() {
                return Err(corrupt("shredded VARIANT OBJECT shape"));
            }
            budget.nodes(fields.len())?;
            let mut children = Vec::new();
            children
                .try_reserve_exact(fields.len())
                .map_err(|_| Error::Resource("cannot allocate shredded VARIANT object".into()))?;
            for ((name, ty), value) in fields.iter().zip(values) {
                if let Some(value) = decode(ty, value, unshredded, depth + 1, budget)? {
                    budget.bytes(name.len())?;
                    children.push((name.clone(), value));
                }
            }
            if !overlay.is_null() && index(overlay)? != 0 {
                let (ty, value) = unshredded
                    .ok_or_else(|| corrupt("VARIANT OBJECT overlay has no unshredded row"))?
                    .decode_from(index(overlay)? - 1, depth + 1, budget)?;
                let DataType::Nested(metadata) = ty else {
                    return Err(corrupt("VARIANT OBJECT overlay is not OBJECT"));
                };
                let NestedType::Object(fields) = metadata.as_ref() else {
                    return Err(corrupt("VARIANT OBJECT overlay is not OBJECT"));
                };
                let Value::Nested(value) = value else {
                    return Err(corrupt("VARIANT OBJECT overlay is NULL"));
                };
                let NestedPayload::Struct(values) = &value.payload else {
                    return Err(corrupt("VARIANT OBJECT overlay payload"));
                };
                children.try_reserve(fields.len()).map_err(|_| {
                    Error::Resource("cannot allocate VARIANT overlay fields".into())
                })?;
                for ((name, ty), value) in fields.iter().zip(values) {
                    children.push((name.clone(), (ty.clone(), value.clone())));
                }
            }
            // See VariantBuilder::CollectObjectChildren: canonicalizing a
            // shredded vector merges typed and leftover fields, then emits
            // every OBJECT in lexicographic order, not physical field order.
            children.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            object_value(children)
        }
        _ => Err(Error::Unsupported(
            "native VARIANT shredded nested family".into(),
        )),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn primitive(ty: &DataType, value: &Value, budget: &mut Budget) -> Result<Typed> {
    if !value.fits_type(ty) {
        return Err(corrupt("shredded VARIANT scalar does not match metadata"));
    }
    let bytes = match value {
        Value::Varchar(value) => value.len(),
        Value::Blob(value) => value.len(),
        Value::Bit(value) => value.bytes().len(),
        Value::Bignum(value) => value.byte_len(),
        _ => 0,
    };
    budget.bytes(bytes)?;
    Ok((ty.clone(), value.clone()))
}
