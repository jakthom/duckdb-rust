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
            if *representation == KeyRepresentation::Integer
                && let Some(values) = column.flat_bigints()
            {
                // An all-valid BIGINT lane is already the exact signed key
                // representation. Keep construction on the borrowed lane so
                // the first batch does not rebuild Values just to find bounds.
                for (i, &value) in values.iter().enumerate() {
                    if i % 1024 == 0 {
                        query.check()?;
                    }
                    let value = i128::from(value);
                    minimum = Some(minimum.map_or(value, |v| v.min(value)));
                    maximum = Some(maximum.map_or(value, |v| v.max(value)));
                }
            } else {
                for (i, value) in column.values().enumerate() {
                    if i % 1024 == 0 {
                        query.check()?;
                    }
                    if let Some(value) = representation.integer_key(&value)? {
                        minimum = Some(minimum.map_or(value, |v| v.min(value)));
                        maximum = Some(maximum.map_or(value, |v| v.max(value)));
                    }
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
        let initialized_now = if !self.initialized {
            self.dense = Dense::new(columns, representations, rows, query)?;
            self.initialized = true;
            true
        } else {
            false
        };
        if !initialized_now
            && let [column] = columns
            && !(representations[0] == KeyRepresentation::Integer
                && ((column.flat_bigints().is_some() && !column.numeric_ascending())
                    || column
                        .dictionary()
                        .is_some_and(|(parent, _)| parent.len() <= rows / 4)))
        {
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
                    let coefficient = representations[0].integer_key(&value)?;
                    if representations[0] == KeyRepresentation::Integer
                        && let Some(value) = coefficient
                        && self.flat_dense_position(value).is_none()
                    {
                        self.grow_flat_dense_to(value, query)?;
                    }
                    groups[index] = self.locate_one([coefficient], row, &mut create)?;
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
        if let [a, b] = columns
            && let Some((parent_a, selected_a)) = a.dictionary()
            && let Some((parent_b, selected_b)) = b.dictionary()
            && let Some(slots) = parent_a.len().checked_mul(parent_b.len())
            && slots <= MAX_DENSE_SLOTS
            && slots <= rows.saturating_mul(4)
        {
            // Physical identity pairs are only a per-batch memo. Duplicate
            // parent values still converge through the selected key mapping,
            // and unobserved parent entries never create speculative groups.
            let mut cached = Vec::new();
            cached
                .try_reserve_exact(slots)
                .map_err(|_| Error::Resource("dictionary grouping allocation failed".into()))?;
            cached.resize(slots, EMPTY);
            let mut create = create;
            let mut result = Vec::with_capacity(rows);
            for (row, (&a, &b)) in selected_a.iter().zip(selected_b).enumerate() {
                if row % 1024 == 0 {
                    query.check()?;
                }
                let slot = &mut cached[a * parent_b.len() + b];
                if *slot == EMPTY {
                    let a = parent_a.get(a).expect("checked dictionary position");
                    let b = parent_b.get(b).expect("checked dictionary position");
                    *slot = self.locate_one(
                        [
                            representations[0].integer_key(&a)?,
                            representations[1].integer_key(&b)?,
                        ],
                        row,
                        &mut create,
                    )?;
                }
                result.push(*slot);
            }
            query.check()?;
            return Ok(result);
        }
        match columns {
            [column] if representations[0] == KeyRepresentation::Integer => {
                if let Some(values) = column.flat_bigints() {
                    // The flat all-valid BIGINT lane is already a signed
                    // coefficient sequence. Its direct locator avoids both
                    // reconstructing Value and the generic nullable tuple.
                    self.locate_flat_bigints(values, rows, query, create)
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
            [a, b]
                if representations == [KeyRepresentation::Integer; 2]
                    && a.flat_bigints().is_some()
                    && b.flat_bigints().is_some() =>
            {
                self.locate_flat_bigint_pairs(
                    a.flat_bigints().expect("selected flat BIGINT lane"),
                    b.flat_bigints().expect("selected flat BIGINT lane"),
                    rows,
                    query,
                    create,
                )
            }
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
            if column.numeric_ascending() {
                query.check()?;
                (
                    values.first().copied().map(i128::from),
                    values.last().copied().map(i128::from),
                )
            } else {
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
            }
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
    #[inline]
    fn locate_flat_bigints(
        &mut self,
        values: &[i64],
        rows: usize,
        query: &QueryContext,
        mut create: impl FnMut(usize) -> Result<usize>,
    ) -> Result<Vec<usize>> {
        let mut result = Vec::with_capacity(rows);
        for (row, &value) in values.iter().enumerate() {
            if row % 1024 == 0 {
                query.check()?;
            }
            let value = i128::from(value);
            // This is Dense::position for a single present value, inlined to
            // avoid its dimension loop and the [Option<i128>; 1] temporary.
            let mut position = self.flat_dense_position(value);
            if position.is_none() {
                self.grow_flat_dense_to(value, query)?;
                position = self.flat_dense_position(value);
            }
            if let Some(position) = position {
                let slot = &mut self.dense.as_mut().expect("located dense slot").slots[position];
                if *slot == EMPTY {
                    *slot = create(row)?;
                }
                result.push(*slot);
                continue;
            }
            let key = Key {
                values: [value, 0],
                nulls: 0,
            };
            let group = match self.sparse.entry(key) {
                std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::hash_map::Entry::Vacant(entry) => *entry.insert(create(row)?),
            };
            result.push(group);
        }
        query.check()?;
        Ok(result)
    }

    #[inline]
    fn locate_flat_bigint_pairs(
        &mut self,
        a: &[i64],
        b: &[i64],
        rows: usize,
        query: &QueryContext,
        mut create: impl FnMut(usize) -> Result<usize>,
    ) -> Result<Vec<usize>> {
        let mut result = Vec::with_capacity(rows);
        for (row, (&a, &b)) in a.iter().zip(b).enumerate() {
            if row % 1024 == 0 {
                query.check()?;
            }
            let values = [i128::from(a), i128::from(b)];
            let slot = self.dense.as_mut().and_then(|dense| {
                let [a, b] = dense.dimensions;
                let x = values[0].wrapping_sub(a.minimum?) as u128;
                let y = values[1].wrapping_sub(b.minimum?) as u128;
                // Construction bounds both dimensions and their product.
                // Preserve the NULL tail in each dimension's stride.
                if x < a.values as u128 && y < b.values as u128 {
                    Some(&mut dense.slots[x as usize * (b.values + 1) + y as usize])
                } else {
                    None
                }
            });
            let group = if let Some(slot) = slot {
                if *slot == EMPTY {
                    *slot = create(row)?;
                }
                *slot
            } else {
                match self.sparse.entry(Key { values, nulls: 0 }) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => *entry.insert(create(row)?),
                }
            };
            result.push(group);
        }
        query.check()?;
        Ok(result)
    }

    #[inline]
    fn flat_dense_position(&self, value: i128) -> Option<usize> {
        let dimension = self.dense.as_ref()?.dimensions[0];
        let offset = value.wrapping_sub(dimension.minimum?) as u128;
        (offset < dimension.values as u128).then_some(offset as usize)
    }

    /// Unordered flat batches discover growth while locating actual rows. This
    /// avoids a second full scan when every key already fits the dense range.
    /// Geometric growth is bounded by the existing slot cap; unobserved slots
    /// stay empty and never create groups or change first-observed ordinals.
    fn grow_flat_dense_to(&mut self, value: i128, query: &QueryContext) -> Result<()> {
        if !self.sparse.is_empty() {
            return Ok(());
        }
        let Some(dense) = self.dense.as_mut() else {
            return Ok(());
        };
        let dimension = dense.dimensions[0];
        let Some(minimum) = dimension.minimum else {
            return Ok(());
        };
        let Some(required) = value
            .checked_sub(minimum)
            .and_then(|offset| usize::try_from(offset).ok())
            .and_then(|offset| offset.checked_add(1))
        else {
            return Ok(());
        };
        if required <= dimension.values || required >= MAX_DENSE_SLOTS {
            return Ok(());
        }
        let width = required
            .max(dimension.values.saturating_mul(2))
            .min(MAX_DENSE_SLOTS - 1);
        // Dictionary parents may carry full-width HUGEINT coefficients. An
        // observed endpoint fits i128, but geometric spare capacity beyond it
        // need not. Keep Dense::position's nonwrapping interval invariant.
        let width = if minimum.checked_add((width - 1) as i128).is_some() {
            width
        } else {
            required
        };
        query.check()?;
        dense
            .slots
            .try_reserve(width + 1 - dense.slots.len())
            .map_err(|_| Error::Resource("dense grouping allocation failed".into()))?;
        let null = dense.slots[dimension.values];
        dense.slots.resize(width + 1, EMPTY);
        dense.slots[dimension.values] = EMPTY;
        dense.slots[width] = null;
        dense.dimensions[0].values = width;
        query.check()
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
    use crate::{common::DataType, parallel::InterruptHandle};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn generic_destinations(values: &[i64]) -> Vec<usize> {
        let mut groups = HashMap::new();
        values
            .iter()
            .map(|&value| {
                let next = groups.len();
                *groups.entry(value).or_insert(next)
            })
            .collect()
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn flat_bigints(values: &[i64]) -> Result<Vector> {
        Vector::flat(
            DataType::BigInt,
            values
                .iter()
                .copied()
                .map(|value| Value::Integer(i128::from(value)))
                .collect(),
        )
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn dictionary_growth_discovers_only_observed_keys_and_preserves_nulls() -> Result<()> {
        let query = QueryContext::background();
        let mut index = IntegerIndex::default();
        let mut next = 0;
        let mut reference = HashMap::new();
        for parents in [
            vec![
                Value::Integer(0),
                Value::Null,
                Value::Integer(0),
                Value::Null,
            ],
            vec![
                Value::Integer(7),
                Value::Integer(3),
                Value::Integer(1),
                Value::Null,
            ],
            vec![
                Value::Integer(-1),
                Value::Integer(i64::MAX.into()),
                Value::Integer(7),
                Value::Null,
            ],
        ] {
            let parent = std::sync::Arc::new(Vector::flat(DataType::BigInt, parents)?);
            let selected = (0..32).map(|i| [2, 0, 3, 1][i % 4]).collect::<Vec<_>>();
            let column = parent.select(selected)?;
            assert!(column.dictionary().is_some());
            let expected = column
                .values()
                .map(|value| {
                    let key = match value {
                        Value::Integer(v) => Some(v),
                        Value::Null => None,
                        _ => unreachable!(),
                    };
                    let ordinal = reference.len();
                    *reference.entry(key).or_insert(ordinal)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                locate_one_column(&mut index, &column, &mut next, &query)?,
                expected
            );
            assert_eq!(next, reference.len());
        }
        assert!(!index.sparse.is_empty());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn dictionary_hugeint_growth_preserves_nonwrapping_dense_bounds() -> Result<()> {
        let query = QueryContext::background();
        let mut index = IntegerIndex::default();
        let mut next = 0;
        let mut reference = HashMap::new();
        for parents in [
            vec![
                Value::Integer(i128::MAX - 5),
                Value::Integer(i128::MAX - 2),
                Value::Null,
            ],
            vec![
                Value::Integer(i128::MAX),
                Value::Integer(i128::MIN),
                Value::Null,
            ],
        ] {
            let parent = std::sync::Arc::new(Vector::flat(DataType::HugeInt, parents)?);
            let column = parent.select((0..12).map(|i| i % 3).collect())?;
            let expected = column
                .values()
                .map(|value| {
                    let key = match value {
                        Value::Integer(value) => Some(value),
                        Value::Null => None,
                        _ => unreachable!(),
                    };
                    let ordinal = reference.len();
                    *reference.entry(key).or_insert(ordinal)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                locate_one_column(&mut index, &column, &mut next, &query)?,
                expected
            );
            let dense = index.dense.as_ref().unwrap();
            let dimension = dense.dimensions[0];
            assert!(
                dimension
                    .minimum
                    .unwrap()
                    .checked_add((dimension.values - 1) as i128)
                    .is_some()
            );
            assert_eq!(dense.position(&[Some(i128::MIN)]), None);
        }
        assert_eq!(next, 5);
        assert_eq!(index.dense.as_ref().unwrap().dimensions[0].values, 6);
        assert_eq!(index.sparse.len(), 1);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn dictionary_pairs_preserve_duplicate_values_nulls_and_changed_parents() -> Result<()> {
        let query = QueryContext::background();
        let mut index = IntegerIndex::default();
        let mut next = 0;
        let mut reference = HashMap::new();
        for parents in [
            [
                vec![
                    Value::Integer(1),
                    Value::Integer(1),
                    Value::Integer(2),
                    Value::Null,
                ],
                vec![Value::Null, Value::Integer(4), Value::Integer(4)],
            ],
            [
                vec![
                    Value::Null,
                    Value::Integer(1),
                    Value::Integer(-1),
                    Value::Integer(i64::MAX.into()),
                ],
                vec![
                    Value::Integer(4),
                    Value::Null,
                    Value::Integer(i64::MIN.into()),
                ],
            ],
        ] {
            let [a, b] = parents;
            let a = std::sync::Arc::new(Vector::flat(DataType::BigInt, a)?)
                .select((0..48).map(|i| [3, 1, 0, 2][i % 4]).collect())?;
            let b = std::sync::Arc::new(Vector::flat(DataType::BigInt, b)?)
                .select((0..48).map(|i| [2, 0, 1][i % 3]).collect())?;
            assert!(a.dictionary().is_some() && b.dictionary().is_some());
            let expected = a
                .values()
                .zip(b.values())
                .map(|(a, b)| {
                    let key = [a, b].map(|v| match v {
                        Value::Integer(v) => Some(v),
                        Value::Null => None,
                        _ => unreachable!(),
                    });
                    let ordinal = reference.len();
                    *reference.entry(key).or_insert(ordinal)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                index.locate(
                    &[&a, &b],
                    &[KeyRepresentation::Integer; 2],
                    a.len(),
                    &query,
                    |_| {
                        let ordinal = next;
                        next += 1;
                        Ok(ordinal)
                    }
                )?,
                expected
            );
            assert_eq!(next, reference.len());
            let interrupt = InterruptHandle::default();
            let cancelled = QueryContext::new(interrupt.clone(), None, 1, usize::MAX)?;
            interrupt.interrupt();
            assert!(matches!(
                index.locate(
                    &[&a, &b],
                    &[KeyRepresentation::Integer; 2],
                    a.len(),
                    &cancelled,
                    |_| unreachable!()
                ),
                Err(Error::Interrupted)
            ));
        }
        // A parent Cartesian product over the memo bound retains generic
        // lookup; a small logical selection cannot cause a large allocation.
        let parent = std::sync::Arc::new(flat_bigints(&(0..300).collect::<Vec<_>>())?);
        let a = parent.clone().select(vec![299, 0, 299])?;
        let b = parent.select(vec![0, 299, 0])?;
        let mut fallback = IntegerIndex::default();
        let mut next = 0;
        assert_eq!(
            fallback.locate(
                &[&a, &b],
                &[KeyRepresentation::Integer; 2],
                3,
                &query,
                |_| {
                    let ordinal = next;
                    next += 1;
                    Ok(ordinal)
                }
            )?,
            [0, 1, 0]
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn flat_bigint_pairs_preserve_dense_sparse_and_nullable_identities() -> Result<()> {
        let query = QueryContext::background();
        for base in [i64::MIN, 0, i64::MAX - 1] {
            let mut index = IntegerIndex::default();
            let mut reference = HashMap::new();
            let mut next = 0;
            for pairs in [
                vec![
                    (Some(base), Some(0)),
                    (Some(base + 1), Some(1)),
                    (Some(base), Some(0)),
                ],
                vec![(Some(base), Some(1)), (Some(base + 1), Some(0))],
                vec![
                    (None, Some(0)),
                    (Some(base), None),
                    (None, None),
                    (Some(base), Some(0)),
                ],
                vec![
                    (Some(i64::MAX), Some(i64::MIN)),
                    (Some(base), Some(1)),
                    (Some(i64::MAX), Some(i64::MIN)),
                ],
                vec![],
            ] {
                let mut expected = Vec::new();
                for pair in &pairs {
                    let ordinal = reference.len();
                    expected.push(*reference.entry(*pair).or_insert(ordinal));
                }
                let vectors = [0, 1].map(|column| {
                    Vector::flat(
                        DataType::BigInt,
                        pairs
                            .iter()
                            .map(|&(a, b)| {
                                [a, b][column]
                                    .map_or(Value::Null, |v| Value::Integer(i128::from(v)))
                            })
                            .collect(),
                    )
                });
                let [a, b] = vectors;
                let (a, b) = (a?, b?);
                let actual = index.locate(
                    &[&a, &b],
                    &[KeyRepresentation::Integer; 2],
                    pairs.len(),
                    &query,
                    |_| {
                        let group = next;
                        next += 1;
                        Ok(group)
                    },
                )?;
                assert_eq!(actual, expected);
                assert_eq!(next, reference.len());
                assert!(index.dense.is_some());
            }
            assert!(!index.sparse.is_empty());
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn flat_bigint_pairs_handle_empty_initialization_extremes_and_cancellation() -> Result<()> {
        let query = QueryContext::background();
        let empty = flat_bigints(&[])?;
        let mut index = IntegerIndex::default();
        assert!(
            index
                .locate(
                    &[&empty, &empty],
                    &[KeyRepresentation::Integer; 2],
                    0,
                    &query,
                    |_| unreachable!()
                )?
                .is_empty()
        );
        let a = flat_bigints(&[i64::MIN, i64::MAX, i64::MIN])?;
        let b = flat_bigints(&[i64::MAX, i64::MIN, i64::MAX])?;
        let mut next = 0;
        assert_eq!(
            index.locate(
                &[&a, &b],
                &[KeyRepresentation::Integer; 2],
                3,
                &query,
                |_| {
                    let group = next;
                    next += 1;
                    Ok(group)
                }
            )?,
            [0, 1, 0]
        );
        assert!(matches!(
            index.locate(
                &[&a, &b],
                &[KeyRepresentation::Integer],
                3,
                &query,
                |_| unreachable!()
            ),
            Err(Error::Internal(_))
        ));
        let interrupt = InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 1, usize::MAX)?;
        interrupt.interrupt();
        assert!(matches!(
            index.locate(
                &[&a, &b],
                &[KeyRepresentation::Integer; 2],
                3,
                &cancelled,
                |_| unreachable!()
            ),
            Err(Error::Interrupted)
        ));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn unordered_flat_growth_preserves_nulls_ordinals_and_sparse_fallback() -> Result<()> {
        let query = QueryContext::background();
        let first = Vector::flat(DataType::BigInt, vec![Value::Integer(0), Value::Null])?;
        let mut index = IntegerIndex::default();
        let mut next = 0;
        assert_eq!(
            locate_one_column(&mut index, &first, &mut next, &query)?,
            [0, 1]
        );
        let unordered = flat_bigints(&[3, 1, 3, 2, 0])?;
        assert!(!unordered.numeric_ascending());
        assert_eq!(
            locate_one_column(&mut index, &unordered, &mut next, &query)?,
            [2, 3, 2, 4, 0]
        );
        assert!(index.sparse.is_empty());
        assert_eq!(next, 5);
        let null_again = Vector::flat(DataType::BigInt, vec![Value::Null, Value::Integer(3)])?;
        assert_eq!(
            locate_one_column(&mut index, &null_again, &mut next, &query)?,
            [1, 2]
        );
        let outliers = flat_bigints(&[i64::MAX, -1, i64::MAX])?;
        assert_eq!(
            locate_one_column(&mut index, &outliers, &mut next, &query)?,
            [5, 6, 5]
        );
        let followup = flat_bigints(&[4, 3, 4])?;
        assert_eq!(
            locate_one_column(&mut index, &followup, &mut next, &query)?,
            [7, 2, 7]
        );
        assert_eq!(next, 8);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn unordered_flat_growth_honors_slot_cap_signed_edges_and_cancellation() -> Result<()> {
        let query = QueryContext::background();
        for base in [i64::MIN, i64::MAX - 3] {
            let mut index = IntegerIndex::default();
            let mut next = 0;
            let first = flat_bigints(&[base, base])?;
            let more = flat_bigints(&[base + 3, base + 1, base + 2, base])?;
            assert_eq!(
                locate_one_column(&mut index, &first, &mut next, &query)?,
                [0, 0]
            );
            assert_eq!(
                locate_one_column(&mut index, &more, &mut next, &query)?,
                [1, 2, 3, 0]
            );
            assert!(index.sparse.is_empty());
        }
        let mut index = IntegerIndex::default();
        let mut next = 0;
        let first = Vector::flat(DataType::BigInt, vec![Value::Integer(0), Value::Null])?;
        locate_one_column(&mut index, &first, &mut next, &query)?;
        let edge = flat_bigints(&[(MAX_DENSE_SLOTS - 2) as i64, 0])?;
        assert_eq!(
            locate_one_column(&mut index, &edge, &mut next, &query)?,
            [2, 0]
        );
        assert_eq!(index.dense.as_ref().unwrap().slots.len(), MAX_DENSE_SLOTS);
        let beyond = flat_bigints(&[(MAX_DENSE_SLOTS - 1) as i64, 0])?;
        assert_eq!(
            locate_one_column(&mut index, &beyond, &mut next, &query)?,
            [3, 0]
        );
        assert_eq!(index.dense.as_ref().unwrap().slots.len(), MAX_DENSE_SLOTS);
        assert_eq!(index.sparse.len(), 1);
        let null_again = Vector::flat(DataType::BigInt, vec![Value::Null])?;
        assert_eq!(
            locate_one_column(&mut index, &null_again, &mut next, &query)?,
            [1]
        );

        let interrupt = crate::parallel::InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 2, usize::MAX)?;
        interrupt.interrupt();
        assert!(matches!(
            locate_one_column(&mut index, &edge, &mut next, &cancelled),
            Err(Error::Interrupted)
        ));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn flat_bigint_locator_preserves_dense_and_sparse_group_ordinals() -> Result<()> {
        let query = QueryContext::background();
        let dense = Vector::flat(DataType::BigInt, vec![Value::Integer(0), Value::Integer(1)])?;
        let sparse = Vector::flat(
            DataType::BigInt,
            vec![Value::Integer(-1), Value::Integer(-1)],
        )?;
        assert!(dense.flat_bigints().is_some());
        assert!(sparse.flat_bigints().is_some());
        let mut index = IntegerIndex::default();
        let mut next_group = 0;
        assert_eq!(
            locate_one_column(&mut index, &dense, &mut next_group, &query)?,
            vec![0, 1]
        );
        // The later lower key cannot extend the monotonic dense range and
        // must retain sparse identity and first-logical-row creation.
        assert_eq!(
            locate_one_column(&mut index, &sparse, &mut next_group, &query)?,
            vec![2, 2]
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn flat_bigint_first_batch_matches_generic_destinations_at_empty_and_signed_bounds()
    -> Result<()> {
        let query = QueryContext::background();
        let mut index = IntegerIndex::default();
        let mut next_group = 0;
        let empty = flat_bigints(&[])?;
        assert!(empty.flat_bigints().is_some());
        assert_eq!(
            locate_one_column(&mut index, &empty, &mut next_group, &query)?,
            generic_destinations(&[])
        );

        let values = [i64::MIN, i64::MIN, 0, i64::MAX, 0, i64::MAX];
        let column = flat_bigints(&values)?;
        assert!(column.flat_bigints().is_some());
        assert_eq!(
            locate_one_column(&mut index, &column, &mut next_group, &query)?,
            generic_destinations(&values)
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn flat_bigint_first_batch_handles_minimum_adjacent_dense_range() -> Result<()> {
        let query = QueryContext::background();
        let values = [i64::MIN, i64::MIN + 1, i64::MIN + 3, i64::MIN + 1];
        let column = flat_bigints(&values)?;
        let mut index = IntegerIndex::default();
        let mut next_group = 0;
        assert_eq!(
            locate_one_column(&mut index, &column, &mut next_group, &query)?,
            generic_destinations(&values)
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn flat_bigint_first_batch_then_monotonic_growth_matches_generic_destinations() -> Result<()> {
        let query = QueryContext::background();
        let first_values = [10, 11, 10];
        let second_values = [12, 13, 12, 10];
        let first = flat_bigints(&first_values)?;
        let second = flat_bigints(&second_values)?;
        let mut index = IntegerIndex::default();
        let mut next_group = 0;
        assert_eq!(
            locate_one_column(&mut index, &first, &mut next_group, &query)?,
            generic_destinations(&first_values)
        );
        let all_values = [10, 11, 10, 12, 13, 12, 10];
        let expected = generic_destinations(&all_values);
        assert_eq!(
            locate_one_column(&mut index, &second, &mut next_group, &query)?,
            expected[first_values.len()..]
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn flat_bigint_dense_initialization_checks_cancellation() -> Result<()> {
        let interrupt = InterruptHandle::default();
        interrupt.interrupt();
        let query = QueryContext::new(interrupt, None, 1, usize::MAX)?;
        let column = flat_bigints(&[1])?;
        let mut index = IntegerIndex::default();
        let mut next_group = 0;
        assert!(locate_one_column(&mut index, &column, &mut next_group, &query).is_err());
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
