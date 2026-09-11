//! Exact canonical VARIANT content, without allocating a normalized value tree.
//! Native tags, ordered object members and scalar payload bits are significant;
//! SQL numeric comparison, casts and grouping keys are deliberately not used.
use super::*;
use crate::{
    common::{type_registry::BoundType, variant::Node},
    parallel::QueryContext,
};

#[cfg(test)]
mod tests;

struct Budget {
    nodes: usize,
    bytes: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Budget {
    fn visit(&mut self, depth: usize, query: &QueryContext) -> Result<()> {
        query.check()?;
        if depth > 64 {
            return Err(Error::Resource("exact VARIANT depth exceeds 64".into()));
        }
        self.nodes = self
            .nodes
            .checked_sub(1)
            .ok_or_else(|| Error::Resource("exact VARIANT exceeds 16 million visits".into()))?;
        Ok(())
    }
    fn bytes(&mut self, count: usize) -> Result<()> {
        self.bytes = self.bytes.checked_sub(count).ok_or_else(|| {
            Error::Resource("exact VARIANT exceeds 64 MiB variable scalar/key bytes".into())
        })?;
        Ok(())
    }
}

/// Validate both roots with the retained selected VARIANT adapter, then compare
/// only the canonical native content. No caller's ambient type registry is used.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn equivalent(
    left: &Value,
    right: &Value,
    bound: &BoundType,
    query: &QueryContext,
) -> Result<bool> {
    equivalent_with_budget(
        left,
        right,
        bound,
        query,
        &mut Budget {
            nodes: 16_777_216,
            bytes: 64 * 1024 * 1024,
        },
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn equivalent_with_budget(
    left: &Value,
    right: &Value,
    bound: &BoundType,
    query: &QueryContext,
    budget: &mut Budget,
) -> Result<bool> {
    query.check()?;
    let ty = NestedType::Variant.data_type();
    if bound.data_type() != &ty {
        return Err(Error::Internal(
            "exact VARIANT comparison requires its selected type".into(),
        ));
    }
    bound.validate(left, query)?;
    bound.validate(right, query)?;
    // In particular, a non-NULL envelope around VARIANT_NULL is not a second
    // representation of root SQL NULL, even under a permissive replacement.
    let result = compare(
        Node::Typed(&ty, left),
        Node::Typed(&ty, right),
        0,
        0,
        budget,
        query,
    )?;
    if (!left.is_null()
        && matches!(
            resolve(Node::Typed(&ty, left), 0, budget, query)?.0,
            Node::Typed(_, Value::Null)
        ))
        || (!right.is_null()
            && matches!(
                resolve(Node::Typed(&ty, right), 0, budget, query)?.0,
                Node::Typed(_, Value::Null)
            ))
    {
        return Err(Error::Conversion(
            "native VARIANT root NULL requires row validity".into(),
        ));
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn resolve<'a>(
    mut node: Node<'a>,
    mut depth: usize,
    budget: &mut Budget,
    query: &QueryContext,
) -> Result<(Node<'a>, usize)> {
    loop {
        budget.visit(depth, query)?;
        node = match node {
            Node::Typed(DataType::Nested(metadata), Value::Nested(value)) => {
                match (metadata.as_ref(), &value.payload) {
                    (NestedType::Variant, NestedPayload::Variant { data_type, value }) => {
                        Node::Typed(data_type, value)
                    }
                    (NestedType::Union(fields), NestedPayload::Union { tag, value }) => {
                        Node::Typed(&fields.get(*tag).ok_or_else(shape)?.1, value)
                    }
                    _ => return Ok((node, depth)),
                }
            }
            _ => return Ok((node, depth)),
        };
        depth += 1;
    }
}

enum Array<'a> {
    Sequence(&'a DataType, &'a [Value]),
    Tuple(&'a [DataType], &'a [Value]),
    Map(&'a DataType, &'a DataType, &'a [(Value, Value)]),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> Array<'a> {
    fn from(node: Node<'a>) -> Result<Option<Self>> {
        let Node::Typed(DataType::Nested(metadata), Value::Nested(value)) = node else {
            return Ok(None);
        };
        Ok(Some(match (metadata.as_ref(), &value.payload) {
            (NestedType::List(child), NestedPayload::Sequence(values)) => {
                Self::Sequence(child, values)
            }
            (NestedType::Array { element, length }, NestedPayload::Sequence(values))
                if *length == values.len() =>
            {
                Self::Sequence(element, values)
            }
            (NestedType::Tuple(fields), NestedPayload::Struct(values))
                if fields.len() == values.len() =>
            {
                Self::Tuple(fields, values)
            }
            (NestedType::Map { key, value }, NestedPayload::Map(entries)) => {
                Self::Map(key, value, entries)
            }
            (NestedType::Struct(_) | NestedType::Object(_), NestedPayload::Struct(_)) => {
                return Ok(None);
            }
            _ => return Err(shape()),
        }))
    }
    fn len(&self) -> usize {
        match self {
            Self::Sequence(_, values) | Self::Tuple(_, values) => values.len(),
            Self::Map(_, _, values) => values.len(),
        }
    }
    fn child(&self, index: usize) -> Node<'a> {
        match self {
            Self::Sequence(ty, values) => Node::Typed(ty, &values[index]),
            Self::Tuple(fields, values) => Node::Typed(&fields[index], &values[index]),
            Self::Map(key, value, entries) => {
                Node::MapEntry(key, &entries[index].0, value, &entries[index].1)
            }
        }
    }
}

enum Object<'a> {
    Fields(&'a [(String, DataType)], &'a [Value]),
    Entry(&'a DataType, &'a Value, &'a DataType, &'a Value),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> Object<'a> {
    fn from(node: Node<'a>) -> Result<Option<Self>> {
        Ok(match node {
            Node::MapEntry(kt, k, vt, v) => Some(Self::Entry(kt, k, vt, v)),
            Node::Typed(DataType::Nested(metadata), Value::Nested(value)) => {
                match (metadata.as_ref(), &value.payload) {
                    (
                        NestedType::Struct(fields) | NestedType::Object(fields),
                        NestedPayload::Struct(values),
                    ) if fields.len() == values.len() => Some(Self::Fields(fields, values)),
                    _ => return Err(shape()),
                }
            }
            _ => None,
        })
    }
    fn len(&self) -> usize {
        match self {
            Self::Fields(fields, _) => fields.len(),
            Self::Entry(..) => 2,
        }
    }
    fn member(&self, index: usize) -> (&'a str, Node<'a>) {
        match self {
            Self::Fields(fields, values) => (
                &fields[index].0,
                Node::Typed(&fields[index].1, &values[index]),
            ),
            Self::Entry(kt, k, _, _) if index == 0 => ("key", Node::Typed(kt, k)),
            Self::Entry(_, _, vt, v) => ("value", Node::Typed(vt, v)),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare(
    left: Node<'_>,
    right: Node<'_>,
    left_depth: usize,
    right_depth: usize,
    budget: &mut Budget,
    query: &QueryContext,
) -> Result<bool> {
    let (left, left_depth) = resolve(left, left_depth, budget, query)?;
    let (right, right_depth) = resolve(right, right_depth, budget, query)?;
    let null = |node| matches!(node, Node::Typed(_, Value::Null));
    if null(left) || null(right) {
        return Ok(null(left) && null(right));
    }
    match (Array::from(left)?, Array::from(right)?) {
        (Some(left), Some(right)) => {
            if left.len() != right.len() {
                return Ok(false);
            }
            for index in 0..left.len() {
                if !compare(
                    left.child(index),
                    right.child(index),
                    left_depth + 1,
                    right_depth + 1,
                    budget,
                    query,
                )? {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        (Some(_), None) | (None, Some(_)) => return Ok(false),
        _ => {}
    }
    match (Object::from(left)?, Object::from(right)?) {
        (Some(left), Some(right)) => {
            if left.len() != right.len() {
                return Ok(false);
            }
            for index in 0..left.len() {
                let (lk, lv) = left.member(index);
                let (rk, rv) = right.member(index);
                if !bytes_equal(lk.as_bytes(), rk.as_bytes(), budget, query)?
                    || !compare(lv, rv, left_depth + 1, right_depth + 1, budget, query)?
                {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        (Some(_), None) | (None, Some(_)) => return Ok(false),
        _ => {}
    }
    scalar(left, right, budget, query)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bytes_equal(
    left: &[u8],
    right: &[u8],
    budget: &mut Budget,
    query: &QueryContext,
) -> Result<bool> {
    budget.bytes(left.len())?;
    budget.bytes(right.len())?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.chunks(4096).zip(right.chunks(4096)) {
        query.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn scalar(
    left: Node<'_>,
    right: Node<'_>,
    budget: &mut Budget,
    query: &QueryContext,
) -> Result<bool> {
    let (Node::Typed(lt, lv), Node::Typed(rt, rv)) = (left, right) else {
        return Err(shape());
    };
    let string = |ty: &DataType| matches!(ty, DataType::Varchar | DataType::Enum(_));
    if string(lt) && string(rt) {
        return bytes_equal(text(lv)?.as_bytes(), text(rv)?.as_bytes(), budget, query);
    }
    if lt != rt {
        return Ok(false);
    }
    Ok(match (lt, lv, rv) {
        (DataType::Float, Value::Float(left), Value::Float(right)) => {
            left.to_bits() == right.to_bits()
        }
        (DataType::Double, Value::Double(left), Value::Double(right)) => {
            left.to_bits() == right.to_bits()
        }
        (DataType::Blob, Value::Blob(left), Value::Blob(right)) => {
            bytes_equal(left, right, budget, query)?
        }
        (DataType::Bit, Value::Bit(left), Value::Bit(right)) => {
            left.length() == right.length()
                && bytes_equal(left.bytes(), right.bytes(), budget, query)?
        }
        (DataType::Bignum, Value::Bignum(left), Value::Bignum(right)) => {
            budget.bytes(left.byte_len())?;
            budget.bytes(right.byte_len())?;
            // Canonical sign/magnitude equality, including negative zero; this
            // is not VARIANT's normalized cross-category numeric comparison.
            left.compare(right, || query.check())?.is_eq()
        }
        (DataType::Boolean, Value::Boolean(_), Value::Boolean(_))
        | (DataType::Uuid, Value::Uuid(_), Value::Uuid(_))
        | (DataType::Date, Value::Date(_), Value::Date(_))
        | (DataType::Decimal { .. }, Value::Decimal { .. }, Value::Decimal { .. }) => lv == rv,
        (ty, Value::Integer(_), Value::Integer(_)) if ty.is_signed_integer() => lv == rv,
        (ty, Value::Unsigned(_), Value::Unsigned(_)) if ty.is_unsigned_integer() => lv == rv,
        (ty, Value::Temporal(_), Value::Temporal(_)) if ty.is_temporal() => lv == rv,
        (DataType::Extension(_), _, _) => {
            return Err(Error::Unsupported(
                "exact native VARIANT extension category".into(),
            ));
        }
        _ => return Err(shape()),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn text(value: &Value) -> Result<&str> {
    match value {
        Value::Varchar(value) => Ok(value),
        Value::Enum(value) => value.label(),
        _ => Err(shape()),
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn shape() -> Error {
    Error::Conversion("exact native VARIANT shape mismatch".into())
}
