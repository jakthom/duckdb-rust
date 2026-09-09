//! Incremental access retains the statement's row identities and visibility.
use super::RowId;
use crate::{
    common::{
        DataType, Error, Result, Row,
        type_registry::BoundType,
        vector::{DataChunk, Vector},
    },
    parallel::QueryContext,
};

/// Owned scan values with stable physical identities. Single-row access avoids
/// a column conversion; bulk access can share columns with execution. Both
/// representations survive the cursor and later writes to the source table.
pub struct ScanBatch(BatchData);

enum BatchData {
    Single {
        id: RowId,
        row: Row,
        types: std::sync::Arc<[DataType]>,
    },
    Columns {
        row_ids: Vec<RowId>,
        data: DataChunk,
    },
}

impl ScanBatch {
    pub fn new(row_ids: Vec<RowId>, data: DataChunk) -> Result<Self> {
        if row_ids.len() != data.len() {
            return Err(Error::Internal(
                "scan identities differ from batch cardinality".into(),
            ));
        }
        Ok(Self(BatchData::Columns { row_ids, data }))
    }
    pub fn single(id: RowId, row: Row, types: std::sync::Arc<[DataType]>) -> Result<Self> {
        if row.len() != types.len()
            || row
                .iter()
                .zip(types.iter())
                .any(|(value, data_type)| !value.fits_type(data_type))
        {
            return Err(Error::Internal(
                "scan row differs from its declared physical types".into(),
            ));
        }
        Ok(Self(BatchData::Single { id, row, types }))
    }
    pub fn len(&self) -> usize {
        match &self.0 {
            BatchData::Single { .. } => 1,
            BatchData::Columns { data, .. } => data.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn into_data(self) -> Result<DataChunk> {
        match self.0 {
            BatchData::Single { row, types, .. } => DataChunk::from_rows(&types, &[row]),
            BatchData::Columns { data, .. } => Ok(data),
        }
    }
    /// A single row is borrowed directly; column input fills the caller's
    /// reusable buffer. Neither path changes the scan batch.
    pub fn read_row<'a>(&'a self, index: usize, buffer: &'a mut Row) -> Result<&'a Row> {
        match &self.0 {
            BatchData::Single { row, .. } if index == 0 => Ok(row),
            BatchData::Single { .. } => Err(Error::Internal("scan row index out of bounds".into())),
            BatchData::Columns { data, .. } => {
                data.read_row(index, buffer)?;
                Ok(buffer)
            }
        }
    }
    pub fn select(self, selection: &[usize]) -> Result<DataChunk> {
        let identity = selection.iter().copied().eq(0..self.len());
        let data = self.into_data()?;
        if identity {
            Ok(data)
        } else {
            data.select(selection)
        }
    }
    /// Validate logical payloads before predicates can discard any input.
    /// Constructors already enforce physical types and identity cardinality.
    pub fn validate(&self, expected: &[BoundType], context: &QueryContext) -> Result<()> {
        let width = match &self.0 {
            BatchData::Single { types, .. } => types.len(),
            BatchData::Columns { data, .. } => data.columns().len(),
        };
        if width != expected.len() {
            return Err(Error::Internal(
                "scan batch differs from its expected schema".into(),
            ));
        }
        for (index, data_type) in expected.iter().enumerate() {
            let actual = match &self.0 {
                BatchData::Single { types, .. } => &types[index],
                BatchData::Columns { data, .. } => data.columns()[index].data_type(),
            };
            if actual != data_type.data_type() {
                return Err(Error::Internal(
                    "scan batch differs from its expected schema".into(),
                ));
            }
            if data_type.requires_logical_validation() {
                let validate = |value| {
                    data_type
                        .validate(value, context)
                        .map_err(|error| match error {
                            Error::Conversion(_) => Error::Internal(
                                "table scan returned an invalid logical value".into(),
                            ),
                            other => other,
                        })
                };
                match &self.0 {
                    BatchData::Single { row, .. } => validate(&row[index])?,
                    BatchData::Columns { data, .. } => {
                        for value in data.columns()[index].values() {
                            validate(value)?;
                        }
                    }
                }
            }
        }
        context.check()
    }
    pub fn rows(&self) -> impl Iterator<Item = (RowId, Row)> + '_ {
        (0..self.len()).map(|index| match &self.0 {
            BatchData::Single { id, row, .. } => (*id, row.clone()),
            BatchData::Columns { row_ids, data } => {
                let mut row = Vec::with_capacity(data.columns().len());
                data.read_row(index, &mut row)
                    .expect("validated scan cardinality");
                (row_ids[index], row)
            }
        })
    }
}

/// A cursor is local to its driver. Each call returns at most `max_rows` rows
/// in an owned batch, with no replay. None is permanent exhaustion; Some is nonempty.
/// Dropping a cursor releases its retained snapshot and performs no mutation.
pub trait TableScan {
    fn next(&mut self, max_rows: usize, context: &QueryContext) -> Result<Option<ScanBatch>>;
}

/// Engine consumers validate foreign cursor output before interpreting it.
pub fn next_batch(
    scan: &mut dyn TableScan,
    max_rows: usize,
    context: &QueryContext,
) -> Result<Option<ScanBatch>> {
    let max_rows = context.batch_demand(max_rows)?;
    let batch = scan.next(max_rows, context)?;
    if let Some(batch) = &batch
        && (batch.is_empty() || batch.len() > max_rows)
    {
        return Err(Error::Internal(
            "table scan violated batch cardinality".into(),
        ));
    }
    context.check()?;
    Ok(batch)
}

pub(crate) struct SnapshotScan<'a> {
    pub rows: std::collections::btree_map::Iter<'a, RowId, Row>,
    pub types: std::sync::Arc<[DataType]>,
    pub finished: bool,
}

impl TableScan for SnapshotScan<'_> {
    fn next(&mut self, max_rows: usize, context: &QueryContext) -> Result<Option<ScanBatch>> {
        if self.finished {
            return Ok(None);
        }
        let result = (|| {
            let max_rows = context.batch_demand(max_rows)?;
            let count = max_rows.min(self.rows.len());
            if count == 0 {
                return Ok(None);
            }
            if count == 1 {
                let (&id, row) = self.rows.next().expect("nonempty snapshot cursor");
                return ScanBatch::single(id, row.clone(), self.types.clone()).map(Some);
            }
            let mut ids = Vec::with_capacity(count);
            let mut columns = self
                .types
                .iter()
                .map(|_| Vec::with_capacity(count))
                .collect::<Vec<_>>();
            for _ in 0..max_rows {
                context.check()?;
                let Some((&id, row)) = self.rows.next() else {
                    break;
                };
                if row.len() != columns.len() {
                    return Err(Error::Internal("row width differs from schema".into()));
                }
                ids.push(id);
                for (column, value) in columns.iter_mut().zip(row) {
                    column.push(value.clone());
                }
            }
            let columns = columns
                .into_iter()
                .zip(self.types.iter())
                .map(|(values, data_type)| Vector::flat(data_type.clone(), values))
                .collect::<Result<_>>()?;
            ScanBatch::new(ids, DataChunk::new(columns, count)?).map(Some)
        })();
        if !matches!(result, Ok(Some(_))) {
            self.finished = true;
        }
        result
    }
}
