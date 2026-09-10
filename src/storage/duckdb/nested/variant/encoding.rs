//! Canonical unshredded VARIANT payload construction, independent of native
//! header/version publication. The caller supplies its retained VARIANT adapter;
//! serialization never reselects builtin or ambient type semantics.
use super::*;
use crate::{
    common::{type_registry::BoundType, variant::Node},
    parallel::QueryContext,
};
#[cfg(test)]
mod tests;

struct Limits {
    nodes: usize,
    bytes: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Limits {
    fn nodes(&mut self, count: usize) -> Result<()> {
        self.nodes = self.nodes.checked_sub(count).ok_or_else(|| {
            Error::Resource("native VARIANT encoding exceeds 16 million visits".into())
        })?;
        Ok(())
    }
    fn bytes(&mut self, count: usize) -> Result<()> {
        self.bytes = self.bytes.checked_sub(count).ok_or_else(|| {
            Error::Resource("native VARIANT encoding exceeds 64 MiB payload bytes".into())
        })?;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_rows(
    values: &[Value],
    variant: &BoundType,
    query: &QueryContext,
) -> Result<Vec<Value>> {
    encode_with_limits(
        values,
        variant,
        query,
        &mut Limits {
            nodes: 16_777_216,
            bytes: 64 * 1024 * 1024,
        },
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encode_with_limits(
    values: &[Value],
    variant: &BoundType,
    query: &QueryContext,
    limits: &mut Limits,
) -> Result<Vec<Value>> {
    query.check()?;
    let data_type = NestedType::Variant.data_type();
    if variant.data_type() != &data_type {
        return Err(Error::Internal(
            "native VARIANT encoder requires a selected VARIANT type".into(),
        ));
    }
    limits.nodes(values.len())?;
    let mut result = Vec::new();
    reserve(&mut result, values.len())?;
    for value in values {
        // Retained validation applies even to NULL rows and before traversing
        // source metadata. Errors never publish a partial output collection.
        variant.validate(value, query)?;
        if value.is_null() {
            result.push(Value::Null);
            continue;
        }
        let mut row = Builder {
            keys: Vec::new(),
            children: Vec::new(),
            values: Vec::new(),
            data: Vec::new(),
            limits,
            query,
        };
        row.emit(Node::Typed(&data_type, value), 0)?;
        if row.values.first().is_none_or(|(tag, _)| *tag == 0) {
            return Err(Error::Conversion(
                "native VARIANT root NULL requires row validity".into(),
            ));
        }
        result.push(row.finish()?);
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<()> {
    values
        .try_reserve(additional)
        .map_err(|_| Error::Resource("cannot allocate canonical VARIANT payload".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn index(value: usize) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| Error::Resource("canonical VARIANT index exceeds UINT32".into()))
}

struct Builder<'a> {
    keys: Vec<String>,
    children: Vec<(Option<u32>, u32)>,
    values: Vec<(u8, u32)>,
    data: Vec<u8>,
    limits: &'a mut Limits,
    query: &'a QueryContext,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Builder<'_> {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.query.check()?;
        self.limits.bytes(bytes.len())?;
        reserve(&mut self.data, bytes.len())?;
        self.data.extend_from_slice(bytes);
        self.query.check()
    }
    fn varint(&mut self, value: usize) -> Result<()> {
        let mut value = index(value)?;
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            self.bytes(&[byte | if value == 0 { 0 } else { 128 }])?;
            if value == 0 {
                return Ok(());
            }
        }
    }
    fn string(&mut self, bytes: &[u8]) -> Result<()> {
        self.varint(bytes.len())?;
        self.bytes(bytes)
    }
    fn native_string(&mut self, value: &Value) -> Result<()> {
        let length = match value {
            Value::Bit(value) => value.bytes().len().checked_add(1),
            Value::Bignum(value) => value.byte_len().checked_add(3),
            _ => return Err(Error::Internal("VARIANT native string source".into())),
        }
        .ok_or_else(|| Error::Resource("VARIANT native string length overflow".into()))?;
        self.varint(length)?;
        // Reserve/charge before allocating the scalar's native conversion.
        self.limits.bytes(length)?;
        reserve(&mut self.data, length)?;
        let bytes = match value {
            Value::Bit(value) => value.to_native(|| self.query.check())?,
            Value::Bignum(value) => value.to_native(|| self.query.check())?,
            _ => unreachable!(),
        };
        if bytes.len() != length {
            return Err(Error::Internal(
                "VARIANT native scalar length changed".into(),
            ));
        }
        self.data.extend_from_slice(&bytes);
        Ok(())
    }
    fn descriptor(&mut self, tag: u8) -> Result<u32> {
        self.limits.nodes(1)?;
        let position = index(self.values.len())?;
        let offset = index(self.data.len())?;
        reserve(&mut self.values, 1)?;
        self.values.push((tag, offset));
        Ok(position)
    }
    fn container(&mut self, tag: u8, count: usize) -> Result<(u32, usize)> {
        self.limits.nodes(count)?;
        let position = self.descriptor(tag)?;
        self.varint(count)?;
        let start = self.children.len();
        if count != 0 {
            self.varint(start)?;
        }
        let end = start
            .checked_add(count)
            .ok_or_else(|| Error::Resource("VARIANT child count overflow".into()))?;
        index(end)?;
        reserve(&mut self.children, count)?;
        self.children.resize(end, (None, 0));
        Ok((position, start))
    }
    fn member(&mut self, slot: usize, name: &str, node: Node<'_>, depth: usize) -> Result<()> {
        self.query.check()?;
        self.limits.nodes(1)?;
        self.limits.bytes(name.len())?;
        let key = index(self.keys.len())?;
        reserve(&mut self.keys, 1)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(name.len())
            .map_err(|_| Error::Resource("cannot allocate VARIANT member name".into()))?;
        owned.push_str(name);
        self.keys.push(owned);
        let child = self.emit(node, depth + 1)?;
        self.children[slot] = (Some(key), child);
        Ok(())
    }
    fn emit(&mut self, node: Node<'_>, depth: usize) -> Result<u32> {
        self.query.check()?;
        self.limits.nodes(1)?;
        if depth > 64 {
            return Err(Error::Resource(
                "native VARIANT encoding depth exceeds 64".into(),
            ));
        }
        let (data_type, value) = match node {
            Node::MapEntry(kt, key, vt, value) => {
                let (position, start) = self.container(29, 2)?;
                self.member(start, "key", Node::Typed(kt, key), depth)?;
                self.member(start + 1, "value", Node::Typed(vt, value), depth)?;
                return Ok(position);
            }
            Node::Typed(ty, value) => (ty, value),
        };
        if value.is_null() {
            return self.descriptor(0);
        }
        if let (DataType::Nested(metadata), Value::Nested(value)) = (data_type, value) {
            return match (metadata.as_ref(), &value.payload) {
                (NestedType::Variant, NestedPayload::Variant { data_type, value }) => {
                    self.emit(Node::Typed(data_type, value), depth + 1)
                }
                (NestedType::Union(fields), NestedPayload::Union { tag, value }) => self.emit(
                    Node::Typed(
                        &fields
                            .get(*tag)
                            .ok_or_else(|| Error::Conversion("VARIANT UNION tag".into()))?
                            .1,
                        value,
                    ),
                    depth + 1,
                ),
                (
                    NestedType::List(child) | NestedType::Array { element: child, .. },
                    NestedPayload::Sequence(values),
                ) => {
                    let (position, start) = self.container(30, values.len())?;
                    for (offset, value) in values.iter().enumerate() {
                        let child = self.emit(Node::Typed(child, value), depth + 1)?;
                        self.children[start + offset] = (None, child);
                    }
                    Ok(position)
                }
                (NestedType::Tuple(fields), NestedPayload::Struct(values)) => {
                    let (position, start) = self.container(30, values.len())?;
                    for (offset, (ty, value)) in fields.iter().zip(values).enumerate() {
                        let child = self.emit(Node::Typed(ty, value), depth + 1)?;
                        self.children[start + offset] = (None, child);
                    }
                    Ok(position)
                }
                (
                    NestedType::Struct(fields) | NestedType::Object(fields),
                    NestedPayload::Struct(values),
                ) => {
                    let (position, start) = self.container(29, values.len())?;
                    for (offset, ((name, ty), value)) in fields.iter().zip(values).enumerate() {
                        self.member(start + offset, name, Node::Typed(ty, value), depth)?;
                    }
                    Ok(position)
                }
                (NestedType::Map { key, value }, NestedPayload::Map(entries)) => {
                    let (position, start) = self.container(30, entries.len())?;
                    for (offset, (k, v)) in entries.iter().enumerate() {
                        let child = self.emit(Node::MapEntry(key, k, value, v), depth + 1)?;
                        self.children[start + offset] = (None, child);
                    }
                    Ok(position)
                }
                _ => Err(Error::Conversion(
                    "VARIANT encoding nested shape mismatch".into(),
                )),
            };
        }
        self.primitive(data_type, value)
    }
    fn primitive(&mut self, ty: &DataType, value: &Value) -> Result<u32> {
        let tag = match ty {
            DataType::Boolean => match value {
                Value::Boolean(true) => 1,
                Value::Boolean(false) => 2,
                _ => return Err(Error::Conversion("VARIANT Boolean payload".into())),
            },
            DataType::TinyInt => 3,
            DataType::SmallInt => 4,
            DataType::Integer => 5,
            DataType::BigInt => 6,
            DataType::HugeInt => 7,
            DataType::UTinyInt => 8,
            DataType::USmallInt => 9,
            DataType::UInteger => 10,
            DataType::UBigInt => 11,
            DataType::UHugeInt => 12,
            DataType::Float => 13,
            DataType::Double => 14,
            DataType::Decimal { .. } => 15,
            DataType::Varchar | DataType::Enum(_) => 16,
            DataType::Blob => 17,
            DataType::Uuid => 18,
            DataType::Date => 19,
            DataType::Time => 20,
            DataType::TimeNs => 21,
            DataType::TimestampS => 22,
            DataType::TimestampMs => 23,
            DataType::Timestamp => 24,
            DataType::TimestampNs => 25,
            DataType::TimeTz => 26,
            DataType::TimestampTz => 27,
            DataType::Interval => 28,
            DataType::Bignum => 31,
            DataType::Bit => 32,
            DataType::TimestampTzNs => 34,
            _ => {
                return Err(Error::Unsupported(format!(
                    "native VARIANT encoding for {ty}"
                )));
            }
        };
        let position = self.descriptor(tag)?;
        if let DataType::Decimal { width, scale } = ty {
            self.varint(usize::from(*width))?;
            self.varint(usize::from(*scale))?;
        }
        match value {
            Value::Boolean(_) => {}
            Value::Integer(value) => {
                self.bytes(&value.to_le_bytes()[..super::super::super::primitive::width(ty)?])?
            }
            Value::Unsigned(value) => {
                self.bytes(&value.to_le_bytes()[..super::super::super::primitive::width(ty)?])?
            }
            Value::Decimal { value, .. } => {
                self.bytes(&value.to_le_bytes()[..super::super::super::primitive::width(ty)?])?
            }
            Value::Float(value) => self.bytes(&value.to_le_bytes())?,
            Value::Double(value) => self.bytes(&value.to_le_bytes())?,
            Value::Varchar(value) => self.string(value.as_bytes())?,
            Value::Blob(value) => self.string(value)?,
            Value::Enum(value) => self.string(value.label()?.as_bytes())?,
            Value::Bit(_) | Value::Bignum(_) => self.native_string(value)?,
            Value::Uuid(value) => self.bytes(&(*value ^ (1_u128 << 127)).to_le_bytes())?,
            Value::Date(value) => self.bytes(&value.days().to_le_bytes())?,
            Value::Temporal(value) => {
                let length = super::super::super::primitive::width(ty)?;
                self.limits.bytes(length)?;
                reserve(&mut self.data, length)?;
                value.append_storage(&mut self.data)?;
            }
            _ => {
                return Err(Error::Conversion(
                    "VARIANT encoding scalar shape mismatch".into(),
                ));
            }
        }
        Ok(position)
    }
    fn finish(self) -> Result<Value> {
        let fields = fields();
        let mut keys = Vec::new();
        reserve(&mut keys, self.keys.len())?;
        for (index, key) in self.keys.into_iter().enumerate() {
            if index % 1024 == 0 {
                self.query.check()?;
            }
            keys.push(Value::Varchar(key));
        }
        let keys = NestedValue::value(fields[0].1.clone(), NestedPayload::Sequence(keys))?;
        let children = records(
            &fields[1].1,
            self.children.into_iter().map(|(key, value)| {
                vec![
                    key.map_or(Value::Null, |key| Value::Unsigned(u128::from(key))),
                    Value::Unsigned(u128::from(value)),
                ]
            }),
            self.query,
        )?;
        let values = records(
            &fields[2].1,
            self.values.into_iter().map(|(tag, offset)| {
                vec![
                    Value::Unsigned(u128::from(tag)),
                    Value::Unsigned(u128::from(offset)),
                ]
            }),
            self.query,
        )?;
        NestedValue::value(
            unshredded_type(),
            NestedPayload::Struct(vec![keys, children, values, Value::Blob(self.data)]),
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn records(
    ty: &DataType,
    source: impl ExactSizeIterator<Item = Vec<Value>>,
    query: &QueryContext,
) -> Result<Value> {
    let DataType::Nested(metadata) = ty else {
        return Err(Error::Internal("VARIANT physical record list".into()));
    };
    let NestedType::List(child) = metadata.as_ref() else {
        return Err(Error::Internal("VARIANT physical record list".into()));
    };
    let mut values = Vec::new();
    reserve(&mut values, source.len())?;
    for (index, row) in source.enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        values.push(NestedValue::value(
            child.clone(),
            NestedPayload::Struct(row),
        )?);
    }
    NestedValue::value(ty.clone(), NestedPayload::Sequence(values))
}
