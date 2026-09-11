use std::{collections::HashSet, sync::Arc};

use crate::{
    common::{
        Error, Result, Value,
        type_registry::{BoundType, KeyRepresentation},
        vector::Vector,
    },
    parallel::QueryContext,
};

enum BuildKeys {
    Bytes(HashSet<Vec<u8>>),
    Integers(HashSet<i128>),
}

/// A per-cursor equality set. The selected type adapter owns key semantics;
/// integer identity is an explicit capability, never a concrete-adapter check.
pub(super) struct MembershipBuilder {
    data_type: Arc<BoundType>,
    keys: BuildKeys,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl MembershipBuilder {
    pub fn new(data_type: Arc<BoundType>) -> Self {
        let keys = match data_type.key_representation() {
            KeyRepresentation::CanonicalBytes => BuildKeys::Bytes(HashSet::new()),
            KeyRepresentation::Integer | KeyRepresentation::NumericCoefficient => {
                BuildKeys::Integers(HashSet::new())
            }
        };
        Self { data_type, keys }
    }

    pub fn insert(&mut self, input: &Vector, context: &QueryContext) -> Result<()> {
        match &mut self.keys {
            BuildKeys::Bytes(keys) => {
                keys.try_reserve(input.len()).map_err(allocation_error)?;
                self.data_type.for_each_key(input, context, |_, key| {
                    if let Some(key) = key {
                        keys.insert(key.to_vec());
                    }
                    Ok(())
                })
            }
            BuildKeys::Integers(keys) => {
                self.data_type.validate_vector(input, context)?;
                keys.try_reserve(input.len()).map_err(allocation_error)?;
                visit_integers(
                    input,
                    self.data_type.key_representation(),
                    context,
                    |_, key| {
                        if let Some(key) = key {
                            keys.insert(key);
                        }
                    },
                )
            }
        }
    }

    /// Seal the build before probing. Compact domains use at most 128 KiB and
    /// at most eight bytes per distinct key; sparse/wide domains retain hashing.
    pub fn finish(self, context: &QueryContext) -> Result<MembershipIndex> {
        context.check()?;
        let keys = match self.keys {
            BuildKeys::Bytes(keys) => ProbeKeys::Bytes(keys),
            BuildKeys::Integers(keys) => compact(keys, context)?,
        };
        Ok(MembershipIndex {
            data_type: self.data_type,
            keys,
        })
    }
}

enum ProbeKeys {
    Bytes(HashSet<Vec<u8>>),
    Integers(HashSet<i128>),
    Dense { minimum: i128, bits: Vec<u64> },
}

pub(super) struct MembershipIndex {
    data_type: Arc<BoundType>,
    keys: ProbeKeys,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl MembershipIndex {
    pub fn is_empty(&self) -> bool {
        match &self.keys {
            ProbeKeys::Bytes(keys) => keys.is_empty(),
            ProbeKeys::Integers(keys) => keys.is_empty(),
            ProbeKeys::Dense { .. } => false,
        }
    }

    /// Return logical positions in input order. NULL never matches, duplicate
    /// build keys have no effect, and anti selection includes NULL probe rows.
    pub fn select(
        &self,
        input: &Vector,
        matched: bool,
        context: &QueryContext,
    ) -> Result<Vec<usize>> {
        let mut selected = Vec::new();
        selected
            .try_reserve(input.len())
            .map_err(allocation_error)?;
        match &self.keys {
            ProbeKeys::Bytes(keys) => {
                self.data_type.for_each_key(input, context, |row, key| {
                    if key.is_some_and(|key| keys.contains(key)) == matched {
                        selected.push(row);
                    }
                    Ok(())
                })?;
            }
            ProbeKeys::Integers(keys) => {
                self.data_type.validate_vector(input, context)?;
                visit_integers(
                    input,
                    self.data_type.key_representation(),
                    context,
                    |row, key| {
                        if key.is_some_and(|key| keys.contains(&key)) == matched {
                            selected.push(row);
                        }
                    },
                )?;
            }
            ProbeKeys::Dense { minimum, bits } => {
                self.data_type.validate_vector(input, context)?;
                visit_integers(
                    input,
                    self.data_type.key_representation(),
                    context,
                    |row, key| {
                        let offset = key
                            .and_then(|key| key.checked_sub(*minimum))
                            .and_then(|offset| usize::try_from(offset).ok());
                        let found = offset.is_some_and(|offset| {
                            bits.get(offset / 64)
                                .is_some_and(|word| word & (1 << (offset % 64)) != 0)
                        });
                        if found == matched {
                            selected.push(row);
                        }
                    },
                )?;
            }
        }
        Ok(selected)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compact(keys: HashSet<i128>, context: &QueryContext) -> Result<ProbeKeys> {
    let mut minimum = i128::MAX;
    let mut maximum = i128::MIN;
    for (index, &key) in keys.iter().enumerate() {
        if index % 1024 == 0 {
            context.check()?;
        }
        minimum = minimum.min(key);
        maximum = maximum.max(key);
    }
    let width = maximum
        .checked_sub(minimum)
        .and_then(|width| width.checked_add(1))
        .and_then(|width| usize::try_from(width).ok())
        .filter(|&width| width <= 1024 * 1024 && width <= keys.len().saturating_mul(64));
    let Some(width) = width else {
        return Ok(ProbeKeys::Integers(keys));
    };
    let mut bits = Vec::new();
    bits.try_reserve_exact(width.div_ceil(64))
        .map_err(allocation_error)?;
    bits.resize(width.div_ceil(64), 0);
    for (index, key) in keys.into_iter().enumerate() {
        if index % 1024 == 0 {
            context.check()?;
        }
        let offset = (key - minimum) as usize;
        bits[offset / 64] |= 1 << (offset % 64);
    }
    context.check()?;
    Ok(ProbeKeys::Dense { minimum, bits })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn allocation_error(_: std::collections::TryReserveError) -> Error {
    Error::Resource("cannot allocate join membership keys".into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn visit_integers(
    input: &Vector,
    representation: KeyRepresentation,
    context: &QueryContext,
    visit: impl FnMut(usize, Option<i128>),
) -> Result<()> {
    fn values<'a>(
        values: impl Iterator<Item = &'a Value>,
        representation: KeyRepresentation,
        context: &QueryContext,
        mut visit: impl FnMut(usize, Option<i128>),
    ) -> Result<()> {
        if representation == KeyRepresentation::Integer {
            // The selected capability and caller's logical validation prove
            // signed storage. Decode once per row without capability dispatch.
            for (row, value) in values.enumerate() {
                if row % 1024 == 0 {
                    context.check()?;
                }
                let key = match value {
                    Value::Integer(value) => Some(*value),
                    Value::Null => None,
                    _ => unreachable!("validated signed membership key"),
                };
                visit(row, key);
            }
            return context.check();
        }
        for (row, value) in values.enumerate() {
            if row % 1024 == 0 {
                context.check()?;
            }
            let key = representation.integer_key(value)?;
            visit(row, key);
        }
        context.check()
    }
    if let Some(flat) = input.flat_values() {
        values(flat.iter(), representation, context, visit)
    } else {
        values(input.values(), representation, context, visit)
    }
}
