use std::{cmp::Ordering, collections::BTreeSet, sync::Arc};

use super::{
    BoundType, BoundValidationIdentity, KeyContext, KeyWriter, OrderingRepresentation, TypeAdapter,
    TypeAdapterAccess, TypeRegistry, ValueValidation,
};
use crate::{
    common::{
        DataType, Error, NestedPayload, NestedType, Result, Value,
        vector::{NestedRowRef, Vector},
    },
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct NestedTypes {
    children: Vec<BoundType>,
    validation: ValueValidation,
}

impl Default for NestedTypes {
    fn default() -> Self {
        Self {
            children: Vec::new(),
            validation: ValueValidation::Logical,
        }
    }
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
    fn compare_child_validated(
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
            (false, false) => self.child(index)?.compare_validated(left, right, query),
        }
    }
    fn compare_payload(
        &self,
        left: &Value,
        right: &Value,
        validated_children: bool,
        query: &QueryContext,
    ) -> Result<Ordering> {
        let compare_child = |index, left, right| {
            if validated_children {
                self.compare_child_validated(index, left, right, query)
            } else {
                self.compare_child(index, left, right, query)
            }
        };
        let order = match (self.payload(left)?, self.payload(right)?) {
            (NestedPayload::Sequence(a), NestedPayload::Sequence(b))
            | (NestedPayload::Struct(a), NestedPayload::Struct(b)) => {
                let structure = matches!(self.payload(left)?, NestedPayload::Struct(_));
                for (index, (a, b)) in a.iter().zip(b).enumerate() {
                    let order = compare_child(if structure { index } else { 0 }, a, b)?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                a.len().cmp(&b.len())
            }
            (NestedPayload::Map(a), NestedPayload::Map(b)) => {
                for ((ak, av), (bk, bv)) in a.iter().zip(b) {
                    for (index, a, b) in [(0, ak, bk), (1, av, bv)] {
                        let order = compare_child(index, a, b)?;
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
                    compare_child(*a, av, bv)?
                }
            }
            _ => return Err(Error::Unsupported("nested comparison payload".into())),
        };
        Ok(order)
    }

    fn compare_columnar_rows(
        &self,
        metadata: &NestedType,
        left: NestedRowRef<'_>,
        right: NestedRowRef<'_>,
        query: &QueryContext,
    ) -> Result<Option<Ordering>> {
        match (left, right) {
            (NestedRowRef::Null, _) | (_, NestedRowRef::Null) => Ok(None),
            (
                NestedRowRef::Struct {
                    children: left,
                    index: left_row,
                },
                NestedRowRef::Struct {
                    children: right,
                    index: right_row,
                },
            ) => {
                let NestedType::Struct(fields) = metadata else {
                    return Err(Error::Internal("columnar STRUCT metadata".into()));
                };
                if left.len() != fields.len() || right.len() != fields.len() {
                    return Err(Error::Internal("columnar STRUCT width differs".into()));
                }
                for (index, (left, right)) in left.iter().zip(right).enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    let order =
                        self.compare_vector_child(index, left, left_row, right, right_row, query)?;
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                }
                Ok(Some(Ordering::Equal))
            }
            (
                NestedRowRef::List {
                    child: left,
                    range: left_range,
                },
                NestedRowRef::List {
                    child: right,
                    range: right_range,
                },
            ) => {
                if !matches!(metadata, NestedType::List(_)) {
                    return Err(Error::Internal("columnar LIST metadata".into()));
                }
                for (position, (left_row, right_row)) in
                    left_range.clone().zip(right_range.clone()).enumerate()
                {
                    if position % 1024 == 0 {
                        query.check()?;
                    }
                    let order =
                        self.compare_vector_child(0, left, left_row, right, right_row, query)?;
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                }
                Ok(Some(left_range.len().cmp(&right_range.len())))
            }
            (left, right) => {
                let (structure, child_count) = match metadata {
                    NestedType::Struct(fields) => (true, fields.len()),
                    NestedType::List(_) => (false, usize::MAX),
                    _ => {
                        return Err(Error::Internal(
                            "columnar nested comparison metadata".into(),
                        ));
                    }
                };
                let left = NestedElements::new(left, structure)?;
                let right = NestedElements::new(right, structure)?;
                if structure && (left.len() != child_count || right.len() != child_count) {
                    return Err(Error::Internal("columnar STRUCT width differs".into()));
                }
                for index in 0..left.len().min(right.len()) {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    let child = if structure { index } else { 0 };
                    let order =
                        self.compare_element(child, left.get(index)?, right.get(index)?, query)?;
                    if order != Ordering::Equal {
                        return Ok(Some(order));
                    }
                }
                Ok(Some(left.len().cmp(&right.len())))
            }
        }
    }

    fn compare_vector_child(
        &self,
        child: usize,
        left: &Vector,
        left_index: usize,
        right: &Vector,
        right_index: usize,
        query: &QueryContext,
    ) -> Result<Ordering> {
        let bound = self.child(child)?;
        if bound.ordering_representation() == OrderingRepresentation::VarcharBytes {
            let left = left
                .varchar_at_validated(left_index)
                .ok_or_else(|| Error::Internal("validated VARCHAR comparison input".into()))?;
            let right = right
                .varchar_at_validated(right_index)
                .ok_or_else(|| Error::Internal("validated VARCHAR comparison input".into()))?;
            return Ok(match (left, right) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            });
        }
        bound.compare_vector_at_validated(left, left_index, right, right_index, query)
    }

    fn compare_element(
        &self,
        child: usize,
        left: NestedElement<'_>,
        right: NestedElement<'_>,
        query: &QueryContext,
    ) -> Result<Ordering> {
        let bound = self.child(child)?;
        match (left, right) {
            (NestedElement::Vector(left, li), NestedElement::Vector(right, ri)) => {
                self.compare_vector_child(child, left, li, right, ri, query)
            }
            (NestedElement::Vector(column, index), NestedElement::Value(value)) => {
                bound.compare_vector_value_at_validated(column, index, value, true, query)
            }
            (NestedElement::Value(value), NestedElement::Vector(column, index)) => {
                bound.compare_vector_value_at_validated(column, index, value, false, query)
            }
            (NestedElement::Value(left), NestedElement::Value(right)) => {
                self.compare_child_validated(child, left, right, query)
            }
        }
    }
    fn key_child(
        &self,
        index: usize,
        value: &Value,
        key_context: KeyContext,
        writer: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        let mut bytes = Vec::new();
        self.child(index)?
            .append_key_with_context(value, key_context, &mut bytes, query)?;
        writer.extend_from_slice(&bytes)
    }

    fn write_nested_key(
        &self,
        value: &Value,
        key_context: KeyContext,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        match self.payload(value)? {
            NestedPayload::Sequence(values) | NestedPayload::Struct(values) => {
                output.extend_from_slice(&(values.len() as u64).to_le_bytes())?;
                let structure = matches!(self.payload(value)?, NestedPayload::Struct(_));
                for (index, value) in values.iter().enumerate() {
                    self.key_child(
                        if structure { index } else { 0 },
                        value,
                        key_context,
                        output,
                        query,
                    )?;
                }
            }
            NestedPayload::Map(entries) => {
                output.extend_from_slice(&(entries.len() as u64).to_le_bytes())?;
                for (key, value) in entries {
                    self.key_child(0, key, key_context, output, query)?;
                    self.key_child(1, value, key_context, output, query)?;
                }
            }
            NestedPayload::Union { tag, value } => {
                output.extend_from_slice(&(*tag as u64).to_le_bytes())?;
                self.key_child(*tag, value, key_context, output, query)?;
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for NestedTypes {
    #[allow(private_interfaces)]
    fn bound_validation_identity(&self, _: TypeAdapterAccess) -> Option<BoundValidationIdentity> {
        self.children
            .iter()
            .all(BoundType::has_reusable_builtin_validation)
            .then_some(BoundValidationIdentity::RecursiveNested)
    }
    fn supports_index(&self, _: &DataType) -> bool {
        false
    }
    fn value_validation(&self) -> ValueValidation {
        self.validation
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
        let children: Vec<BoundType> = metadata
            .children()
            .into_iter()
            .map(|child| types.bind(child))
            .collect::<Result<_>>()?;
        let validation = if matches!(
            metadata.as_ref(),
            NestedType::Map { .. } | NestedType::Variant
        ) || children.iter().any(BoundType::requires_logical_validation)
        {
            ValueValidation::Logical
        } else {
            // `NestedValue::fits_type`, enforced by every Vector constructor,
            // proves list/array cardinality, STRUCT/TUPLE shape, UNION tag and
            // the recursively declared physical child types. Only MAP key
            // uniqueness, unsupported VARIANT payloads and selected logical
            // child adapters need another pass.
            ValueValidation::Physical
        };
        Ok(Some(Arc::new(Self {
            children,
            validation,
        })))
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
                    if (name.is_empty() && matches!(metadata.as_ref(), NestedType::Union(_)))
                        || !names.insert(name.to_ascii_lowercase())
                    {
                        return Err(Error::Bind(
                            "nested fields require unique names; UNION names must be nonempty"
                                .into(),
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
        self.compare_payload(left, right, false, query)
    }
    #[allow(private_interfaces)]
    fn compare_validated_batch(
        &self,
        _: TypeAdapterAccess,
        data_type: &DataType,
        left: &crate::common::vector::Vector,
        right: &crate::common::vector::Vector,
        query: &QueryContext,
    ) -> Option<Result<Vec<Option<Ordering>>>> {
        let DataType::Nested(metadata) = data_type else {
            return Some(Err(Error::Internal("nested batch comparison type".into())));
        };
        let supported_metadata = matches!(
            metadata.as_ref(),
            NestedType::Struct(_) | NestedType::List(_)
        );
        if supported_metadata && left.has_nested_row_access() && right.has_nested_row_access() {
            return Some((|| {
                let mut output = Vec::new();
                output.try_reserve_exact(left.len()).map_err(|_| {
                    Error::Resource("nested comparison result allocation failed".into())
                })?;
                for index in 0..left.len() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    output.push(self.compare_columnar_rows(
                        metadata,
                        left.nested_row_at(index).expect("prechecked nested row"),
                        right.nested_row_at(index).expect("prechecked nested row"),
                        query,
                    )?);
                }
                Ok(output)
            })());
        }
        // BoundType::compare_batch validated both complete vectors through this
        // exact adapter and its retained children before dispatching here.
        Some(super::batch::compare_values(left, right, query, |a, b| {
            self.compare_payload(a, b, true, query)
        }))
    }
    #[allow(private_interfaces)]
    #[allow(clippy::too_many_arguments)]
    fn compare_validated_vector_at(
        &self,
        _: TypeAdapterAccess,
        data_type: &DataType,
        left: &Vector,
        left_index: usize,
        right: &Vector,
        right_index: usize,
        query: &QueryContext,
    ) -> Option<Result<Ordering>> {
        let DataType::Nested(metadata) = data_type else {
            return Some(Err(Error::Internal("nested comparison type".into())));
        };
        if !matches!(
            metadata.as_ref(),
            NestedType::Struct(_) | NestedType::List(_)
        ) {
            return None;
        }
        let (Some(left), Some(right)) = (
            left.nested_row_at(left_index),
            right.nested_row_at(right_index),
        ) else {
            return None;
        };
        Some(
            self.compare_columnar_rows(metadata, left, right, query)
                .and_then(|order| {
                    order.ok_or_else(|| {
                        Error::Internal("validated non-NULL nested row became NULL".into())
                    })
                }),
        )
    }
    #[allow(private_interfaces)]
    #[allow(clippy::too_many_arguments)]
    fn compare_validated_vector_value_at(
        &self,
        _: TypeAdapterAccess,
        data_type: &DataType,
        column: &Vector,
        index: usize,
        value: &Value,
        column_is_left: bool,
        query: &QueryContext,
    ) -> Option<Result<Ordering>> {
        let DataType::Nested(metadata) = data_type else {
            return Some(Err(Error::Internal("nested comparison type".into())));
        };
        if !matches!(
            metadata.as_ref(),
            NestedType::Struct(_) | NestedType::List(_)
        ) {
            return None;
        }
        let Value::Nested(value) = value else {
            return None;
        };
        let column = column.nested_row_at(index)?;
        let scalar = NestedRowRef::Scalar(&value.payload);
        let (left, right) = if column_is_left {
            (column, scalar)
        } else {
            (scalar, column)
        };
        Some(
            self.compare_columnar_rows(metadata, left, right, query)
                .and_then(|order| {
                    order.ok_or_else(|| {
                        Error::Internal("validated non-NULL nested row became NULL".into())
                    })
                }),
        )
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        self.write_nested_key(value, KeyContext::Equality, output, query)
    }
    fn write_key_with_context(
        &self,
        _: &DataType,
        value: &Value,
        key_context: KeyContext,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        self.write_nested_key(value, key_context, output, query)
    }
}

enum NestedElement<'a> {
    Vector(&'a Vector, usize),
    Value(&'a Value),
}

enum NestedElements<'a> {
    ColumnarStruct {
        children: &'a [Vector],
        index: usize,
    },
    ColumnarList {
        child: &'a Vector,
        range: std::ops::Range<usize>,
    },
    Scalar(&'a [Value]),
}

impl<'a> NestedElements<'a> {
    fn new(row: NestedRowRef<'a>, structure: bool) -> Result<Self> {
        match (structure, row) {
            (true, NestedRowRef::Struct { children, index }) => {
                Ok(Self::ColumnarStruct { children, index })
            }
            (false, NestedRowRef::List { child, range }) => Ok(Self::ColumnarList { child, range }),
            (true, NestedRowRef::Scalar(NestedPayload::Struct(values)))
            | (false, NestedRowRef::Scalar(NestedPayload::Sequence(values))) => {
                Ok(Self::Scalar(values))
            }
            _ => Err(Error::Internal("nested row differs from metadata".into())),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::ColumnarStruct { children, .. } => children.len(),
            Self::ColumnarList { range, .. } => range.len(),
            Self::Scalar(values) => values.len(),
        }
    }

    fn get(&self, index: usize) -> Result<NestedElement<'a>> {
        match self {
            Self::ColumnarStruct {
                children,
                index: row,
            } => children
                .get(index)
                .map(|child| NestedElement::Vector(child, *row))
                .ok_or_else(|| Error::Internal("columnar STRUCT child index".into())),
            Self::ColumnarList { child, range } => range
                .start
                .checked_add(index)
                .filter(|index| *index < range.end)
                .map(|index| NestedElement::Vector(child, index))
                .ok_or_else(|| Error::Internal("columnar LIST child index".into())),
            Self::Scalar(values) => values
                .get(index)
                .map(NestedElement::Value)
                .ok_or_else(|| Error::Internal("scalar nested child index".into())),
        }
    }
}
