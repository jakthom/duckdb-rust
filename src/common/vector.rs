use std::sync::Arc;

use super::{DataType, Error, Result, Row, Value};

#[derive(Clone, Debug)]
enum Encoding {
    Flat(Arc<Vec<Value>>),
    Constant(Value),
    Dictionary(Arc<Vector>, Arc<[usize]>),
    /// Immutable table storage retains CTAS batches without first copying all
    /// payloads into a second table-wide flat allocation.  A scan-sized slice
    /// that lies within one segment becomes that segment's ordinary vector,
    /// so scalar and aggregate kernels retain their existing flat fast paths.
    Chunks(Arc<[Vector]>, Arc<[usize]>),
}

/// Immutable, owning column view. Selection and validity are resolved by `get`.
#[derive(Clone, Debug)]
pub struct Vector {
    data_type: DataType,
    encoding: Encoding,
    /// DuckDB stores DECIMAL widths through 18 as physical signed integers.
    /// Retain that source-shaped lane beside the logical `Value` oracle for
    /// flat, non-NULL columns so numeric kernels do not redispatch the enum.
    decimal_i64: Option<Arc<Vec<i64>>>,
    offset: usize,
    count: usize,
    all_valid: bool,
    numeric_ascending: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Vector {
    /// Combine same-typed immutable input batches without copying their
    /// payloads.  This is storage-facing: execution still observes ordinary
    /// vectors after a scan slices a segment-sized batch.
    pub(crate) fn chunked(data_type: DataType, chunks: Vec<Self>) -> Result<Self> {
        let mut offsets = Vec::with_capacity(chunks.len().saturating_add(1));
        offsets.push(0);
        let mut count = 0usize;
        let mut all_valid = true;
        let mut numeric_ascending = data_type.is_decimal() || data_type.is_unsigned_integer();
        let mut previous = None;
        for chunk in &chunks {
            if chunk.data_type != data_type {
                return Err(Error::Internal("chunked vector type differs".into()));
            }
            count = count
                .checked_add(chunk.len())
                .ok_or_else(|| Error::Resource("chunked vector size overflow".into()))?;
            offsets.push(count);
            all_valid &= chunk.all_valid();
            if numeric_ascending {
                numeric_ascending &= chunk.numeric_ascending();
                if let Some(last) = previous
                    && let Some(first) = chunk.get(0)
                {
                    numeric_ascending &= numeric_le(last, first);
                }
                previous = chunk.get(chunk.len().saturating_sub(1));
            }
        }
        Ok(Self {
            data_type,
            encoding: Encoding::Chunks(chunks.into(), offsets.into()),
            decimal_i64: None,
            offset: 0,
            count,
            all_valid,
            numeric_ascending,
        })
    }
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
        Self::flat(data_type, values)
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
            decimal_i64: None,
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
            decimal_i64: None,
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
            decimal_i64: None,
            all_valid,
            numeric_ascending: false,
        })
    }
    /// Construct a validated, non-NULL narrow DECIMAL column from its native
    /// signed coefficients. This retains the logical values required by the
    /// generic execution contract while transferring the same coefficient
    /// allocation to physical consumers instead of rebuilding it from Values.
    pub(crate) fn try_decimal_i64(data_type: DataType, coefficients: Vec<i64>) -> Result<Self> {
        let DataType::Decimal {
            width: width @ 1..=18,
            scale,
        } = data_type
        else {
            return Err(Error::Internal(
                "narrow decimal coefficients require DECIMAL(1..=18) metadata".into(),
            ));
        };
        let maximum = crate::common::numeric::DECIMAL_POWERS[usize::from(width)];
        if scale > width
            || coefficients
                .iter()
                .any(|value| u128::from(value.unsigned_abs()) >= maximum)
        {
            return Err(Error::Internal(
                "narrow decimal coefficient differs from declared metadata".into(),
            ));
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(coefficients.len())
            .map_err(|_| Error::Resource("cannot allocate narrow DECIMAL logical column".into()))?;
        let mut numeric_ascending = true;
        let mut previous = None;
        for &value in &coefficients {
            numeric_ascending &= previous.is_none_or(|previous| previous <= value);
            previous = Some(value);
            values.push(Value::Decimal {
                value: i128::from(value),
                width,
                scale,
            });
        }
        let count = values.len();
        Ok(Self {
            data_type: DataType::Decimal { width, scale },
            encoding: Encoding::Flat(Arc::new(values)),
            decimal_i64: Some(Arc::new(coefficients)),
            offset: 0,
            count,
            all_valid: true,
            numeric_ascending,
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
        // Validate the complete logical column before allocating an optional
        // physical cache. This preserves the pre-cache type-error precedence
        // and avoids reserving a lane that a NULL would immediately discard.
        let decimal_i64 =
            if all_valid && matches!(data_type, DataType::Decimal { width: 1..=18, .. }) {
                let mut coefficients = Vec::new();
                coefficients.try_reserve_exact(values.len()).map_err(|_| {
                    Error::Resource("cannot allocate DECIMAL coefficient column".into())
                })?;
                for value in &values {
                    let Value::Decimal { value, .. } = value else {
                        unreachable!("validated narrow DECIMAL column");
                    };
                    coefficients.push(i64::try_from(*value).map_err(|_| {
                        Error::Internal("narrow DECIMAL coefficient exceeds i64".into())
                    })?);
                }
                Some(Arc::new(coefficients))
            } else {
                None
            };
        Ok(Self {
            data_type,
            offset: 0,
            count: values.len(),
            encoding: Encoding::Flat(Arc::new(values)),
            decimal_i64,
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
            decimal_i64: None,
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
            decimal_i64: None,
        }
    }
    /// An owning contiguous view, with no payload copy or selection allocation.
    /// Bounds are relative to this view, including for nested selections.
    pub fn slice(&self, offset: usize, count: usize) -> Result<Self> {
        if offset > self.count || count > self.count - offset {
            return Err(Error::Internal("vector slice out of bounds".into()));
        }
        if let Encoding::Chunks(chunks, offsets) = &self.encoding {
            let start = self.offset + offset;
            let end = start + count;
            let segment = offsets
                .partition_point(|&end| end <= start)
                .saturating_sub(1);
            if let Some(chunk) = chunks.get(segment)
                && end <= offsets[segment + 1]
            {
                return chunk.slice(start - offsets[segment], count);
            }
        }
        Ok(Self {
            data_type: self.data_type.clone(),
            encoding: self.encoding.clone(),
            decimal_i64: self.decimal_i64.clone(),
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
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= index)
                    .saturating_sub(1);
                chunks
                    .get(segment)
                    .and_then(|chunk| chunk.get(index - offsets[segment]))
            }
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
            Encoding::Chunks(_, _) => output.extend(self.values().cloned()),
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
    /// Borrow the compact physical coefficients for a flat DECIMAL(1..=18)
    /// view. Logical values remain authoritative for every fallback and for
    /// encodings whose NULL/selection semantics need resolution.
    pub(crate) fn flat_decimal_i64(&self) -> Option<&[i64]> {
        if !matches!(self.encoding, Encoding::Flat(_)) {
            return None;
        }
        self.decimal_i64
            .as_ref()
            .map(|values| &values[self.offset..self.offset + self.count])
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

#[cfg(test)]
mod physical_tests {
    use super::*;

    fn decimal(value: i128, width: u8) -> Value {
        Value::Decimal {
            value,
            width,
            scale: 2,
        }
    }

    #[test]
    fn narrow_decimal_lanes_follow_flat_slices_and_decline_other_encodings() -> Result<()> {
        let data_type = DataType::Decimal {
            width: 12,
            scale: 2,
        };
        let flat = Vector::flat(
            data_type.clone(),
            vec![decimal(-100, 12), decimal(0, 12), decimal(250, 12)],
        )?;
        assert_eq!(flat.flat_decimal_i64(), Some(&[-100, 0, 250][..]));
        assert_eq!(flat.clone().flat_decimal_i64(), Some(&[-100, 0, 250][..]));
        assert_eq!(flat.slice(1, 2)?.flat_decimal_i64(), Some(&[0, 250][..]));
        assert_eq!(flat.slice(0, 0)?.flat_decimal_i64(), Some(&[][..]));
        assert_eq!(
            Vector::flat(data_type.clone(), Vec::new())?.flat_decimal_i64(),
            Some(&[][..])
        );
        let contiguous =
            Vector::concatenate(data_type.clone(), &[flat.slice(0, 1)?, flat.slice(1, 2)?])?;
        assert_eq!(contiguous.flat_decimal_i64(), Some(&[-100, 0, 250][..]));
        let noncontiguous =
            Vector::concatenate(data_type.clone(), &[flat.slice(2, 1)?, flat.slice(0, 1)?])?;
        assert_eq!(noncontiguous.flat_decimal_i64(), Some(&[250, -100][..]));
        assert!(
            Vector::flat(
                data_type.clone(),
                vec![decimal(1, 12), Value::Null, decimal(2, 12)]
            )?
            .flat_decimal_i64()
            .is_none()
        );
        assert!(
            Vector::constant(data_type.clone(), decimal(1, 12), 3)?
                .flat_decimal_i64()
                .is_none()
        );
        assert!(
            Arc::new(flat)
                .select(vec![2, 0, 2])?
                .flat_decimal_i64()
                .is_none()
        );
        assert!(
            Vector::flat(
                DataType::Decimal {
                    width: 19,
                    scale: 2,
                },
                vec![decimal(1, 19)]
            )?
            .flat_decimal_i64()
            .is_none()
        );
        let width_18 = DataType::Decimal {
            width: 18,
            scale: 0,
        };
        let extrema = Vector::flat(
            width_18,
            vec![
                Value::Decimal {
                    value: -999_999_999_999_999_999,
                    width: 18,
                    scale: 0,
                },
                Value::Decimal {
                    value: 999_999_999_999_999_999,
                    width: 18,
                    scale: 0,
                },
            ],
        )?;
        assert_eq!(
            extrema.flat_decimal_i64(),
            Some(&[-999_999_999_999_999_999, 999_999_999_999_999_999][..])
        );
        for mismatched in [
            Value::Decimal {
                value: 1,
                width: 11,
                scale: 2,
            },
            Value::Decimal {
                value: 1,
                width: 12,
                scale: 1,
            },
        ] {
            assert!(Vector::flat(data_type.clone(), vec![mismatched]).is_err());
        }
        Ok(())
    }

    #[test]
    fn direct_narrow_decimal_coefficients_preserve_metadata_and_lanes() -> Result<()> {
        let data_type = DataType::Decimal {
            width: 18,
            scale: 2,
        };
        let vector = Vector::try_decimal_i64(data_type.clone(), vec![-250, 0, 999])?;
        assert_eq!(vector.data_type(), &data_type);
        assert_eq!(vector.flat_decimal_i64(), Some(&[-250, 0, 999][..]));
        assert_eq!(
            vector.values().cloned().collect::<Vec<_>>(),
            vec![decimal(-250, 18), decimal(0, 18), decimal(999, 18)]
        );
        assert!(vector.numeric_ascending());
        assert!(
            Vector::try_decimal_i64(
                DataType::Decimal {
                    width: 19,
                    scale: 2
                },
                vec![1],
            )
            .is_err()
        );
        assert!(
            Vector::try_decimal_i64(
                DataType::Decimal {
                    width: 18,
                    scale: 2
                },
                vec![1_000_000_000_000_000_000],
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn chunked_storage_retains_segments_and_recovers_flat_scan_slices() -> Result<()> {
        let first = Vector::try_bigints([Ok(Some(10)), Ok(Some(11))])?;
        let second = Vector::try_bigints([Ok(Some(12)), Ok(Some(13))])?;
        let chunks = Vector::chunked(DataType::BigInt, vec![first.clone(), second.clone()])?;
        assert_eq!(
            chunks.values().cloned().collect::<Vec<_>>(),
            vec![
                Value::Integer(10),
                Value::Integer(11),
                Value::Integer(12),
                Value::Integer(13),
            ]
        );
        assert!(chunks.flat_values().is_none());
        assert_eq!(chunks.slice(2, 2)?.flat_values(), second.flat_values());
        assert_eq!(
            chunks.slice(1, 2)?.values().cloned().collect::<Vec<_>>(),
            vec![Value::Integer(11), Value::Integer(12)]
        );
        Ok(())
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
        if selection.iter().copied().eq(0..self.count) {
            return Ok(self.clone());
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Append an exact physical identity for values eligible for dictionary
/// encoding. The declared vector type supplies logical metadata, so ENUM and
/// extension payloads retain their physical ordinal/bytes here. False keeps an
/// opaque representation flat.
pub(crate) fn append_physical_identity(value: &Value, output: &mut Vec<u8>) -> bool {
    use super::{NestedPayload, TemporalValue};
    match value {
        Value::Null => output.push(0),
        Value::Boolean(value) => number(1, &[*value as u8], output),
        Value::Integer(value) => number(2, &value.to_le_bytes(), output),
        Value::Unsigned(value) => number(3, &value.to_le_bytes(), output),
        Value::Decimal {
            value,
            width,
            scale,
        } => {
            output.push(4);
            output.extend_from_slice(&value.to_le_bytes());
            output.extend_from_slice(&[*width, *scale]);
        }
        Value::Float(value) => number(5, &value.to_bits().to_le_bytes(), output),
        Value::Double(value) => number(6, &value.to_bits().to_le_bytes(), output),
        Value::Varchar(value) => bytes(7, value.as_bytes(), output),
        Value::Blob(value) => bytes(8, value, output),
        Value::Bit(_) | Value::Bignum(_) => return false,
        Value::Uuid(value) => number(9, &value.to_le_bytes(), output),
        Value::Enum(value) => number(10, &value.ordinal.to_le_bytes(), output),
        Value::Date(value) => number(11, &value.days().to_le_bytes(), output),
        Value::Temporal(value) => {
            output.push(12);
            match value {
                TemporalValue::Time(value) => number(0, &value.to_le_bytes(), output),
                TemporalValue::TimeNs(value) => number(1, &value.to_le_bytes(), output),
                TemporalValue::TimeTz { micros, offset } => {
                    output.push(2);
                    output.extend_from_slice(&micros.to_le_bytes());
                    output.extend_from_slice(&offset.to_le_bytes());
                }
                TemporalValue::Timestamp(value) => number(3, &value.to_le_bytes(), output),
                TemporalValue::TimestampS(value) => number(4, &value.to_le_bytes(), output),
                TemporalValue::TimestampMs(value) => number(5, &value.to_le_bytes(), output),
                TemporalValue::TimestampNs(value) => number(6, &value.to_le_bytes(), output),
                TemporalValue::TimestampTz(value) => number(7, &value.to_le_bytes(), output),
                TemporalValue::TimestampTzNs(value) => number(8, &value.to_le_bytes(), output),
                TemporalValue::Interval {
                    months,
                    days,
                    micros,
                } => {
                    output.push(9);
                    output.extend_from_slice(&months.to_le_bytes());
                    output.extend_from_slice(&days.to_le_bytes());
                    output.extend_from_slice(&micros.to_le_bytes());
                }
            }
        }
        Value::Nested(value) => {
            output.push(13);
            return match &value.payload {
                NestedPayload::Sequence(values) => sequence(0, values, output),
                NestedPayload::Struct(values) => sequence(1, values, output),
                NestedPayload::Map(entries) => {
                    output.push(2);
                    output.extend_from_slice(&(entries.len() as u64).to_le_bytes());
                    for (key, value) in entries {
                        if !append_physical_identity(key, output)
                            || !append_physical_identity(value, output)
                        {
                            return false;
                        }
                    }
                    true
                }
                NestedPayload::Union { tag, value } => {
                    output.push(3);
                    output.extend_from_slice(&(*tag as u64).to_le_bytes());
                    append_physical_identity(value, output)
                }
                NestedPayload::Variant { .. } => false,
            };
        }
        Value::Extension(value) => bytes(14, &value.bytes, output),
    }
    true
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn number(tag: u8, value: &[u8], output: &mut Vec<u8>) {
    output.push(tag);
    output.extend_from_slice(value);
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bytes(tag: u8, value: &[u8], output: &mut Vec<u8>) {
    output.push(tag);
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sequence(tag: u8, values: &[Value], output: &mut Vec<u8>) -> bool {
    output.push(tag);
    output.extend_from_slice(&(values.len() as u64).to_le_bytes());
    values
        .iter()
        .all(|value| append_physical_identity(value, output))
}
