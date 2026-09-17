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
                if let Some(value) = representation.integer_key(&value)? {
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
        if let [column] = columns {
            self.grow_monotonic_dense(column, representations[0], query)?;
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
                        [representations[0].integer_key(&value)?],
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
            [column] if representations[0] == KeyRepresentation::Integer => {
                if let Some(values) = column.flat_bigints() {
                    // The flat all-valid BIGINT lane is already a signed
                    // coefficient sequence; avoid reconstructing Value for
                    // every row before compact lookup.
                    self.locate_coefficients(
                        values
                            .iter()
                            .copied()
                            .map(|value| Ok([Some(i128::from(value))])),
                        rows,
                        query,
                        create,
                    )
                } else {
                    self.locate_values(
                        column.values().map(|value| [value]),
                        representations,
                        rows,
                        query,
                        create,
                    )
                }
            }
            [column]
                if representations[0] == KeyRepresentation::NumericCoefficient
                    && column.data_type().is_unsigned_integer() =>
            {
                // Select the declared physical mapping outside the hot loop.
                // The capability remains separate from signed ordering.
                let coefficient = |value: Value| match value {
                    Value::Unsigned(value) => Ok([Some(value as i128)]),
                    Value::Null => Ok([None]),
                    _ => Err(Error::Internal("unsigned grouping key type".into())),
                };
                if let Some(values) = column.flat_values() {
                    self.locate_coefficients(
                        values.iter().cloned().map(coefficient),
                        rows,
                        query,
                        create,
                    )
                } else {
                    self.locate_coefficients(column.values().map(coefficient), rows, query, create)
                }
            }
            [column] => match column.flat_values() {
                Some(values) => self.locate_values(
                    values.iter().cloned().map(|v| [v]),
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
                    a.iter()
                        .cloned()
                        .zip(b.iter().cloned())
                        .map(|(a, b)| [a, b]),
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
    /// Extend a one-dimensional dense range only when existing positions stay
    /// unchanged. Later high keys otherwise fall into the sparse map after a
    /// small first batch, which is needlessly expensive for monotonic scans.
    fn grow_monotonic_dense(
        &mut self,
        column: &Vector,
        representation: KeyRepresentation,
        query: &QueryContext,
    ) -> Result<()> {
        if !self.sparse.is_empty() {
            return Ok(());
        }
        let Some(dense) = self.dense.as_mut() else {
            return Ok(());
        };
        let dimension = dense.dimensions[0];
        let Some(minimum) = dimension.minimum else {
            // All-NULL first batches have no stable numeric origin.
            return Ok(());
        };
        let (batch_min, batch_max) = if representation == KeyRepresentation::Integer
            && let Some(values) = column.flat_bigints()
        {
            // This encoding is all-valid by construction. Scan its borrowed
            // signed lane directly rather than materializing `Value` and
            // rediscovering the already-declared integer representation.
            let mut batch_min = None;
            let mut batch_max = None;
            for (index, &value) in values.iter().enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                let value = i128::from(value);
                batch_min = Some(batch_min.map_or(value, |current: i128| current.min(value)));
                batch_max = Some(batch_max.map_or(value, |current: i128| current.max(value)));
            }
            (batch_min, batch_max)
        } else {
            // Keep every other physical shape on the general representation
            // path: nullable, selected and non-BIGINT vectors retain their
            // existing conversion and NULL behavior.
            let mut batch_min = None;
            let mut batch_max = None;
            for (index, value) in column.values().enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                if let Some(value) = representation.integer_key(&value)? {
                    batch_min = Some(batch_min.map_or(value, |current: i128| current.min(value)));
                    batch_max = Some(batch_max.map_or(value, |current: i128| current.max(value)));
                }
            }
            (batch_min, batch_max)
        };
        let Some(batch_min) = batch_min else {
            return Ok(());
        };
        let Some(batch_max) = batch_max else {
            return Ok(());
        };
        if batch_min < minimum {
            return Ok(());
        }
        let Some(width) = batch_max
            .checked_sub(minimum)
            .and_then(|width| usize::try_from(width).ok())
            .and_then(|width| width.checked_add(1))
        else {
            return Ok(());
        };
        if width <= dimension.values || width.saturating_add(1) > MAX_DENSE_SLOTS {
            return Ok(());
        }
        // The old NULL slot follows the numeric range. Move it to the new
        // tail before the former slot becomes a valid numeric position.
        let null = dense.slots[dimension.values];
        dense.slots.resize(width + 1, EMPTY);
        dense.slots[dimension.values] = EMPTY;
        dense.slots[width] = null;
        dense.dimensions[0].values = width;
        query.check()
    }
    #[inline]
    fn locate_values<const N: usize>(
        &mut self,
        values: impl Iterator<Item = [Value; N]>,
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
                    Value::Integer(value) => Some(value),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::DataType;

    fn locate_one_column(
        index: &mut IntegerIndex,
        column: &Vector,
        next_group: &mut usize,
        query: &QueryContext,
    ) -> Result<Vec<usize>> {
        index.locate(
            &[column],
            &[KeyRepresentation::Integer],
            column.len(),
            query,
            |_| {
                let group = *next_group;
                *next_group += 1;
                Ok(group)
            },
        )
    }

    #[test]
    fn dense_growth_keeps_null_slot_across_flat_bigint_batches() -> Result<()> {
        let query = QueryContext::background();
        let first = Vector::flat(
            DataType::BigInt,
            vec![Value::Integer(0), Value::Integer(1), Value::Null],
        )?;
        let second = Vector::flat(DataType::BigInt, vec![Value::Integer(2), Value::Integer(3)])?;
        let third = Vector::flat(DataType::BigInt, vec![Value::Null])?;
        assert!(first.flat_bigints().is_none());
        assert!(second.flat_bigints().is_some());
        let mut index = IntegerIndex::default();
        let mut next_group = 0;
        assert_eq!(
            locate_one_column(&mut index, &first, &mut next_group, &query)?,
            vec![0, 1, 2]
        );
        assert_eq!(
            locate_one_column(&mut index, &second, &mut next_group, &query)?,
            vec![3, 4]
        );
        assert_eq!(
            locate_one_column(&mut index, &third, &mut next_group, &query)?,
            vec![2]
        );
        Ok(())
    }

    #[test]
    fn dense_growth_keeps_nullable_bigint_on_generic_path() -> Result<()> {
        let query = QueryContext::background();
        let first = Vector::flat(DataType::BigInt, vec![Value::Integer(0), Value::Null])?;
        let second = Vector::flat(DataType::BigInt, vec![Value::Integer(1), Value::Null])?;
        assert!(first.flat_bigints().is_none());
        let mut index = IntegerIndex::default();
        let mut next_group = 0;
        assert_eq!(
            locate_one_column(&mut index, &first, &mut next_group, &query)?,
            vec![0, 1]
        );
        assert_eq!(
            locate_one_column(&mut index, &second, &mut next_group, &query)?,
            vec![2, 1]
        );
        Ok(())
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
