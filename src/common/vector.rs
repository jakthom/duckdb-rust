use std::sync::Arc;

use super::{DataType, Error, Result, Row, Value};

#[derive(Clone, Debug)]
enum Encoding {
    Flat(Arc<[Value]>),
    Constant(Value, usize),
    Dictionary(Arc<Vector>, Arc<[usize]>),
}

/// Immutable, owning column view. Selection and validity are resolved by `get`.
#[derive(Clone, Debug)]
pub struct Vector {
    data_type: DataType,
    encoding: Encoding,
}

impl Vector {
    pub fn flat(data_type: DataType, values: Vec<Value>) -> Result<Self> {
        if values.iter().any(|value| !value.fits_type(&data_type)) {
            return Err(Error::Internal(
                "vector values require explicit conversion to the declared type".into(),
            ));
        }
        Ok(Self {
            data_type,
            encoding: Encoding::Flat(values.into()),
        })
    }
    pub fn constant(data_type: DataType, value: Value, count: usize) -> Result<Self> {
        if !value.fits_type(&data_type) {
            return Err(Error::Internal(
                "constant vector requires explicit conversion to the declared type".into(),
            ));
        }
        Ok(Self {
            data_type,
            encoding: Encoding::Constant(value, count),
        })
    }
    pub fn select(self: &Arc<Self>, selection: Vec<usize>) -> Result<Self> {
        if selection.iter().any(|&i| i >= self.len()) {
            return Err(Error::Internal("vector selection out of bounds".into()));
        }
        Ok(Self {
            data_type: self.data_type.clone(),
            encoding: Encoding::Dictionary(self.clone(), selection.into()),
        })
    }
    pub fn data_type(&self) -> &DataType {
        &self.data_type
    }
    pub fn len(&self) -> usize {
        match &self.encoding {
            Encoding::Flat(v) => v.len(),
            Encoding::Constant(_, n) => *n,
            Encoding::Dictionary(_, s) => s.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, index: usize) -> Option<&Value> {
        match &self.encoding {
            Encoding::Flat(v) => v.get(index),
            Encoding::Constant(v, n) => (index < *n).then_some(v),
            Encoding::Dictionary(v, s) => s.get(index).and_then(|&i| v.get(i)),
        }
    }
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        (0..self.len()).filter_map(|i| self.get(i))
    }
}

#[derive(Clone, Debug)]
pub struct DataChunk {
    columns: Vec<Vector>,
    count: usize,
}

impl DataChunk {
    pub fn new(columns: Vec<Vector>, count: usize) -> Result<Self> {
        if columns.iter().any(|v| v.len() != count) {
            return Err(Error::Internal(
                "chunk columns differ in cardinality".into(),
            ));
        }
        Ok(Self { columns, count })
    }
    pub fn from_rows(types: &[DataType], rows: &[Row]) -> Result<Self> {
        if rows.iter().any(|r| r.len() != types.len()) {
            return Err(Error::Internal("row width differs from schema".into()));
        }
        let columns = types
            .iter()
            .enumerate()
            .map(|(i, t)| Vector::flat(t.clone(), rows.iter().map(|r| r[i].clone()).collect()))
            .collect::<Result<_>>()?;
        Self::new(columns, rows.len())
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn columns(&self) -> &[Vector] {
        &self.columns
    }
    /// Owns selected column views without copying flat or dictionary payloads.
    /// Reordering and duplicates are allowed; an empty projection preserves
    /// cardinality. Every ordinal is checked before returning a result.
    pub fn project(&self, ordinals: &[usize]) -> Result<Self> {
        let columns = ordinals
            .iter()
            .map(|&ordinal| {
                self.columns
                    .get(ordinal)
                    .cloned()
                    .ok_or_else(|| Error::Internal("chunk projection out of bounds".into()))
            })
            .collect::<Result<_>>()?;
        Self::new(columns, self.count)
    }
    pub fn select(&self, selection: &[usize]) -> Result<Self> {
        if selection.iter().any(|&index| index >= self.count) {
            return Err(Error::Internal("chunk selection out of bounds".into()));
        }
        let columns = self
            .columns
            .iter()
            .map(|column| Arc::new(column.clone()).select(selection.to_vec()))
            .collect::<Result<_>>()?;
        Self::new(columns, selection.len())
    }
    pub fn rows(&self) -> impl Iterator<Item = Row> + '_ {
        (0..self.count).map(|i| {
            self.columns
                .iter()
                .map(|v| v.get(i).expect("validated chunk cardinality").clone())
                .collect()
        })
    }
    /// Replace a reusable row buffer without allocating a new row for each
    /// evaluation. An invalid index leaves the supplied buffer unchanged.
    pub fn read_row(&self, index: usize, row: &mut Row) -> Result<()> {
        if index >= self.count {
            return Err(Error::Internal("chunk row index out of bounds".into()));
        }
        row.clear();
        row.extend(self.columns.iter().map(|column| {
            column
                .get(index)
                .expect("validated chunk cardinality")
                .clone()
        }));
        Ok(())
    }
}
