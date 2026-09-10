//! Small integer tuples use bounded direct addressing with a sparse overflow
//! map. Only an explicit, validated integer-key capability permits this path.
use crate::{
    Value,
    common::{Error, Result, vector::Vector},
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
    fn new(columns: &[&Vector], rows: usize, query: &QueryContext) -> Result<Option<Self>> {
        let mut dimensions = [Dimension::default(); 2];
        let mut count = 1usize;
        for (dimension, column) in dimensions.iter_mut().zip(columns) {
            let (mut minimum, mut maximum) = (None::<i128>, None::<i128>);
            for (i, value) in column.values().enumerate() {
                if i % 1024 == 0 {
                    query.check()?;
                }
                if let Value::Integer(value) = value {
                    minimum = Some(minimum.map_or(*value, |v| v.min(*value)));
                    maximum = Some(maximum.map_or(*value, |v| v.max(*value)));
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
    fn position<const N: usize>(&self, values: &[&Value; N]) -> Option<usize> {
        let mut result = 0;
        for (i, &value) in values.iter().enumerate() {
            let dimension = &self.dimensions[i];
            let position = match value {
                Value::Null => dimension.values,
                Value::Integer(value) => {
                    let position = usize::try_from(value.checked_sub(dimension.minimum?)?).ok()?;
                    if position >= dimension.values {
                        return None;
                    }
                    position
                }
                _ => unreachable!("validated integer grouping key"),
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
        rows: usize,
        query: &QueryContext,
        create: impl FnMut(usize) -> Result<usize>,
    ) -> Result<Vec<usize>> {
        if let Some(empty) = self.empty {
            return Ok(vec![empty; rows]);
        }
        if !self.initialized {
            self.dense = Dense::new(columns, rows, query)?;
            self.initialized = true;
        }
        match columns {
            [column] => match column.flat_values() {
                Some(values) => self.locate_values(values.iter().map(|v| [v]), rows, query, create),
                None => self.locate_values(column.values().map(|v| [v]), rows, query, create),
            },
            [a, b] => match a.flat_values().zip(b.flat_values()) {
                Some((a, b)) => {
                    self.locate_values(a.iter().zip(b).map(|(a, b)| [a, b]), rows, query, create)
                }
                None => self.locate_values(
                    a.values().zip(b.values()).map(|(a, b)| [a, b]),
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
    fn locate_values<'a, const N: usize>(
        &mut self,
        values: impl Iterator<Item = [&'a Value; N]>,
        rows: usize,
        query: &QueryContext,
        mut create: impl FnMut(usize) -> Result<usize>,
    ) -> Result<Vec<usize>> {
        let mut result = Vec::with_capacity(rows);
        for (row, values) in values.enumerate() {
            if row % 1024 == 0 {
                query.check()?;
            }
            let dense_slot = self
                .dense
                .as_mut()
                .and_then(|dense| dense.position(&values).map(|i| &mut dense.slots[i]));
            let group = if let Some(slot) = dense_slot {
                if *slot == EMPTY {
                    *slot = create(row)?;
                }
                *slot
            } else {
                let mut key = Key::default();
                for (i, value) in values.iter().enumerate() {
                    match value {
                        Value::Integer(value) => key.values[i] = *value,
                        Value::Null => key.nulls |= 1 << i,
                        _ => unreachable!("validated integer grouping key"),
                    }
                }
                match self.sparse.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => *entry.insert(create(row)?),
                }
            };
            result.push(group);
        }
        query.check()?;
        Ok(result)
    }
}
