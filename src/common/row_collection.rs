use std::ops::Index;

use super::{Error, Result, Row, Value, vector::DataChunk};

/// Owned, fully materialized rows in one contiguous value allocation. Borrowed
/// iteration and positional access return slices without allocating per row.
/// Consuming iteration explicitly transfers each row into its own Vec.
#[derive(Clone)]
pub struct RowCollection {
    width: usize,
    count: usize,
    values: Vec<Value>,
}

impl RowCollection {
    pub fn new(width: usize) -> Self {
        Self {
            width,
            count: 0,
            values: Vec::new(),
        }
    }
    pub fn from_rows(width: usize, rows: Vec<Row>) -> Result<Self> {
        if rows.iter().any(|row| row.len() != width) {
            return Err(Error::Internal(
                "materialized row width differs from schema".into(),
            ));
        }
        let mut output = Self::new(width);
        output.reserve(rows.len())?;
        output.count = rows.len();
        output.values.extend(rows.into_iter().flatten());
        Ok(output)
    }
    fn reserve(&mut self, count: usize) -> Result<()> {
        self.count
            .checked_add(count)
            .ok_or_else(|| Error::Resource("materialized row count overflow".into()))?;
        let values = self
            .width
            .checked_mul(count)
            .ok_or_else(|| Error::Resource("materialized value count overflow".into()))?;
        self.values
            .try_reserve(values)
            .map_err(|_| Error::Resource("cannot allocate materialized rows".into()))
    }
    /// Shape/allocation errors leave the collection unchanged. Payloads are
    /// copied from an already validated owning chunk, without row temporaries.
    pub fn append(&mut self, chunk: &DataChunk) -> Result<()> {
        if chunk.columns().len() != self.width {
            return Err(Error::Internal(
                "materialized chunk width differs from schema".into(),
            ));
        }
        self.reserve(chunk.len())?;
        if let [column] = chunk.columns() {
            column.append_to(&mut self.values);
        } else {
            for index in 0..chunk.len() {
                self.values.extend(chunk.columns().iter().map(|column| {
                    column
                        .get(index)
                        .expect("validated chunk cardinality")
                        .clone()
                }));
            }
        }
        self.count += chunk.len();
        Ok(())
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn width(&self) -> usize {
        self.width
    }
    #[inline]
    pub fn get(&self, index: usize) -> Option<&[Value]> {
        (index < self.count).then(|| &self.values[index * self.width..(index + 1) * self.width])
    }
    pub fn iter(&self) -> Rows<'_> {
        Rows {
            collection: self,
            positions: 0..self.count,
        }
    }
    pub fn into_rows(self) -> Vec<Row> {
        self.into_iter().collect()
    }
}

impl Index<usize> for RowCollection {
    type Output = [Value];
    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        self.get(index)
            .expect("materialized row index out of bounds")
    }
}

pub struct Rows<'a> {
    collection: &'a RowCollection,
    positions: std::ops::Range<usize>,
}
impl<'a> Iterator for Rows<'a> {
    type Item = &'a [Value];
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.positions
            .next()
            .and_then(|index| self.collection.get(index))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.positions.size_hint()
    }
}
impl DoubleEndedIterator for Rows<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.positions
            .next_back()
            .and_then(|index| self.collection.get(index))
    }
}
impl ExactSizeIterator for Rows<'_> {}
impl std::iter::FusedIterator for Rows<'_> {}
impl<'a> IntoIterator for &'a RowCollection {
    type Item = &'a [Value];
    type IntoIter = Rows<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct IntoRows {
    width: usize,
    remaining: usize,
    values: std::vec::IntoIter<Value>,
}
impl Iterator for IntoRows {
    type Item = Row;
    fn next(&mut self) -> Option<Row> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        Some(self.values.by_ref().take(self.width).collect())
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for IntoRows {}
impl std::iter::FusedIterator for IntoRows {}
impl IntoIterator for RowCollection {
    type Item = Row;
    type IntoIter = IntoRows;
    fn into_iter(self) -> IntoRows {
        IntoRows {
            width: self.width,
            remaining: self.count,
            values: self.values.into_iter(),
        }
    }
}
impl std::fmt::Debug for RowCollection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self).finish()
    }
}
impl PartialEq for RowCollection {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other)
    }
}
impl PartialEq<Vec<Row>> for RowCollection {
    fn eq(&self, other: &Vec<Row>) -> bool {
        self.iter().eq(other.iter().map(Vec::as_slice))
    }
}
impl PartialEq<RowCollection> for Vec<Row> {
    fn eq(&self, other: &RowCollection) -> bool {
        other == self
    }
}
impl serde::Serialize for RowCollection {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}
