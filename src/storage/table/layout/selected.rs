//! Declared-type traversal for a selected format's exact content rules.
use crate::{
    common::{
        DataType, NestedPayload, NestedType, Result, Value,
        type_registry::{BoundType, TypeRegistry},
    },
    parallel::QueryContext,
    storage::format::SnapshotFormat,
};

pub(super) struct Column {
    bound: BoundType,
    children: Vec<Column>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Column {
    pub(super) fn bind(ty: &DataType, types: &TypeRegistry, query: &QueryContext) -> Result<Self> {
        query.check()?;
        let bound = types.bind(ty)?;
        let children = if let DataType::Nested(metadata) = ty {
            metadata
                .children()
                .into_iter()
                .map(|ty| Self::bind(ty, types, query))
                .collect::<Result<_>>()?
        } else {
            Vec::new()
        };
        Ok(Self { bound, children })
    }

    fn equal(
        &self,
        left: &Value,
        right: &Value,
        format: &dyn SnapshotFormat,
        query: &QueryContext,
    ) -> Result<bool> {
        query.check()?;
        let result = format.checkpoint_value_equivalent(&self.bound, left, right, query)?;
        query.check()?;
        if let Some(equal) = result {
            return Ok(equal);
        }
        Ok(match (left, right) {
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Double(a), Value::Double(b)) => a.to_bits() == b.to_bits(),
            (Value::Nested(a), Value::Nested(b)) => {
                if a.data_type != *self.bound.data_type() || b.data_type != a.data_type {
                    return Ok(false);
                }
                let DataType::Nested(metadata) = self.bound.data_type() else {
                    return Ok(false);
                };
                match (metadata.as_ref(), &a.payload, &b.payload) {
                    (
                        NestedType::List(_) | NestedType::Array { .. },
                        NestedPayload::Sequence(a),
                        NestedPayload::Sequence(b),
                    ) => self.sequence(a, b, true, format, query)?,
                    (
                        NestedType::Struct(_) | NestedType::Tuple(_) | NestedType::Object(_),
                        NestedPayload::Struct(a),
                        NestedPayload::Struct(b),
                    ) => self.sequence(a, b, false, format, query)?,
                    (NestedType::Map { .. }, NestedPayload::Map(a), NestedPayload::Map(b)) => {
                        if a.len() != b.len() {
                            return Ok(false);
                        }
                        for ((ak, av), (bk, bv)) in a.iter().zip(b) {
                            if !self.children[0].equal(ak, bk, format, query)?
                                || !self.children[1].equal(av, bv, format, query)?
                            {
                                return Ok(false);
                            }
                        }
                        true
                    }
                    (
                        NestedType::Union(_),
                        NestedPayload::Union { tag: a, value: av },
                        NestedPayload::Union { tag: b, value: bv },
                    ) => a == b && self.children[*a].equal(av, bv, format, query)?,
                    // Without an explicit format decision VARIANT is physically
                    // exact too, including its dynamic metadata and wrappers.
                    (NestedType::Variant, _, _) => {
                        super::values::equal([left].into_iter(), [right].into_iter(), query)?
                    }
                    _ => false,
                }
            }
            _ => left == right,
        })
    }

    fn sequence(
        &self,
        left: &[Value],
        right: &[Value],
        repeated: bool,
        format: &dyn SnapshotFormat,
        query: &QueryContext,
    ) -> Result<bool> {
        if left.len() != right.len() {
            return Ok(false);
        }
        for (index, (a, b)) in left.iter().zip(right).enumerate() {
            if !self.children[if repeated { 0 } else { index }].equal(a, b, format, query)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn equal<'a>(
    left: impl Iterator<Item = &'a Value> + Clone,
    right: impl Iterator<Item = &'a Value> + Clone,
    columns: &[Column],
    format: &dyn SnapshotFormat,
    query: &QueryContext,
) -> Result<bool> {
    query.check()?;
    if left.clone().count() != columns.len() || right.clone().count() != columns.len() {
        return Ok(false);
    }
    // Preflight the complete logical row on each side before delegation. The
    // format's per-leaf codec budget must not reset the outer depth/visit limit.
    // Self-comparison uses exact float bits and never skips shared Arc children.
    super::values::equal(left.clone(), left.clone(), query)?;
    super::values::equal(right.clone(), right.clone(), query)?;
    for ((left, right), column) in left.zip(right).zip(columns) {
        column.bound.validate(left, query)?;
        column.bound.validate(right, query)?;
        if !column.equal(left, right, format, query)? {
            return Ok(false);
        }
    }
    Ok(true)
}
