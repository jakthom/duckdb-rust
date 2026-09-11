use std::sync::Arc;

use super::{DataType, Error, Result, Row, Value};

#[derive(Clone, Debug)]
enum Encoding {
    Flat(Arc<Vec<Value>>),
    Constant(Value),
    Dictionary(Arc<Vector>, Arc<[usize]>),
}

/// Immutable, owning column view. Selection and validity are resolved by `get`.
#[derive(Clone, Debug)]
pub struct Vector {
    data_type: DataType,
    encoding: Encoding,
    offset: usize,
    count: usize,
    all_valid: bool,
    numeric_ascending: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Vector {
    /// Concatenate already validated, identically typed columns in logical
    /// order. No adapter assertion can skip physical validation: every input
    /// was constructed through this module's checked constructors.
    pub fn concatenate(data_type: DataType, columns: &[Self]) -> Result<Self> {
        let mut values = Vec::new();
        let mut count = 0usize;
        for column in columns {
            if column.data_type != data_type {
                return Err(Error::Internal("concatenated vector type differs".into()));
            }
            count = count
                .checked_add(column.len())
                .ok_or_else(|| Error::Resource("concatenated vector size overflow".into()))?;
        }
        if let Some(first) = columns.first()
            && let Encoding::Flat(backing) = &first.encoding
        {
            let mut end = first.offset;
            if columns.iter().all(|column| {
                let contiguous = column.offset == end && matches!(&column.encoding, Encoding::Flat(other) if Arc::ptr_eq(backing, other));
                end = column.offset + column.count;
                contiguous
            }) {
                return Ok(Self { count, ..first.clone() });
            }
        }
        values
            .try_reserve(count)
            .map_err(|_| Error::Resource("cannot allocate concatenated vector".into()))?;
        for column in columns {
            column.append_to(&mut values);
        }
        Ok(Self {
            data_type,
            encoding: Encoding::Flat(Arc::new(values)),
            offset: 0,
            count,
            all_valid: columns.iter().all(Self::all_valid),
            numeric_ascending: false,
        })
    }
    /// Construct BIGINT storage from statically bounded physical values. The
    /// constructor establishes type and validity without a second value scan.
    /// An iterator error discards the partial column and stops consumption.
    pub fn try_bigints(values: impl IntoIterator<Item = Result<Option<i64>>>) -> Result<Self> {
        let values = values.into_iter();
        let mut output = Vec::new();
        output
            .try_reserve(values.size_hint().0)
            .map_err(|_| Error::Resource("cannot allocate BIGINT column".into()))?;
        let mut all_valid = true;
        for value in values {
            output.push(match value? {
                Some(value) => Value::Integer(value as i128),
                None => {
                    all_valid = false;
                    Value::Null
                }
            });
        }
        Ok(Self {
            data_type: DataType::BigInt,
            offset: 0,
            count: output.len(),
            encoding: Encoding::Flat(Arc::new(output)),
            all_valid,
            numeric_ascending: false,
        })
    }
    /// Checked full-width signed output, validating while consuming rather
    /// than rescanning an already statically bounded i128 payload.
    pub fn try_hugeints(values: impl IntoIterator<Item = Result<Option<i128>>>) -> Result<Self> {
        let values = values.into_iter();
        let mut output = Vec::new();
        output
            .try_reserve(values.size_hint().0)
            .map_err(|_| Error::Resource("cannot allocate HUGEINT column".into()))?;
        let mut all_valid = true;
        for value in values {
            output.push(match value? {
                Some(value) => Value::Integer(value),
                None => {
                    all_valid = false;
                    Value::Null
                }
            });
        }
        Ok(Self {
            data_type: DataType::HugeInt,
            count: output.len(),
            offset: 0,
            encoding: Encoding::Flat(Arc::new(output)),
            all_valid,
            numeric_ascending: false,
        })
    }
    /// A single-pass unsigned constructor. The declared width is checked for
    /// every emitted payload; errors stop the input and discard partial output.
    pub fn try_unsigned(
        data_type: DataType,
        values: impl IntoIterator<Item = Result<Option<u128>>>,
    ) -> Result<Self> {
        let bits = data_type
            .unsigned_bits()
            .ok_or_else(|| Error::Internal("unsigned column requires an unsigned type".into()))?;
        let maximum = u128::MAX >> (128 - bits);
        let values = values.into_iter();
        let mut output = Vec::new();
        output
            .try_reserve(values.size_hint().0)
            .map_err(|_| Error::Resource("cannot allocate unsigned column".into()))?;
        let mut all_valid = true;
        for value in values {
            output.push(match value? {
                Some(value) if value <= maximum => Value::Unsigned(value),
                Some(_) => {
                    return Err(Error::Internal(
                        "unsigned column value exceeds declared width".into(),
                    ));
                }
                None => {
                    all_valid = false;
                    Value::Null
                }
            });
        }
        Ok(Self {
            data_type,
            count: output.len(),
            offset: 0,
            encoding: Encoding::Flat(Arc::new(output)),
            all_valid,
            numeric_ascending: false,
        })
    }
    pub fn flat(data_type: DataType, values: Vec<Value>) -> Result<Self> {
        let mut all_valid = true;
        let mut numeric_ascending = data_type.is_decimal() || data_type.is_unsigned_integer();
        let mut previous = None;
        for value in &values {
            if !value.fits_type(&data_type) {
                return Err(Error::Internal(
                    "vector values require explicit conversion to the declared type".into(),
                ));
            }
            all_valid &= !value.is_null();
            if numeric_ascending {
                numeric_ascending =
                    !value.is_null() && previous.is_none_or(|previous| numeric_le(previous, value));
                previous = Some(value);
            }
        }
        Ok(Self {
            data_type,
            offset: 0,
            count: values.len(),
            encoding: Encoding::Flat(Arc::new(values)),
            all_valid,
            numeric_ascending,
        })
    }
    pub fn constant(data_type: DataType, value: Value, count: usize) -> Result<Self> {
        if !value.fits_type(&data_type) {
            return Err(Error::Internal(
                "constant vector requires explicit conversion to the declared type".into(),
            ));
        }
        Ok(Self {
            numeric_ascending: !value.is_null()
                && (data_type.is_decimal() || data_type.is_unsigned_integer()),
            data_type,
            all_valid: !value.is_null(),
            encoding: Encoding::Constant(value),
            offset: 0,
            count,
        })
    }
    pub fn select(self: &Arc<Self>, selection: Vec<usize>) -> Result<Self> {
        if selection.iter().any(|&i| i >= self.len()) {
            return Err(Error::Internal("vector selection out of bounds".into()));
        }
        let ordered = selection.windows(2).all(|pair| pair[0] <= pair[1]);
        Ok(self.selected(selection.into(), ordered))
    }
    // Only checked Vector/DataChunk selection constructors call this helper.
    // Chunk cardinality establishes the same bounds for every column.
    fn selected(self: &Arc<Self>, selection: Arc<[usize]>, ordered: bool) -> Self {
        if matches!(self.encoding, Encoding::Constant(_)) {
            return Self {
                offset: 0,
                count: selection.len(),
                ..self.as_ref().clone()
            };
        }
        Self {
            numeric_ascending: self.numeric_ascending && ordered,
            data_type: self.data_type.clone(),
            offset: 0,
            count: selection.len(),
            all_valid: self.all_valid,
            encoding: Encoding::Dictionary(self.clone(), selection),
        }
    }
    /// An owning contiguous view, with no payload copy or selection allocation.
    /// Bounds are relative to this view, including for nested selections.
    pub fn slice(&self, offset: usize, count: usize) -> Result<Self> {
        if offset > self.count || count > self.count - offset {
            return Err(Error::Internal("vector slice out of bounds".into()));
        }
        Ok(Self {
            data_type: self.data_type.clone(),
            encoding: self.encoding.clone(),
            offset: self.offset + offset,
            count,
            all_valid: self.all_valid,
            numeric_ascending: self.numeric_ascending,
        })
    }
    pub fn data_type(&self) -> &DataType {
        &self.data_type
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// True proves that no logical row is NULL. False is conservative: a
    /// selection or slice may have excluded the parent's NULLs. The proof is
    /// established by constructors and never supplied by an adapter unchecked.
    pub fn all_valid(&self) -> bool {
        self.all_valid || self.is_empty()
    }
    /// Constructor-established physical unsigned/decimal order with no NULLs.
    /// This is not a promise about an adapter's comparison semantics. Only an
    /// adapter that uses physical numeric order may use this proof to search.
    pub fn numeric_ascending(&self) -> bool {
        self.numeric_ascending
    }
    pub fn get(&self, index: usize) -> Option<&Value> {
        if index >= self.count {
            return None;
        }
        let index = self.offset + index;
        match &self.encoding {
            Encoding::Flat(v) => v.get(index),
            Encoding::Constant(v) => Some(v),
            Encoding::Dictionary(v, s) => s.get(index).and_then(|&i| v.get(i)),
        }
    }
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        (0..self.len()).filter_map(|i| self.get(i))
    }
    /// Append owned values in logical order, preserving slices and selections.
    /// Flat and selected-flat columns avoid repeated encoding dispatch. Output
    /// grows by exactly `len`; the source remains immutable and independently owned.
    pub fn append_to(&self, output: &mut Vec<Value>) {
        if self.all_valid
            && self.data_type.is_signed_integer()
            && let Some(values) = self.flat_values()
        {
            // Physical validation proves every payload is an integer. Copy
            // the inline coefficient without generic heap-owning Value clone
            // dispatch in window preparation and materialization.
            output.extend(values.iter().map(|value| match value {
                Value::Integer(value) => Value::Integer(*value),
                _ => unreachable!("validated non-NULL signed column"),
            }));
            return;
        }
        match &self.encoding {
            Encoding::Flat(values) => {
                output.extend_from_slice(&values[self.offset..self.offset + self.count])
            }
            Encoding::Constant(value) => {
                output.extend(std::iter::repeat_n(value, self.count).cloned())
            }
            Encoding::Dictionary(parent, selection) => {
                if let Some(values) = parent.flat_values() {
                    output.extend(
                        selection[self.offset..self.offset + self.count]
                            .iter()
                            .map(|&index| values[index].clone()),
                    );
                } else {
                    output.extend(self.values().cloned());
                }
            }
        }
    }
    /// A borrowed contiguous physical view when this encoding provides one.
    /// The view contains exactly this vector's logical range, including NULLs.
    /// Consumers must retain the general `values` path for other encodings.
    pub fn flat_values(&self) -> Option<&[Value]> {
        match &self.encoding {
            Encoding::Flat(values) => Some(&values[self.offset..self.offset + self.count]),
            _ => None,
        }
    }
    /// The repeated value when every logical row uses a constant encoding.
    pub fn constant_value(&self) -> Option<&Value> {
        match &self.encoding {
            Encoding::Constant(value) => Some(value),
            _ => None,
        }
    }
    /// Borrow the immediate owning dictionary and this view's checked logical
    /// selection. Parent positions identify identical physical inputs, not
    /// merely SQL-equal values. Nested selections remain valid parent views.
    pub fn dictionary(&self) -> Option<(&Arc<Self>, &[usize])> {
        match &self.encoding {
            Encoding::Dictionary(parent, selection) => {
                Some((parent, &selection[self.offset..self.offset + self.count]))
            }
            _ => None,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn numeric_le(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Unsigned(a), Value::Unsigned(b)) => a <= b,
        (Value::Decimal { value: a, .. }, Value::Decimal { value: b, .. }) => a <= b,
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub struct DataChunk {
    columns: Vec<Vector>,
    count: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
    /// Retain a contiguous row range without copying column payloads. Empty
    /// ranges and zero-column chunks obey the same cardinality contract.
    pub fn slice(&self, offset: usize, count: usize) -> Result<Self> {
        if offset > self.count || count > self.count - offset {
            return Err(Error::Internal("chunk slice out of bounds".into()));
        }
        Self::new(
            self.columns
                .iter()
                .map(|column| column.slice(offset, count))
                .collect::<Result<_>>()?,
            count,
        )
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
        let ordered = selection.windows(2).all(|pair| pair[0] <= pair[1]);
        let selection: Arc<[usize]> = selection.into();
        let columns = self
            .columns
            .iter()
            .map(|column| Arc::new(column.clone()).selected(selection.clone(), ordered))
            .collect();
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
