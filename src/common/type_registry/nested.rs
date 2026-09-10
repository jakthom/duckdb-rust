use std::{cmp::Ordering, collections::BTreeSet, sync::Arc};

use super::{BoundType, KeyWriter, TypeAdapter, TypeRegistry};
use crate::{
    common::{DataType, Error, NestedPayload, NestedType, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug, Default)]
pub struct NestedTypes {
    children: Vec<BoundType>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NestedTypes {
    fn payload<'a>(&self, value: &'a Value) -> Result<&'a NestedPayload> {
        match value {
            Value::Nested(value) => Ok(&value.payload),
            _ => Err(Error::Conversion("expected nested value".into())),
        }
    }
    fn child(&self, index: usize) -> Result<&BoundType> {
        self.children
            .get(index)
            .ok_or_else(|| Error::Internal("nested adapter was not bound".into()))
    }
    fn compare_child(
        &self,
        index: usize,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        match (left.is_null(), right.is_null()) {
            (true, true) => Ok(Ordering::Equal),
            (true, false) => Ok(Ordering::Greater),
            (false, true) => Ok(Ordering::Less),
            (false, false) => self.child(index)?.compare(left, right, query),
        }
    }
    fn key_child(
        &self,
        index: usize,
        value: &Value,
        writer: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        let mut bytes = Vec::new();
        self.child(index)?.append_key(value, &mut bytes, query)?;
        writer.extend_from_slice(&bytes)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for NestedTypes {
    fn supports_index(&self, _: &DataType) -> bool {
        false
    }
    fn name(&self) -> &'static str {
        "recursive-nested-types"
    }
    fn bind_type(
        &self,
        data_type: &DataType,
        types: &TypeRegistry,
    ) -> Result<Option<Arc<dyn TypeAdapter>>> {
        let DataType::Nested(metadata) = data_type else {
            return Err(Error::Bind("nested type metadata".into()));
        };
        let children = metadata
            .children()
            .into_iter()
            .map(|child| types.bind(child))
            .collect::<Result<_>>()?;
        Ok(Some(Arc::new(Self { children })))
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        let DataType::Nested(metadata) = data_type else {
            return Err(Error::Bind("nested type metadata".into()));
        };
        match metadata.as_ref() {
            NestedType::Array { length, .. } if *length == 0 || *length > 100000 => {
                return Err(Error::Bind(
                    "ARRAY size must be between 1 and 100000".into(),
                ));
            }
            NestedType::Struct(fields) | NestedType::Union(fields) => {
                if matches!(metadata.as_ref(), NestedType::Union(_))
                    && (fields.is_empty() || fields.len() > 256)
                {
                    return Err(Error::Bind("invalid nested field count".into()));
                }
                let mut names = BTreeSet::new();
                for (name, _) in fields {
                    if name.is_empty() || !names.insert(name.to_ascii_lowercase()) {
                        return Err(Error::Bind(
                            "nested fields require unique nonempty names".into(),
                        ));
                    }
                }
            }
            NestedType::Object(fields) => {
                let mut names = BTreeSet::new();
                for (name, _) in fields {
                    if !names.insert(name) {
                        return Err(Error::Bind(
                            "OBJECT fields require exact unique names".into(),
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn validate_value(&self, _: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        query.check()?;
        match self.payload(value)? {
            NestedPayload::Sequence(values) => {
                for value in values {
                    self.child(0)?.validate(value, query)?;
                }
            }
            NestedPayload::Struct(values) => {
                for (index, value) in values.iter().enumerate() {
                    self.child(index)?.validate(value, query)?;
                }
            }
            NestedPayload::Map(entries) => {
                let mut keys = BTreeSet::new();
                for (key, value) in entries {
                    if key.is_null() {
                        return Err(Error::Conversion("MAP keys cannot be NULL".into()));
                    }
                    let mut bytes = Vec::new();
                    self.child(0)?.append_key(key, &mut bytes, query)?;
                    if !keys.insert(bytes) {
                        return Err(Error::Conversion("MAP keys must be unique".into()));
                    }
                    self.child(1)?.validate(value, query)?;
                }
            }
            NestedPayload::Union { tag, value } => self.child(*tag)?.validate(value, query)?,
            NestedPayload::Variant { .. } => {
                return Err(Error::Unsupported(
                    "VARIANT runtime semantics are not integrated yet".into(),
                ));
            }
        }
        Ok(())
    }
    fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
        Ok(None)
    }
    fn common_type_with_registry(
        &self,
        left: &DataType,
        right: &DataType,
        types: &TypeRegistry,
    ) -> Result<Option<DataType>> {
        let (DataType::Nested(a), DataType::Nested(b)) = (left, right) else {
            return Ok(None);
        };
        let result = match (a.as_ref(), b.as_ref()) {
            (NestedType::List(a), NestedType::List(b))
            | (NestedType::List(a), NestedType::Array { element: b, .. })
            | (NestedType::Array { element: a, .. }, NestedType::List(b)) => {
                NestedType::List(types.common_type(a, b)?)
            }
            (
                NestedType::Array {
                    element: a,
                    length: x,
                },
                NestedType::Array {
                    element: b,
                    length: y,
                },
            ) if x == y => NestedType::Array {
                element: types.common_type(a, b)?,
                length: *x,
            },
            (NestedType::Struct(a), NestedType::Struct(b)) => {
                let mut fields = a.clone();
                for (name, ty) in b {
                    if let Some((_, existing)) = fields
                        .iter_mut()
                        .find(|(field, _)| field.eq_ignore_ascii_case(name))
                    {
                        *existing = types.common_type(existing, ty)?;
                    } else {
                        fields.push((name.clone(), ty.clone()));
                    }
                }
                NestedType::Struct(fields)
            }
            (NestedType::Map { key: a, value: x }, NestedType::Map { key: b, value: y }) => {
                NestedType::Map {
                    key: types.common_type(a, b)?,
                    value: types.common_type(x, y)?,
                }
            }
            (NestedType::Tuple(a), NestedType::Tuple(b)) if a.len() == b.len() => {
                NestedType::Tuple(
                    a.iter()
                        .zip(b)
                        .map(|(a, b)| types.common_type(a, b))
                        .collect::<Result<_>>()?,
                )
            }
            (NestedType::Tuple(a), NestedType::Struct(b))
            | (NestedType::Struct(b), NestedType::Tuple(a))
                if a.len() == b.len() =>
            {
                NestedType::Struct(
                    a.iter()
                        .zip(b)
                        .map(|(a, (name, b))| Ok((name.clone(), types.common_type(a, b)?)))
                        .collect::<Result<_>>()?,
                )
            }
            _ => return Ok(None),
        };
        Ok(Some(result.data_type()))
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        let order = match (self.payload(left)?, self.payload(right)?) {
            (NestedPayload::Sequence(a), NestedPayload::Sequence(b))
            | (NestedPayload::Struct(a), NestedPayload::Struct(b)) => {
                let structure = matches!(self.payload(left)?, NestedPayload::Struct(_));
                for (index, (a, b)) in a.iter().zip(b).enumerate() {
                    let order =
                        self.compare_child(if structure { index } else { 0 }, a, b, query)?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                a.len().cmp(&b.len())
            }
            (NestedPayload::Map(a), NestedPayload::Map(b)) => {
                for ((ak, av), (bk, bv)) in a.iter().zip(b) {
                    for (index, a, b) in [(0, ak, bk), (1, av, bv)] {
                        let order = self.compare_child(index, a, b, query)?;
                        if order != Ordering::Equal {
                            return Ok(order);
                        }
                    }
                }
                a.len().cmp(&b.len())
            }
            (
                NestedPayload::Union { tag: a, value: av },
                NestedPayload::Union { tag: b, value: bv },
            ) => {
                if a != b {
                    a.cmp(b)
                } else {
                    self.compare_child(*a, av, bv, query)?
                }
            }
            _ => return Err(Error::Unsupported("nested comparison payload".into())),
        };
        Ok(order)
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        match self.payload(value)? {
            NestedPayload::Sequence(values) | NestedPayload::Struct(values) => {
                output.extend_from_slice(&(values.len() as u64).to_le_bytes())?;
                let structure = matches!(self.payload(value)?, NestedPayload::Struct(_));
                for (index, value) in values.iter().enumerate() {
                    self.key_child(if structure { index } else { 0 }, value, output, query)?;
                }
            }
            NestedPayload::Map(entries) => {
                output.extend_from_slice(&(entries.len() as u64).to_le_bytes())?;
                for (key, value) in entries {
                    self.key_child(0, key, output, query)?;
                    self.key_child(1, value, output, query)?;
                }
            }
            NestedPayload::Union { tag, value } => {
                output.extend_from_slice(&(*tag as u64).to_le_bytes())?;
                self.key_child(*tag, value, output, query)?;
            }
            NestedPayload::Variant { .. } => {
                return Err(Error::Unsupported(
                    "VARIANT keys are not integrated yet".into(),
                ));
            }
        }
        Ok(())
    }
}
