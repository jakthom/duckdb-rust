//! Publication capabilities are distinct from logical type IDs. A newer type
//! must not enter an old file through CREATE, ALTER, an empty table or a child.
use super::*;
use crate::common::{DataType, NestedType};
#[cfg(test)]
mod tests;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn new_headers(version: u64) -> Result<(u64, u64)> {
    Ok(match version {
        64 => (64, 1),
        65..=68 => (version, version - 61),
        69 => (69, 69),
        _ => {
            return Err(Error::Unsupported(format!(
                "native write storage version {version}"
            )));
        }
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn checkpoint_type(data_type: &DataType, version: u64) -> Result<()> {
    visit(data_type, |ty| {
        let required = match ty {
            DataType::Nested(metadata) => match metadata.as_ref() {
                NestedType::Tuple(_) => 69,
                NestedType::Struct(fields) if fields.is_empty() => 69,
                NestedType::Variant => 68,
                _ => 64,
            },
            _ => 64,
        };
        if version < required {
            return Err(Error::InvalidInput(format!(
                "{ty} columns require storage version {required}; database uses {version}"
            )));
        }
        primitive::type_id(ty).map(|_| ())
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn wal_type(data_type: &DataType) -> Result<()> {
    wal_type_at(data_type, None)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn wal_type_at(data_type: &DataType, version: Option<u64>) -> Result<()> {
    if let Some(version) = version {
        checkpoint_type(data_type, version)?;
    }
    visit(data_type, |ty| {
        if let DataType::Nested(metadata) = ty {
            match metadata.as_ref() {
                NestedType::Tuple(_) | NestedType::Variant if version.is_none() => {
                    return Err(Error::Unsupported(format!(
                        "native WAL publication for {ty} requires retained checkpoint capabilities"
                    )));
                }
                NestedType::Struct(fields) if fields.is_empty() && version.is_none() => {
                    return Err(Error::Unsupported(
                        "native WAL empty STRUCT requires retained checkpoint capabilities".into(),
                    ));
                }
                _ => (),
            }
        }
        primitive::type_id(ty).map(|_| ())
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn visit(data_type: &DataType, mut check: impl FnMut(&DataType) -> Result<()>) -> Result<()> {
    crate::common::type_registry::check_metadata(data_type)?;
    let mut pending = vec![data_type];
    while let Some(ty) = pending.pop() {
        check(ty)?;
        if let DataType::Nested(metadata) = ty {
            pending.extend(metadata.children());
        }
    }
    Ok(())
}
