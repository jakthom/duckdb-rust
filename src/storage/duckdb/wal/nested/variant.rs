//! VARIANT uses the ordinary STRUCT vector envelope over its four canonical
//! unshredded children. Both pinned cores use Vector::{Serialize,Deserialize};
//! development's new string-vector encoding is handled by the selected vector
//! reader. No extra logical STRUCT layer appears in the WAL wire representation.
use super::*;
use crate::storage::duckdb::nested::variant::wal as canonical;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write(
    output: &mut Encoder,
    values: &[Value],
    depth: usize,
    remaining: &mut usize,
    context: &QueryContext,
) -> Result<()> {
    let selected = context.types().bind(&NestedType::Variant.data_type())?;
    let rows = canonical::encode(values, &selected, depth, remaining, context)?;
    let DataType::Nested(metadata) = canonical::data_type() else {
        unreachable!()
    };
    super::write(output, &metadata, &rows, depth, remaining, context)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(
    reader: &mut Reader,
    count: usize,
    validity: Option<&[u8]>,
    depth: usize,
    remaining: &mut usize,
    context: &QueryContext,
) -> Result<Vec<Value>> {
    let selected = context.types().bind(&NestedType::Variant.data_type())?;
    let physical = super::read(
        reader,
        &canonical::data_type(),
        count,
        validity,
        depth,
        remaining,
        context,
    )?;
    canonical::decode(&physical, &selected, depth, remaining, context)
        .map_err(super::super::recovery_error)
}

#[cfg(test)]
mod tests;
