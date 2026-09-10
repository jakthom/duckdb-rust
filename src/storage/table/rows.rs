//! Published snapshots own columns. A private writer materializes rows once,
//! then validates and seals them before publication. These are alternative
//! representations of the same data, never a query cache or a second copy.
use std::{collections::BTreeMap, ops::Index, sync::Arc};

use serde::{Deserialize, Serialize, ser::SerializeMap};

use crate::{
    common::{
        DataType, Error, Result, Row, Value,
        vector::{DataChunk, Vector},
    },
    parallel::QueryContext,
    storage::{RowId, scan::SnapshotScan},
};

#[derive(Clone, Debug)]
pub(super) enum Rows {
    Writable(BTreeMap<RowId, Row>),
    Published {
        ids: Arc<[RowId]>,
        data: DataChunk,
        types: Arc<[DataType]>,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for Rows {
    fn default() -> Self {
        Self::Writable(BTreeMap::new())
    }
}

#[derive(Clone, Copy)]
pub(super) enum RowView<'a> {
    Row(&'a Row),
    Columns(&'a DataChunk, usize),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RowView<'_> {
    pub fn len(&self) -> usize {
        match self {
            Self::Row(row) => row.len(),
            Self::Columns(data, _) => data.columns().len(),
        }
    }
    pub fn get(&self, column: usize) -> Option<&Value> {
        match self {
            Self::Row(row) => row.get(column),
            Self::Columns(data, index) => data.columns().get(column)?.get(*index),
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        (0..self.len()).map(|index| &self[index])
    }
    pub fn to_owned(self) -> Row {
        self.iter().cloned().collect()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Index<usize> for RowView<'_> {
    type Output = Value;
    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("validated row column")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Serialize for RowView<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Rows {
    pub fn add_column(
        &mut self,
        data_type: &DataType,
        value: &Value,
        context: &QueryContext,
    ) -> Result<()> {
        context.check()?;
        match self {
            Self::Writable(rows) => {
                for row in rows.values_mut() {
                    context.check()?;
                    row.push(value.clone());
                }
            }
            Self::Published { data, types, .. } => {
                let mut columns = data.columns().to_vec();
                columns.push(Vector::constant(
                    data_type.clone(),
                    value.clone(),
                    data.len(),
                )?);
                *data = DataChunk::new(columns, data.len())?;
                let mut next = types.to_vec();
                next.push(data_type.clone());
                *types = next.into();
            }
        }
        Ok(())
    }
    pub fn drop_column(&mut self, column: usize, context: &QueryContext) -> Result<()> {
        context.check()?;
        match self {
            Self::Writable(rows) => {
                for row in rows.values_mut() {
                    context.check()?;
                    row.remove(column);
                }
            }
            Self::Published { data, types, .. } => {
                let mut columns = data.columns().to_vec();
                columns.remove(column);
                *data = DataChunk::new(columns, data.len())?;
                let mut next = types.to_vec();
                next.remove(column);
                *types = next.into();
            }
        }
        Ok(())
    }
    pub fn len(&self) -> usize {
        match self {
            Self::Writable(rows) => rows.len(),
            Self::Published { ids, .. } => ids.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn iter(&self) -> impl Iterator<Item = (&RowId, RowView<'_>)> {
        let (rows, columns) = match self {
            Self::Writable(rows) => (Some(rows), None),
            Self::Published { ids, data, .. } => (None, Some((ids, data))),
        };
        rows.into_iter()
            .flat_map(|rows| rows.iter().map(|(id, row)| (id, RowView::Row(row))))
            .chain(columns.into_iter().flat_map(|(ids, data)| {
                ids.iter()
                    .enumerate()
                    .map(move |(index, id)| (id, RowView::Columns(data, index)))
            }))
    }
    pub fn keys(&self) -> impl Iterator<Item = &RowId> {
        self.iter().map(|(id, _)| id)
    }
    pub fn values(&self) -> impl Iterator<Item = RowView<'_>> {
        self.iter().map(|(_, row)| row)
    }
    pub fn get(&self, id: &RowId) -> Option<RowView<'_>> {
        match self {
            Self::Writable(rows) => rows.get(id).map(RowView::Row),
            Self::Published { ids, data, .. } => ids
                .binary_search(id)
                .ok()
                .map(|index| RowView::Columns(data, index)),
        }
    }
    pub fn contains_key(&self, id: &RowId) -> bool {
        self.get(id).is_some()
    }
    fn writable(&mut self) -> &mut BTreeMap<RowId, Row> {
        if matches!(self, Self::Published { .. }) {
            *self = Self::Writable(self.iter().map(|(&id, row)| (id, row.to_owned())).collect());
        }
        let Self::Writable(rows) = self else {
            unreachable!("materialized writer")
        };
        rows
    }
    pub fn insert(&mut self, id: RowId, row: Row) -> Option<Row> {
        self.writable().insert(id, row)
    }
    pub fn remove(&mut self, id: &RowId) -> Option<Row> {
        self.writable().remove(id)
    }
    pub fn get_mut(&mut self, id: &RowId) -> Option<&mut Row> {
        self.writable().get_mut(id)
    }
    /// Consumes the private writer after logical validation. On failure the
    /// caller discards this unpublished table; existing snapshots are untouched.
    pub fn seal(&mut self, types: Arc<[DataType]>, context: &QueryContext) -> Result<()> {
        if matches!(self, Self::Published { .. }) {
            return Ok(());
        }
        let Self::Writable(rows) = std::mem::take(self) else {
            unreachable!("unpublished rows")
        };
        let count = rows.len();
        let mut ids = Vec::with_capacity(count);
        let mut columns: Vec<_> = types.iter().map(|_| Vec::with_capacity(count)).collect();
        for (id, row) in rows {
            context.check()?;
            if row.len() != columns.len() {
                return Err(Error::Internal("row width differs from table".into()));
            }
            ids.push(id);
            for (column, value) in columns.iter_mut().zip(row) {
                column.push(value);
            }
        }
        let columns = columns
            .into_iter()
            .zip(types.iter())
            .map(|(values, data_type)| Vector::flat(data_type.clone(), values))
            .collect::<Result<_>>()?;
        context.check()?;
        *self = Self::Published {
            ids: ids.into(),
            data: DataChunk::new(columns, count)?,
            types,
        };
        Ok(())
    }
    pub fn scan(&self) -> Result<SnapshotScan<'_>> {
        match self {
            Self::Published { ids, data, types } => Ok(SnapshotScan {
                ids,
                data,
                types: types.clone(),
                position: 0,
                finished: false,
            }),
            Self::Writable(_) => Err(Error::Internal("unpublished table scan".into())),
        }
    }
}

// The snapshot format describes logical rows and identities, independently of
// their in-memory representation. Decoding remains private until validation.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Serialize for Rows {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (id, row) in self.iter() {
            map.serialize_entry(id, &row)?;
        }
        map.end()
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'de> Deserialize<'de> for Rows {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        BTreeMap::deserialize(deserializer).map(Self::Writable)
    }
}
