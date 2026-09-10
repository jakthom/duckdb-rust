//! Bounded logical decoding of one canonical unshredded VARIANT row. Offsets
//! and child references are checked before following them; native bytes are
//! never interpreted as Rust ownership or as an opaque logical value.
use super::*;
use crate::common::{BignumValue, BitString};
#[cfg(test)]
mod tests;

pub(super) type Typed = (DataType, Value);

pub(super) struct Budget {
    nodes: usize,
    bytes: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Budget {
    pub(super) fn new() -> Self {
        Self {
            nodes: 16_777_216,
            bytes: 64 * 1024 * 1024,
        }
    }
    pub(super) fn nodes(&mut self, count: usize) -> Result<()> {
        self.nodes = self.nodes.checked_sub(count).ok_or_else(|| {
            Error::Resource("native VARIANT exceeds 16 million logical visits".into())
        })?;
        Ok(())
    }
    pub(super) fn bytes(&mut self, count: usize) -> Result<()> {
        self.bytes = self.bytes.checked_sub(count).ok_or_else(|| {
            Error::Resource("native VARIANT materialization exceeds 64 MiB".into())
        })?;
        Ok(())
    }
}

pub(super) struct Unshredded<'a> {
    keys: &'a [Value],
    children: &'a [Value],
    values: &'a [Value],
    data: &'a [u8],
    ordered_objects: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn record(value: &Value) -> Result<&[Value]> {
    match value {
        Value::Nested(value) => match &value.payload {
            NestedPayload::Struct(fields) => Ok(fields),
            _ => Err(corrupt("VARIANT physical child is not STRUCT")),
        },
        _ => Err(corrupt(
            "VARIANT physical STRUCT child is NULL or malformed",
        )),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn sequence(value: &Value) -> Result<&[Value]> {
    match value {
        Value::Nested(value) => match &value.payload {
            NestedPayload::Sequence(values) => Ok(values),
            _ => Err(corrupt("VARIANT physical child is not LIST")),
        },
        _ => Err(corrupt("VARIANT physical LIST child is NULL or malformed")),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn index(value: &Value) -> Result<usize> {
    match value {
        Value::Unsigned(value) if *value <= u32::MAX as u128 => Ok(*value as usize),
        _ => Err(corrupt("VARIANT index is NULL or outside UINT32")),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> Unshredded<'a> {
    pub(super) fn new(value: &'a Value, budget: &mut Budget) -> Result<Option<Self>> {
        if value.is_null() {
            return Ok(None);
        }
        let [keys, children, values, Value::Blob(data)] = record(value)? else {
            return Err(corrupt("VARIANT canonical row shape"));
        };
        let result = Self {
            keys: sequence(keys)?,
            children: sequence(children)?,
            values: sequence(values)?,
            data,
            ordered_objects: false,
        };
        budget.nodes(result.keys.len())?;
        budget.nodes(result.children.len())?;
        budget.nodes(result.values.len())?;
        for key in result.keys {
            if !matches!(key, Value::Varchar(_)) {
                return Err(corrupt("VARIANT key is NULL or not VARCHAR"));
            }
        }
        for child in result.children {
            let [key, value] = record(child)? else {
                return Err(corrupt("VARIANT child reference shape"));
            };
            if !key.is_null() && index(key)? >= result.keys.len() {
                return Err(corrupt("VARIANT key reference outside dictionary"));
            }
            if index(value)? >= result.values.len() {
                return Err(corrupt("VARIANT value reference outside dictionary"));
            }
        }
        for value in result.values {
            let [tag, offset] = record(value)? else {
                return Err(corrupt("VARIANT value descriptor shape"));
            };
            if index(tag)? > 34 || index(offset)? > data.len() {
                return Err(corrupt("VARIANT value tag or byte offset outside payload"));
            }
        }
        Ok(Some(result))
    }

    pub(super) fn with_ordered_objects(mut self) -> Self {
        self.ordered_objects = true;
        self
    }

    pub(super) fn decode(&self, position: usize, budget: &mut Budget) -> Result<Typed> {
        self.decode_from(position, 0, budget)
    }

    pub(super) fn decode_from(
        &self,
        position: usize,
        depth: usize,
        budget: &mut Budget,
    ) -> Result<Typed> {
        self.decode_at(position, depth, &mut Vec::new(), budget)
    }

    fn decode_at(
        &self,
        position: usize,
        depth: usize,
        active: &mut Vec<usize>,
        budget: &mut Budget,
    ) -> Result<Typed> {
        budget.nodes(1)?;
        if depth > 64 {
            return Err(Error::Resource("native VARIANT nesting exceeds 64".into()));
        }
        if active.contains(&position) {
            return Err(corrupt("cyclic native VARIANT child references"));
        }
        let descriptor = self
            .values
            .get(position)
            .ok_or_else(|| corrupt("VARIANT value index outside dictionary"))?;
        let [tag, offset] = record(descriptor)? else {
            return Err(corrupt("VARIANT descriptor shape"));
        };
        let tag = index(tag)?;
        let mut offset = index(offset)?;
        match tag {
            0 => return Ok((DataType::Null, Value::Null)),
            1 | 2 => return Ok((DataType::Boolean, Value::Boolean(tag == 1))),
            29 | 30 => {
                let count = self.varint(&mut offset)?;
                let start = if count == 0 {
                    0
                } else {
                    self.varint(&mut offset)?
                };
                let end = start
                    .checked_add(count)
                    .ok_or_else(|| corrupt("VARIANT child range overflow"))?;
                let children = self
                    .children
                    .get(start..end)
                    .ok_or_else(|| corrupt("VARIANT child range outside dictionary"))?;
                active.push(position);
                let decoded = (|| {
                    budget.nodes(count)?;
                    let mut array = Vec::new();
                    let mut object = Vec::new();
                    if tag == 30 {
                        array.try_reserve_exact(count)
                    } else {
                        object.try_reserve_exact(count)
                    }
                    .map_err(|_| {
                        Error::Resource("cannot allocate native VARIANT children".into())
                    })?;
                    for child in children {
                        let [key, value] = record(child)? else {
                            return Err(corrupt("VARIANT child reference shape"));
                        };
                        let value = self.decode_at(index(value)?, depth + 1, active, budget)?;
                        if tag == 30 {
                            if !key.is_null() {
                                return Err(corrupt("VARIANT ARRAY child has an OBJECT key"));
                            }
                            array.push(value);
                        } else {
                            let Value::Varchar(key) = self
                                .keys
                                .get(index(key)?)
                                .ok_or_else(|| corrupt("VARIANT OBJECT key outside dictionary"))?
                            else {
                                return Err(corrupt("VARIANT OBJECT key is not VARCHAR"));
                            };
                            budget.bytes(key.len())?;
                            object.push((key.clone(), value));
                        }
                    }
                    if tag == 30 {
                        array_value(array)
                    } else {
                        if self.ordered_objects {
                            object.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                        }
                        object_value(object)
                    }
                })();
                active.pop();
                return decoded;
            }
            16 | 17 | 31 | 32 => {
                let length = self.varint(&mut offset)?;
                let end = offset
                    .checked_add(length)
                    .ok_or_else(|| corrupt("VARIANT string length overflow"))?;
                let bytes = self
                    .data
                    .get(offset..end)
                    .ok_or_else(|| corrupt("truncated VARIANT string"))?;
                budget.bytes(length)?;
                return Ok(match tag {
                    16 => (
                        DataType::Varchar,
                        Value::Varchar(
                            std::str::from_utf8(bytes)
                                .map_err(|_| corrupt("invalid UTF-8 VARIANT string"))?
                                .to_owned(),
                        ),
                    ),
                    17 => (DataType::Blob, Value::Blob(bytes.to_vec())),
                    31 => (
                        DataType::Bignum,
                        BignumValue::from_native(bytes, || Ok(()))?.value(),
                    ),
                    32 => (
                        DataType::Bit,
                        BitString::from_native(bytes, || Ok(()))?.value(),
                    ),
                    _ => unreachable!(),
                });
            }
            33 => return Err(Error::Unsupported("native VARIANT GEOMETRY payload".into())),
            _ => {}
        }
        let ty = match tag {
            3 => DataType::TinyInt,
            4 => DataType::SmallInt,
            5 => DataType::Integer,
            6 => DataType::BigInt,
            7 => DataType::HugeInt,
            8 => DataType::UTinyInt,
            9 => DataType::USmallInt,
            10 => DataType::UInteger,
            11 => DataType::UBigInt,
            12 => DataType::UHugeInt,
            13 => DataType::Float,
            14 => DataType::Double,
            15 => {
                let width = u8::try_from(self.varint(&mut offset)?)
                    .map_err(|_| corrupt("VARIANT DECIMAL precision overflow"))?;
                let scale = u8::try_from(self.varint(&mut offset)?)
                    .map_err(|_| corrupt("VARIANT DECIMAL scale overflow"))?;
                let ty = DataType::Decimal { width, scale };
                crate::common::type_registry::check_metadata(&ty)
                    .map_err(|_| corrupt("invalid VARIANT DECIMAL metadata"))?;
                ty
            }
            18 => DataType::Uuid,
            19 => DataType::Date,
            20 => DataType::Time,
            21 => DataType::TimeNs,
            22 => DataType::TimestampS,
            23 => DataType::TimestampMs,
            24 => DataType::Timestamp,
            25 => DataType::TimestampNs,
            26 => DataType::TimeTz,
            27 => DataType::TimestampTz,
            28 => DataType::Interval,
            34 => DataType::TimestampTzNs,
            _ => return Err(corrupt("unknown VARIANT value tag")),
        };
        let value =
            super::super::super::primitive::scalar(self.data, offset, &ty).map_err(|error| {
                match error {
                    // This is a local native decoder, not an extensible SQL cast.
                    // Physical temporal-domain errors describe a corrupt payload;
                    // infrastructure, resource and unsupported failures stay intact.
                    Error::Conversion(message) | Error::OutOfRange(message) => {
                        corrupt(format!("invalid VARIANT scalar payload: {message}"))
                    }
                    other => other,
                }
            })?;
        if value.is_null() || !value.fits_type(&ty) {
            return Err(corrupt("invalid non-NULL VARIANT scalar payload"));
        }
        Ok((ty, value))
    }

    fn varint(&self, offset: &mut usize) -> Result<usize> {
        let mut result = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = *self
                .data
                .get(*offset)
                .ok_or_else(|| corrupt("truncated VARIANT varint"))?;
            *offset = offset
                .checked_add(1)
                .ok_or_else(|| corrupt("VARIANT byte offset overflow"))?;
            if shift == 28 && byte > 15 {
                return Err(corrupt("VARIANT uint32 varint overflow"));
            }
            result |= u32::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Ok(result as usize);
            }
        }
        Err(corrupt("unterminated VARIANT uint32 varint"))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn envelope((data_type, value): Typed) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    NestedValue::value(
        NestedType::Variant.data_type(),
        NestedPayload::Variant { data_type, value },
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn array_value(children: Vec<Typed>) -> Result<Typed> {
    let child_type = children
        .first()
        .map(|(ty, _)| ty.clone())
        .filter(|ty| children.iter().all(|(other, _)| other == ty))
        .unwrap_or_else(|| NestedType::Variant.data_type());
    let variant = child_type == NestedType::Variant.data_type();
    let values = children
        .into_iter()
        .map(|child| {
            if variant {
                envelope(child)
            } else {
                Ok(child.1)
            }
        })
        .collect::<Result<_>>()?;
    let ty = NestedType::List(child_type).data_type();
    Ok((
        ty.clone(),
        NestedValue::value(ty, NestedPayload::Sequence(values))?,
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn object_value(children: Vec<(String, Typed)>) -> Result<Typed> {
    // Canonical objects already collapse exact duplicate JSON keys at input.
    // A stored duplicate (including typed/leftover collisions) is malformed,
    // not permission to discard a child here. Empty and case-distinct names
    // remain valid and do not inherit SQL STRUCT's identifier restrictions.
    let mut names = std::collections::BTreeSet::new();
    for (name, _) in &children {
        if !names.insert(name) {
            return Err(corrupt("duplicate exact native VARIANT OBJECT key"));
        }
    }
    let ty = NestedType::Object(
        children
            .iter()
            .map(|(name, (ty, _))| (name.clone(), ty.clone()))
            .collect(),
    )
    .data_type();
    let values = children.into_iter().map(|(_, (_, value))| value).collect();
    Ok((
        ty.clone(),
        NestedValue::value(ty, NestedPayload::Struct(values))?,
    ))
}
