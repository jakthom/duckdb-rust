//! Typed Value metadata bridge. Unlike a row-vector boundary, sibling VARIANTs
//! share both remaining materialization bytes and logical visits with callers.
use super::*;
use crate::{common::type_registry::BoundType, parallel::QueryContext};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn encode(
    value: &Value,
    selected: &BoundType,
    depth: usize,
    nodes: &mut usize,
    bytes: &mut usize,
    query: &QueryContext,
) -> Result<Value> {
    let mut limits = encoding::Limits {
        nodes: *nodes,
        bytes: *bytes,
    };
    let mut result = encoding::encode_with_limits(
        std::slice::from_ref(value),
        selected,
        query,
        &mut limits,
        depth,
    )?;
    *nodes = limits.nodes;
    *bytes = limits.bytes;
    result
        .pop()
        .ok_or_else(|| Error::Internal("empty canonical VARIANT literal".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn decode(
    value: &Value,
    selected: &BoundType,
    depth: usize,
    nodes: &mut usize,
    bytes: &mut usize,
    query: &QueryContext,
) -> Result<Value> {
    query.check()?;
    if selected.data_type() != &NestedType::Variant.data_type() {
        return Err(Error::Internal(
            "selected literal VARIANT type mismatch".into(),
        ));
    }
    let mut budget = payload::Budget::with_context(*nodes, query);
    budget.bytes = *bytes;
    let physical = payload::Unshredded::new(value, &mut budget)?
        .ok_or_else(|| corrupt("non-NULL VARIANT literal has NULL physical value"))?;
    let child = physical.decode_from(0, depth, &mut budget)?;
    if child.1.is_null() {
        return Err(corrupt("VARIANT literal root NULL requires is_null flag"));
    }
    let result = payload::envelope(child)?;
    selected.validate(&result, query)?;
    *nodes = budget.nodes;
    *bytes = budget.bytes;
    Ok(result)
}
