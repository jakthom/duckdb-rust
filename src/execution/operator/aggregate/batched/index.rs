//! Small integer tuples use bounded direct addressing with a sparse overflow
//! map. Only an explicit, validated integer-key capability permits this path.
use crate::{
    Value,
    common::{Error, Result, type_registry::KeyRepresentation, vector::Vector},
    parallel::QueryContext,
};
use std::collections::HashMap;

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
struct Key {
    values: [i128; 2],
    nulls: u8,
}
#[derive(Clone, Copy, Default)]
struct Dimension {
    minimum: Option<i128>,
    values: usize,
}
struct Dense {
    dimensions: [Dimension; 2],
    slots: Vec<usize>,
}
const EMPTY: usize = usize::MAX;
const MAX_DENSE_SLOTS: usize = 65_536;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Dense {
    fn new(
        columns: &[&Vector],
        representations: &[KeyRepresentation],
        rows: usize,
        query: &QueryContext,
    ) -> Result<Option<Self>> {
        let mut dimensions = [Dimension::default(); 2];
        let mut count = 1usize;
        for ((dimension, column), representation) in
            dimensions.iter_mut().zip(columns).zip(representations)
        {
            let (mut minimum, mut maximum) = (None::<i128>, None::<i128>);
            for (i, value) in column.values().enumerate() {
                if i % 1024 == 0 {
                    query.check()?;
                }
                if let Some(value) = representation.integer_key(value)? {
                    minimum = Some(minimum.map_or(value, |v| v.min(value)));
                    maximum = Some(maximum.map_or(value, |v| v.max(value)));
                }
            }
            let values = match minimum.zip(maximum) {
                None => 0,
                Some((a, b)) => {
                    let Some(values) = b
                        .checked_sub(a)
                        .and_then(|v| usize::try_from(v).ok())
                        .and_then(|v| v.checked_add(1))
                    else {
                        return Ok(None);
                    };
                    values
                }
            };
            let Some(size) = values
                .checked_add(1)
                .and_then(|width| count.checked_mul(width))
            else {
                return Ok(None);
            };
            count = size;
            if count > MAX_DENSE_SLOTS || count > rows.saturating_mul(4) {
                return Ok(None);
            }
            *dimension = Dimension { minimum, values };
        }
        Ok(Some(Self {
            dimensions,
            slots: vec![EMPTY; count],
        }))
    }
    #[inline(always)]
    fn position<const N: usize>(&self, values: &[Option<i128>; N]) -> Option<usize> {
        let mut result = 0;
        for (i, &value) in values.iter().enumerate() {
            let dimension = &self.dimensions[i];
            let position = match value {
                None => dimension.values,
                Some(value) => {
                    // Dense construction proves minimum + values - 1 fits
                    // i128. Wrapped differences below that bounded width are
                    // therefore exactly the in-range offsets, even at either
                    // signed endpoint. Full-width outliers stay sparse.
                    let position = value.wrapping_sub(dimension.minimum?) as u128;
                    if position >= dimension.values as u128 {
                        return None;
                    }
                    position as usize
                }
            };
            result = result * (dimension.values + 1) + position;
        }
        Some(result)
    }
}

#[derive(Default)]
pub(super) struct IntegerIndex {
    initialized: bool,
    dense: Option<Dense>,
    sparse: HashMap<Key, usize>,
    empty: Option<usize>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IntegerIndex {
    pub(super) fn set_empty(&mut self, group: usize) {
        self.empty = Some(group);
    }
    pub(super) fn locate(
        &mut self,
        columns: &[&Vector],
        representations: &[KeyRepresentation],
        rows: usize,
        query: &QueryContext,
        create: impl FnMut(usize) -> Result<usize>,
    ) -> Result<Vec<usize>> {
        if representations.len() != columns.len()
            || representations.iter().any(|key| !key.has_integer_keys())
        {
            return Err(Error::Internal(
                "grouping keys differ from compact key capabilities".into(),
            ));
        }
        if let Some(empty) = self.empty {
            return Ok(vec![empty; rows]);
        }
        if !self.initialized {
            self.dense = Dense::new(columns, representations, rows, query)?;
            self.initialized = true;
        }
        if let [column] = columns
            && let Some((dictionary, selected)) = column.dictionary()
            && dictionary.len() <= rows / 4
        {
            // Physical dictionary identities can share total equality work.
            // Create each group at its first logical row, not parent order.
            let mut create = create;
            let mut groups = vec![EMPTY; dictionary.len()];
            let mut remaining = dictionary.len();
            let mut identity = true;
            let mut result = Vec::with_capacity(rows);
            for (row, &index) in selected.iter().enumerate() {
                if row % 1024 == 0 {
                    query.check()?;
                }
                if groups[index] == EMPTY {
                    let value = dictionary.get(index).expect("checked dictionary position");
                    groups[index] = self.locate_one(
                        [representations[0].integer_key(value)?],
                        row,
                        &mut create,
                    )?;
                    remaining -= 1;
                    identity &= groups[index] == index;
                    if remaining == 0 && identity {
                        // Every parent entry has now been observed and maps
                        // to the identical group ordinal. Remaining physical
                        // selections are already valid group destinations.
                        // Unobserved entries never create speculative groups.
                        result.push(groups[index]);
                        for block in selected[row + 1..].chunks(1024) {
                            query.check()?;
                            result.extend_from_slice(block);
                        }
                        query.check()?;
                        return Ok(result);
                    }
                }
                result.push(groups[index]);
            }
            query.check()?;
            return Ok(result);
        }
        match columns {
            [column]
                if representations[0] == KeyRepresentation::NumericCoefficient
                    && column.data_type().is_unsigned_integer() =>
            {
                // Select the declared physical mapping outside the hot loop.
                // The capability remains separate from signed ordering.
                let coefficient = |value: &Value| match value {
                    Value::Unsigned(value) => Ok([Some(*value as i128)]),
                    Value::Null => Ok([None]),
                    _ => Err(Error::Internal("unsigned grouping key type".into())),
                };
                if let Some(values) = column.flat_values() {
                    self.locate_coefficients(values.iter().map(coefficient), rows, query, create)
                } else {
                    self.locate_coefficients(column.values().map(coefficient), rows, query, create)
                }
            }
            [column] => match column.flat_values() {
                Some(values) => self.locate_values(
                    values.iter().map(|v| [v]),
                    representations,
                    rows,
                    query,
                    create,
                ),
                None => self.locate_values(
                    column.values().map(|v| [v]),
                    representations,
                    rows,
                    query,
                    create,
                ),
            },
            [a, b] => match a.flat_values().zip(b.flat_values()) {
                Some((a, b)) => self.locate_values(
                    a.iter().zip(b).map(|(a, b)| [a, b]),
                    representations,
                    rows,
                    query,
                    create,
                ),
                None => self.locate_values(
                    a.values().zip(b.values()).map(|(a, b)| [a, b]),
                    representations,
                    rows,
                    query,
                    create,
                ),
            },
            _ => Err(Error::Internal(
                "integer grouping index requires one or two columns".into(),
            )),
        }
    }
    #[inline]
    fn locate_values<'a, const N: usize>(
        &mut self,
        values: impl Iterator<Item = [&'a Value; N]>,
        representations: &[KeyRepresentation],
        rows: usize,
        query: &QueryContext,
        create: impl FnMut(usize) -> Result<usize>,
    ) -> Result<Vec<usize>> {
        if representations
            .iter()
            .all(|key| *key == KeyRepresentation::Integer)
        {
            // Decode the selected physical representation once, outside the
            // row loop. Keep small coefficient tuples in the lookup's frame.
            let coefficients = values.map(|values| {
                Ok(values.map(|value| match value {
                    Value::Integer(value) => Some(*value),
                    Value::Null => None,
                    _ => unreachable!("validated signed grouping key"),
                }))
            });
            return self.locate_coefficients(coefficients, rows, query, create);
        }
        let coefficients = values.map(|values| {
            let mut coefficients = [None; N];
            for (i, value) in values.iter().enumerate() {
                coefficients[i] = representations[i].integer_key(value)?;
            }
            Ok(coefficients)
        });
        self.locate_coefficients(coefficients, rows, query, create)
    }
    #[inline]
    fn locate_coefficients<const N: usize>(
        &mut self,
        values: impl Iterator<Item = Result<[Option<i128>; N]>>,
        rows: usize,
        query: &QueryContext,
        mut create: impl FnMut(usize) -> Result<usize>,
    ) -> Result<Vec<usize>> {
        let mut result = Vec::with_capacity(rows);
        for (row, coefficients) in values.enumerate() {
            if row % 1024 == 0 {
                query.check()?;
            }
            result.push(self.locate_one(coefficients?, row, &mut create)?);
        }
        query.check()?;
        Ok(result)
    }
    // This is the per-row hot path; an out-of-line call copies the wide
    // nullable key tuple for every row and every grouping set.
    #[inline(always)]
    fn locate_one<const N: usize>(
        &mut self,
        coefficients: [Option<i128>; N],
        row: usize,
        create: &mut impl FnMut(usize) -> Result<usize>,
    ) -> Result<usize> {
        let dense_slot = self
            .dense
            .as_mut()
            .and_then(|dense| dense.position(&coefficients).map(|i| &mut dense.slots[i]));
        if let Some(slot) = dense_slot {
            if *slot == EMPTY {
                *slot = create(row)?;
            }
            return Ok(*slot);
        }
        let mut key = Key::default();
        for (i, value) in coefficients.iter().enumerate() {
            match value {
                Some(value) => key.values[i] = *value,
                None => key.nulls |= 1 << i,
            }
        }
        Ok(match self.sparse.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
            std::collections::hash_map::Entry::Vacant(entry) => *entry.insert(create(row)?),
        })
    }
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    // One dimension is visited once; two unwind steps include loop exit.
    #[kani::unwind(2)]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn kani_dense_group_offsets_match_checked_full_width_arithmetic() {
        let minimum: i128 = kani::any();
        let values: usize = kani::any();
        let value: i128 = kani::any();
        kani::assume(values > 0 && values < MAX_DENSE_SLOTS);
        // Dense::new establishes both the bounded allocation and its
        // non-wrapping inclusive maximum. No condition restricts the key.
        kani::assume(minimum.checked_add((values - 1) as i128).is_some());
        let dense = Dense {
            dimensions: [
                Dimension {
                    minimum: Some(minimum),
                    values,
                },
                Dimension::default(),
            ],
            slots: Vec::new(),
        };
        let expected = value
            .checked_sub(minimum)
            .and_then(|offset| usize::try_from(offset).ok())
            .filter(|&offset| offset < values);
        assert_eq!(dense.position(&[Some(value)]), expected);
        assert_eq!(dense.position(&[None]), Some(values));
    }
}
