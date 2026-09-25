//! Published snapshots own columns. A private writer materializes rows once,
//! then validates and seals them before publication. These are alternative
//! representations of the same data, never a query cache or a second copy.
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use serde::{Deserialize, Serialize, ser::SerializeMap};

use crate::{
    common::{
        DataType, Error, Result, Row, Value,
        vector::{DataChunk, Vector},
    },
    parallel::QueryContext,
    storage::{
        RowId,
        scan::{RowIdentities, SnapshotScan},
    },
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

pub(crate) enum PackedBigInts<'a> {
    Flat(&'a [i64]),
    Chunks(Vec<&'a [i64]>),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PackedBigInts<'_> {
    pub(crate) fn len(&self) -> usize {
        self.segments().iter().map(|values| values.len()).sum()
    }
    pub(crate) fn segments(&self) -> &[&[i64]] {
        match self {
            Self::Flat(values) => std::slice::from_ref(values),
            Self::Chunks(values) => values,
        }
    }
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
    pub fn get(&self, column: usize) -> Option<Value> {
        match self {
            Self::Row(row) => row.get(column).cloned(),
            Self::Columns(data, index) => data.columns().get(column)?.get(*index),
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = Value> + '_ {
        (0..self.len()).filter_map(|index| self.get(index))
    }
    pub fn to_owned(self) -> Row {
        self.iter().collect()
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
    /// Borrow the one ordinary all-valid BIGINT lane only when its published
    /// identities are the untouched implicit append stream. Checkpoint
    /// encoding uses this private proof to avoid reconstructing owned rows.
    pub(super) fn implicit_append_bigints(
        &self,
        next_id: RowId,
        context: &QueryContext,
    ) -> Result<Option<PackedBigInts<'_>>> {
        let Self::Published { ids, data, types } = self else {
            return Ok(None);
        };
        let Ok(expected) = usize::try_from(next_id) else {
            return Ok(None);
        };
        if expected == 0
            || ids.len() != expected
            || data.len() != expected
            || types.as_ref() != [DataType::BigInt]
            || data.columns().len() != 1
        {
            return Ok(None);
        }
        context.check_rows(expected)?;
        if ids
            .iter()
            .enumerate()
            .any(|(index, id)| *id != index as RowId)
        {
            return Ok(None);
        }
        context.check()?;
        Ok(data.columns()[0]
            .flat_bigints()
            .map(PackedBigInts::Flat)
            .or_else(|| {
                data.columns()[0]
                    .flat_bigint_segments()
                    .map(PackedBigInts::Chunks)
            }))
    }
    pub fn from_chunks(
        ids: Vec<RowId>,
        types: Arc<[DataType]>,
        chunks: Vec<DataChunk>,
        context: &QueryContext,
    ) -> Result<Self> {
        let count = chunks.iter().try_fold(0usize, |count, chunk| {
            count
                .checked_add(chunk.len())
                .ok_or_else(|| Error::Resource("table row count overflow".into()))
        })?;
        if ids.len() != count {
            return Err(Error::Internal(
                "published table identities differ from columns".into(),
            ));
        }
        context.check_rows(count)?;
        let mut columns = types.iter().map(|_| Vec::new()).collect::<Vec<_>>();
        for chunk in chunks {
            context.check()?;
            if chunk.columns().len() != types.len()
                || !chunk
                    .columns()
                    .iter()
                    .map(Vector::data_type)
                    .eq(types.iter())
            {
                return Err(Error::Internal(
                    "insert chunk differs from table schema".into(),
                ));
            }
            for (output, column) in columns.iter_mut().zip(chunk.columns()) {
                output.push(column.clone());
            }
        }
        // CTAS already hands ownership of immutable batch vectors to storage.
        // A high-cardinality column cannot use the compact dictionary, so
        // retain its segments instead of allocating a second table-wide Value
        // column while all input batches remain live. Scan slices fully
        // contained in a segment recover the original vector, including its
        // flat numeric fast paths. Small physical domains keep the existing
        // dictionary path, where copying is an actual storage reduction.
        let columns = columns
            .into_iter()
            .zip(types.iter())
            .map(|(mut columns, data_type)| {
                if retains_high_cardinality_segments(&columns, count) {
                    return match columns.len() {
                        0 => Vector::chunked(data_type.clone(), columns),
                        1 => Ok(columns.pop().expect("one input column")),
                        _ => Vector::chunked(data_type.clone(), columns),
                    };
                }
                compact_vectors(data_type, columns, count)
            })
            .collect::<Result<_>>()?;
        Ok(Self::Published {
            ids: ids.into(),
            data: DataChunk::new(columns, count)?,
            types,
        })
    }
    pub fn add_column_values(
        &mut self,
        data_type: &DataType,
        values: &BTreeMap<RowId, Value>,
        context: &QueryContext,
    ) -> Result<()> {
        context.check()?;
        if values.len() != self.len() || self.keys().any(|id| !values.contains_key(id)) {
            return Err(Error::Internal(
                "ADD COLUMN values differ from live physical slots".into(),
            ));
        }
        match self {
            Self::Writable(rows) => {
                for (id, row) in rows {
                    context.check()?;
                    row.push(values[id].clone());
                }
            }
            Self::Published { ids, data, types } => {
                let column = ids.iter().map(|id| values[id].clone()).collect::<Vec<_>>();
                let mut columns = data.columns().to_vec();
                columns.push(Vector::flat(data_type.clone(), column)?);
                *data = DataChunk::new(columns, data.len())?;
                let mut next = types.to_vec();
                next.push(data_type.clone());
                *types = next.into();
            }
        }
        Ok(())
    }
    pub fn add_column_constant(
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
    pub fn published_data(&self) -> Option<&DataChunk> {
        match self {
            Self::Published { data, .. } => Some(data),
            Self::Writable(_) => None,
        }
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
            .map(|(values, data_type)| compact_column(data_type, values))
            .collect::<Result<_>>()?;
        context.check()?;
        *self = Self::Published {
            ids: ids.into(),
            data: DataChunk::new(columns, count)?,
            types,
        };
        Ok(())
    }
    pub fn scan_ordered(&self, order: &[RowId]) -> Result<SnapshotScan> {
        match self {
            Self::Published { ids, data, types } => {
                if order.len() != ids.len() {
                    return Err(Error::Internal(
                        "physical scan order differs from live rows".into(),
                    ));
                }
                if order == ids.as_ref() {
                    return Ok(SnapshotScan {
                        identities: RowIdentities::Shared {
                            ids: ids.clone(),
                            offset: 0,
                        },
                        len: ids.len(),
                        data: data.clone(),
                        types: types.clone(),
                        position: 0,
                        finished: false,
                    });
                }
                let selection = order
                    .iter()
                    .map(|id| {
                        ids.binary_search(id).map_err(|_| {
                            Error::Internal("physical scan references an invisible row".into())
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(SnapshotScan {
                    identities: RowIdentities::Owned(order.to_vec()),
                    len: order.len(),
                    data: data.select(&selection)?,
                    types: types.clone(),
                    position: 0,
                    finished: false,
                })
            }
            Self::Writable(_) => Err(Error::Internal("unpublished table scan".into())),
        }
    }
    pub fn scan_implicit_append(&self, next_id: RowId) -> Result<SnapshotScan> {
        match self {
            Self::Published { ids, data, types }
                if ids.len() == usize::try_from(next_id).unwrap_or(usize::MAX) =>
            {
                Ok(SnapshotScan {
                    identities: RowIdentities::Range { start: 0 },
                    len: ids.len(),
                    data: data.clone(),
                    types: types.clone(),
                    position: 0,
                    finished: false,
                })
            }
            Self::Published { .. } => Err(Error::Internal(
                "implicit physical stream differs from live rows".into(),
            )),
            Self::Writable(_) => Err(Error::Internal("unpublished table scan".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Stop probing as soon as the small physical dictionary is known not to fit.
/// This mirrors `compact_vectors`' 256-entry limit without materializing a
/// full column merely to discover an ordinary high-cardinality CTAS input.
fn retains_high_cardinality_segments(columns: &[Vector], count: usize) -> bool {
    const MAX_DICTIONARY_VALUES: usize = 256;
    let limit = MAX_DICTIONARY_VALUES.min(count / 4);
    if limit == 0 {
        return false;
    }
    let mut dictionary = HashMap::<Vec<u8>, ()>::new();
    let mut key = Vec::new();
    for column in columns {
        for value in column.values() {
            key.clear();
            if !crate::common::vector::append_physical_identity(&value, &mut key) {
                return false;
            }
            dictionary.entry(key.clone()).or_insert(());
            if dictionary.len() > limit {
                return true;
            }
        }
    }
    false
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Published table segments retain a compact physical dictionary when exact
/// payload identity has a small domain. This is storage encoding, not SQL
/// equality: floating-point bits and nested payload boundaries are preserved.
/// Unsupported opaque payloads and wider domains stay flat.
fn compact_column(data_type: &DataType, values: Vec<Value>) -> Result<Vector> {
    const MAX_DICTIONARY_VALUES: usize = 256;
    if values.len() < 8 {
        return Vector::flat(data_type.clone(), values);
    }
    let limit = MAX_DICTIONARY_VALUES.min(values.len() / 4);
    let mut dictionary = HashMap::<Vec<u8>, usize>::new();
    let mut unique = Vec::new();
    let mut selection = Vec::with_capacity(values.len());
    let mut key = Vec::new();
    for value in &values {
        key.clear();
        if !crate::common::vector::append_physical_identity(value, &mut key) {
            return Vector::flat(data_type.clone(), values);
        }
        if let Some(&index) = dictionary.get(key.as_slice()) {
            selection.push(index);
            continue;
        }
        if unique.len() == limit {
            return Vector::flat(data_type.clone(), values);
        }
        let index = unique.len();
        dictionary.insert(key.clone(), index);
        unique.push(value.clone());
        selection.push(index);
    }
    if unique.len() == 1 {
        return Vector::constant(
            data_type.clone(),
            unique.pop().expect("one value"),
            values.len(),
        );
    }
    Arc::new(Vector::flat(data_type.clone(), unique)?).select(selection)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Merge already encoded CTAS batches by their exact dictionary entries.
/// This avoids materializing and then re-hashing every logical row when each
/// source batch already proved a small physical domain.
fn compact_vectors(data_type: &DataType, columns: Vec<Vector>, count: usize) -> Result<Vector> {
    if data_type.is_signed_integer()
        && let Some(compact) = compact_dense_signed_vectors(data_type, &columns, count)?
    {
        return Ok(compact);
    }
    const MAX_DICTIONARY_VALUES: usize = 256;
    let limit = MAX_DICTIONARY_VALUES.min(count / 4);
    let mut dictionary = HashMap::<Vec<u8>, usize>::new();
    let mut unique = Vec::new();
    let mut selection = Vec::with_capacity(count);
    let mut key = Vec::new();
    let mut failed = false;
    {
        let mut intern = |value: &Value| -> Option<usize> {
            key.clear();
            if !crate::common::vector::append_physical_identity(value, &mut key) {
                return None;
            }
            if let Some(&index) = dictionary.get(key.as_slice()) {
                return Some(index);
            }
            if unique.len() == limit {
                return None;
            }
            let index = unique.len();
            dictionary.insert(key.clone(), index);
            unique.push(value.clone());
            Some(index)
        };
        'columns: for column in &columns {
            if let Some(value) = column.constant_value() {
                let Some(index) = intern(value) else {
                    failed = true;
                    break;
                };
                selection.extend(std::iter::repeat_n(index, column.len()));
                continue;
            }
            let Some((parent, selected)) = column.dictionary() else {
                failed = true;
                break;
            };
            let mut mapped = vec![usize::MAX; parent.len()];
            for &source in selected {
                if mapped[source] == usize::MAX {
                    let Some(index) =
                        intern(&parent.get(source).expect("validated dictionary index"))
                    else {
                        failed = true;
                        break 'columns;
                    };
                    mapped[source] = index;
                }
                selection.push(mapped[source]);
            }
        }
    }
    if failed || selection.len() != count {
        let combined = Vector::concatenate(data_type.clone(), &columns)?;
        let mut values = Vec::with_capacity(count);
        combined.append_to(&mut values);
        return compact_column(data_type, values);
    }
    if unique.len() == 1 {
        return Vector::constant(data_type.clone(), unique.pop().expect("one value"), count);
    }
    Arc::new(Vector::flat(data_type.clone(), unique)?).select(selection)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Dense fixed-width integer domains can be materially larger than the small
/// opaque-payload dictionary limit and still use less memory than repeated
/// row `Value`s. Build their dictionary arithmetically, without hashing every
/// input or constructing a second flat column before deciding the encoding.
fn compact_dense_signed_vectors(
    data_type: &DataType,
    columns: &[Vector],
    count: usize,
) -> Result<Option<Vector>> {
    const MAX_DENSE_DICTIONARY_VALUES: usize = 16_384;
    if count < 8 {
        return Ok(None);
    }
    let mut minimum = None::<i128>;
    let mut maximum = None::<i128>;
    let mut has_null = false;
    for column in columns {
        for value in column.values() {
            match value {
                Value::Null => has_null = true,
                Value::Integer(value) => {
                    minimum = Some(minimum.map_or(value, |minimum| minimum.min(value)));
                    maximum = Some(maximum.map_or(value, |maximum| maximum.max(value)));
                }
                _ => {
                    return Err(Error::Internal(
                        "signed column has non-integer payload".into(),
                    ));
                }
            }
        }
    }
    let Some((minimum, maximum)) = minimum.zip(maximum) else {
        return Ok(Some(Vector::constant(
            data_type.clone(),
            Value::Null,
            count,
        )?));
    };
    let Some(width) = maximum
        .abs_diff(minimum)
        .checked_add(1)
        .and_then(|width| usize::try_from(width).ok())
    else {
        return Ok(None);
    };
    let entries = width.saturating_add(usize::from(has_null));
    if entries > MAX_DENSE_DICTIONARY_VALUES || entries > count / 4 {
        return Ok(None);
    }
    let mut values = Vec::with_capacity(entries);
    values.extend((0..width).map(|offset| Value::Integer(minimum + offset as i128)));
    let null = values.len();
    if has_null {
        values.push(Value::Null);
    }
    let mut selection = Vec::with_capacity(count);
    for column in columns {
        selection.extend(column.values().map(|value| match value {
            Value::Null => null,
            Value::Integer(value) => (value - minimum) as usize,
            _ => unreachable!("validated signed column"),
        }));
    }
    let parent = Arc::new(Vector::flat(data_type.clone(), values)?);
    Ok(Some(parent.select(selection)?))
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
