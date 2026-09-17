use std::sync::Arc;

use super::{DataType, Error, Result, Row, Value};

#[derive(Clone, Debug)]
enum Encoding {
    /// Typed all-valid flats are authoritative physical storage. Generic,
    /// nullable and mixed columns retain their exact logical representation.
    FlatValues(Arc<Vec<Value>>),
    /// All-valid DOUBLE values retain their native lane.  This is deliberately
    /// separate from generic values: NULL and selected vectors still use the
    /// authoritative generic representation.
    FlatDouble(Arc<Vec<f64>>),
    FlatSigned(SignedLanes),
    FlatDecimalI64(Arc<Vec<i64>>),
    Constant(Value),
    Dictionary(Arc<Vector>, Arc<[usize]>),
    /// Immutable table storage retains CTAS batches without first copying all
    /// payloads into a second table-wide flat allocation.  A scan-sized slice
    /// that lies within one segment becomes that segment's ordinary vector,
    /// so scalar and aggregate kernels retain their existing flat fast paths.
    Chunks(Arc<[Vector]>, Arc<[usize]>),
}

/// The physical width of an all-valid signed column is part of its storage
/// contract, rather than an implementation detail of `Value::Integer`.  Keep
/// the wide logical scalar at the encoding boundary so scans of ordinary SQL
/// integer columns do not pay the HUGEINT-sized backing cost.
#[derive(Clone, Debug)]
enum SignedLanes {
    Tiny(Arc<Vec<i8>>),
    Small(Arc<Vec<i16>>),
    Integer(Arc<Vec<i32>>),
    Big(Arc<Vec<i64>>),
    Huge(Arc<Vec<i128>>),
}

/// A checked scalar view over signed physical storage. This distinguishes a
/// SQL NULL from a representation that cannot supply an i64 without first
/// constructing a logical `Value`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignedI64At {
    Value(i64),
    Null,
    Unsupported,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SignedLanes {
    fn from_values(data_type: &DataType, values: &[Value]) -> Self {
        match data_type {
            DataType::TinyInt => Self::Tiny(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value as i8,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            DataType::SmallInt => Self::Small(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value as i16,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            DataType::Integer => Self::Integer(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value as i32,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            DataType::BigInt => Self::Big(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value as i64,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            DataType::HugeInt => Self::Huge(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            _ => unreachable!("signed lanes require a signed integer type"),
        }
    }

    fn value(&self, index: usize) -> Option<i128> {
        match self {
            Self::Tiny(values) => values.get(index).copied().map(i128::from),
            Self::Small(values) => values.get(index).copied().map(i128::from),
            Self::Integer(values) => values.get(index).copied().map(i128::from),
            Self::Big(values) => values.get(index).copied().map(i128::from),
            Self::Huge(values) => values.get(index).copied(),
        }
    }

    #[inline(always)]
    fn i64_value(&self, index: usize) -> Option<i64> {
        match self {
            Self::Tiny(values) => values.get(index).copied().map(i64::from),
            Self::Small(values) => values.get(index).copied().map(i64::from),
            Self::Integer(values) => values.get(index).copied().map(i64::from),
            Self::Big(values) => values.get(index).copied(),
            Self::Huge(values) => values
                .get(index)
                .copied()
                .and_then(|value| value.try_into().ok()),
        }
    }

    fn append_values(&self, range: std::ops::Range<usize>, output: &mut Vec<Value>) {
        match self {
            Self::Tiny(values) => output.extend(
                values[range]
                    .iter()
                    .copied()
                    .map(i128::from)
                    .map(Value::Integer),
            ),
            Self::Small(values) => output.extend(
                values[range]
                    .iter()
                    .copied()
                    .map(i128::from)
                    .map(Value::Integer),
            ),
            Self::Integer(values) => output.extend(
                values[range]
                    .iter()
                    .copied()
                    .map(i128::from)
                    .map(Value::Integer),
            ),
            Self::Big(values) => output.extend(
                values[range]
                    .iter()
                    .copied()
                    .map(i128::from)
                    .map(Value::Integer),
            ),
            Self::Huge(values) => output.extend(values[range].iter().copied().map(Value::Integer)),
        }
    }

    fn append_indices(&self, indices: &[usize], offset: usize, output: &mut Vec<Value>) {
        match self {
            Self::Tiny(values) => output.extend(
                indices
                    .iter()
                    .map(|&index| values[offset + index])
                    .map(i128::from)
                    .map(Value::Integer),
            ),
            Self::Small(values) => output.extend(
                indices
                    .iter()
                    .map(|&index| values[offset + index])
                    .map(i128::from)
                    .map(Value::Integer),
            ),
            Self::Integer(values) => output.extend(
                indices
                    .iter()
                    .map(|&index| values[offset + index])
                    .map(i128::from)
                    .map(Value::Integer),
            ),
            Self::Big(values) => output.extend(
                indices
                    .iter()
                    .map(|&index| values[offset + index])
                    .map(i128::from)
                    .map(Value::Integer),
            ),
            Self::Huge(values) => output.extend(
                indices
                    .iter()
                    .map(|&index| values[offset + index])
                    .map(Value::Integer),
            ),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn same_flat_backing(left: &Encoding, right: &Encoding) -> bool {
    match (left, right) {
        (Encoding::FlatValues(left), Encoding::FlatValues(right)) => Arc::ptr_eq(left, right),
        (Encoding::FlatDouble(left), Encoding::FlatDouble(right)) => Arc::ptr_eq(left, right),
        (
            Encoding::FlatSigned(SignedLanes::Tiny(left)),
            Encoding::FlatSigned(SignedLanes::Tiny(right)),
        ) => Arc::ptr_eq(left, right),
        (
            Encoding::FlatSigned(SignedLanes::Small(left)),
            Encoding::FlatSigned(SignedLanes::Small(right)),
        ) => Arc::ptr_eq(left, right),
        (
            Encoding::FlatSigned(SignedLanes::Integer(left)),
            Encoding::FlatSigned(SignedLanes::Integer(right)),
        ) => Arc::ptr_eq(left, right),
        (
            Encoding::FlatSigned(SignedLanes::Big(left)),
            Encoding::FlatSigned(SignedLanes::Big(right)),
        ) => Arc::ptr_eq(left, right),
        (
            Encoding::FlatSigned(SignedLanes::Huge(left)),
            Encoding::FlatSigned(SignedLanes::Huge(right)),
        ) => Arc::ptr_eq(left, right),
        (Encoding::FlatDecimalI64(left), Encoding::FlatDecimalI64(right)) => {
            Arc::ptr_eq(left, right)
        }
        _ => false,
    }
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
                    && let Some(first) = chunk.value(0)
                {
                    numeric_ascending &= numeric_le(&last, &first);
                }
                previous = chunk.value(chunk.len().saturating_sub(1));
            }
        }
        Ok(Self {
            data_type,
            encoding: Encoding::Chunks(chunks.into(), offsets.into()),
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
            && matches!(
                first.encoding,
                Encoding::FlatValues(_)
                    | Encoding::FlatDouble(_)
                    | Encoding::FlatSigned(_)
                    | Encoding::FlatDecimalI64(_)
            )
        {
            let mut end = first.offset;
            if columns.iter().all(|column| {
                let contiguous =
                    column.offset == end && same_flat_backing(&first.encoding, &column.encoding);
                end = column.offset + column.count;
                contiguous
            }) {
                return Ok(Self {
                    count,
                    ..first.clone()
                });
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
        let mut numeric_ascending = true;
        let mut previous = None;
        for value in values {
            output.push(match value? {
                Some(value) => {
                    numeric_ascending &= previous.is_none_or(|previous| previous <= value);
                    previous = Some(value);
                    Value::Integer(value as i128)
                }
                None => {
                    all_valid = false;
                    numeric_ascending = false;
                    Value::Null
                }
            });
        }
        let count = output.len();
        let encoding = if all_valid {
            Encoding::FlatSigned(SignedLanes::from_values(&DataType::BigInt, &output))
        } else {
            Encoding::FlatValues(Arc::new(output))
        };
        Ok(Self {
            data_type: DataType::BigInt,
            offset: 0,
            count,
            encoding,
            all_valid,
            numeric_ascending,
        })
    }
    /// Construct an all-valid BIGINT column whose producer already owns and
    /// validated the native physical lane.
    pub(crate) fn bigints_prevalidated(values: Vec<i64>) -> Self {
        Self::bigints_prevalidated_with_order(values, false)
    }
    /// Preserve a producer's single-pass ascending proof with an all-valid
    /// native BIGINT lane. The caller must derive the flag from every value.
    pub(crate) fn bigints_prevalidated_with_order(
        values: Vec<i64>,
        numeric_ascending: bool,
    ) -> Self {
        let count = values.len();
        Self {
            data_type: DataType::BigInt,
            offset: 0,
            count,
            encoding: Encoding::FlatSigned(SignedLanes::Big(Arc::new(values))),
            all_valid: true,
            numeric_ascending,
        }
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
        let count = output.len();
        let encoding = if all_valid {
            Encoding::FlatSigned(SignedLanes::from_values(&DataType::HugeInt, &output))
        } else {
            Encoding::FlatValues(Arc::new(output))
        };
        Ok(Self {
            data_type: DataType::HugeInt,
            count,
            offset: 0,
            encoding,
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
            encoding: Encoding::FlatValues(Arc::new(output)),
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
        let mut numeric_ascending = true;
        let mut previous = None;
        for &value in &coefficients {
            numeric_ascending &= previous.is_none_or(|previous| previous <= value);
            previous = Some(value);
        }
        let count = coefficients.len();
        Ok(Self {
            data_type: DataType::Decimal { width, scale },
            encoding: Encoding::FlatDecimalI64(Arc::new(coefficients)),
            offset: 0,
            count,
            all_valid: true,
            numeric_ascending,
        })
    }
    /// Transfer coefficients after a cast kernel has checked the declared
    /// range.  Do not rescan them here: this constructor is deliberately
    /// narrower than `try_decimal_i64` and remains crate-private.
    pub(crate) fn decimal_i64_prevalidated(
        data_type: DataType,
        coefficients: Vec<i64>,
        numeric_ascending: bool,
    ) -> Self {
        debug_assert!(matches!(data_type, DataType::Decimal { width: 1..=18, .. }));
        Self {
            data_type,
            count: coefficients.len(),
            encoding: Encoding::FlatDecimalI64(Arc::new(coefficients)),
            offset: 0,
            all_valid: true,
            // The producing cast checks this while it converts the values,
            // avoiding a second scan and retaining the proof for range
            // predicates.
            numeric_ascending,
        }
    }
    /// Construct an all-valid DOUBLE column from values already known to be
    /// DOUBLE payloads.  IEEE special values are valid SQL DOUBLE values.
    pub(crate) fn try_doubles(values: Vec<f64>) -> Result<Self> {
        Ok(Self {
            data_type: DataType::Double,
            count: values.len(),
            encoding: Encoding::FlatDouble(Arc::new(values)),
            offset: 0,
            all_valid: true,
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
        // Validate the whole logical column before transferring it into the
        // authoritative physical lane.  A NULL deliberately keeps the exact
        // generic representation and therefore its original fallback rules.
        let count = values.len();
        let encoding = if all_valid && data_type.is_signed_integer() {
            Encoding::FlatSigned(SignedLanes::from_values(&data_type, &values))
        } else if all_valid && matches!(data_type, DataType::Decimal { width: 1..=18, .. }) {
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
            Encoding::FlatDecimalI64(Arc::new(coefficients))
        } else if all_valid && data_type == DataType::Double {
            Encoding::FlatDouble(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Double(value) => *value,
                        _ => unreachable!("validated DOUBLE column"),
                    })
                    .collect(),
            ))
        } else {
            Encoding::FlatValues(Arc::new(values))
        };
        Ok(Self {
            data_type,
            offset: 0,
            count,
            encoding,
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
    /// Transform the immediate parent of a dictionary while retaining this
    /// vector's checked selection and view. The mapper owns only the parent;
    /// it cannot replace, reorder or revalidate the existing selection.
    pub fn map_dictionary_parent(
        &self,
        data_type: DataType,
        mapper: impl FnOnce(&Self) -> Result<Self>,
    ) -> Result<Self> {
        let Encoding::Dictionary(parent, selection) = &self.encoding else {
            return Err(Error::Internal(
                "dictionary parent mapping requires dictionary encoding".into(),
            ));
        };
        let mapped = mapper(parent)?;
        if mapped.data_type != data_type || mapped.len() != parent.len() {
            return Err(Error::Internal(
                "mapped dictionary parent differs in type or cardinality".into(),
            ));
        }
        let mapped = Arc::new(mapped);
        Ok(Self {
            data_type,
            encoding: Encoding::Dictionary(mapped.clone(), selection.clone()),
            offset: self.offset,
            count: self.count,
            all_valid: self.all_valid && mapped.all_valid,
            // `self` establishes that this unchanged selection is ordered;
            // the mapped parent supplies the independent physical order proof.
            numeric_ascending: self.numeric_ascending && mapped.numeric_ascending,
        })
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
    /// Resolve a logical value by ownership. Typed physical lanes cannot
    /// synthesize a borrowed `Value`, so all encoding-transparent consumers
    /// use this single owned seam.
    pub fn value(&self, index: usize) -> Option<Value> {
        if index >= self.count {
            return None;
        }
        let index = self.offset + index;
        match &self.encoding {
            Encoding::FlatValues(v) => v.get(index).cloned(),
            Encoding::FlatDouble(v) => v.get(index).copied().map(Value::Double),
            Encoding::FlatSigned(v) => v.value(index).map(Value::Integer),
            Encoding::FlatDecimalI64(v) => {
                v.get(index).copied().map(|value| match self.data_type {
                    DataType::Decimal { width, scale } => Value::Decimal {
                        value: i128::from(value),
                        width,
                        scale,
                    },
                    _ => unreachable!("decimal physical lane requires decimal type"),
                })
            }
            Encoding::Constant(v) => Some(v.clone()),
            Encoding::Dictionary(v, s) => s.get(index).and_then(|&i| v.value(i)),
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= index)
                    .saturating_sub(1);
                chunks
                    .get(segment)
                    .and_then(|chunk| chunk.value(index - offsets[segment]))
            }
        }
    }
    /// Compatibility spelling for the owned scalar access seam.  This is not
    /// a borrowed accessor: callers which need a `&Value` must keep the owned
    /// result alive locally.
    pub fn get(&self, index: usize) -> Option<Value> {
        self.value(index)
    }
    pub fn values(&self) -> impl Iterator<Item = Value> + '_ {
        (0..self.len()).filter_map(|i| self.value(i))
    }
    /// Append owned values in logical order, preserving slices and selections.
    /// Flat and selected-flat columns avoid repeated encoding dispatch. Output
    /// grows by exactly `len`; the source remains immutable and independently owned.
    pub fn append_to(&self, output: &mut Vec<Value>) {
        if self.all_valid
            && self.data_type.is_signed_integer()
            && let Encoding::FlatSigned(values) = &self.encoding
        {
            // Physical validation proves every payload is an integer. Copy
            // the inline coefficient without generic heap-owning Value clone
            // dispatch in window preparation and materialization.
            values.append_values(self.offset..self.offset + self.count, output);
            return;
        }
        match &self.encoding {
            Encoding::FlatValues(values) => {
                output.extend_from_slice(&values[self.offset..self.offset + self.count])
            }
            Encoding::FlatDouble(values) => output.extend(
                values[self.offset..self.offset + self.count]
                    .iter()
                    .copied()
                    .map(Value::Double),
            ),
            Encoding::Constant(value) => {
                output.extend(std::iter::repeat_n(value, self.count).cloned())
            }
            Encoding::Dictionary(parent, selection) => {
                let selection = &selection[self.offset..self.offset + self.count];
                if parent.all_valid
                    && let Encoding::FlatSigned(values) = &parent.encoding
                {
                    values.append_indices(selection, parent.offset, output);
                } else if let Some(values) = parent.flat_values() {
                    output.extend(selection.iter().map(|&index| values[index].clone()));
                } else {
                    output.extend(self.values());
                }
            }
            Encoding::FlatSigned(_) | Encoding::FlatDecimalI64(_) | Encoding::Chunks(_, _) => {
                output.extend(self.values())
            }
        }
    }
    /// A borrowed contiguous physical view when this encoding provides one.
    /// The view contains exactly this vector's logical range, including NULLs.
    /// Consumers must retain the general `values` path for other encodings.
    pub fn flat_values(&self) -> Option<&[Value]> {
        match &self.encoding {
            Encoding::FlatValues(values) => Some(&values[self.offset..self.offset + self.count]),
            _ => None,
        }
    }
    /// Borrow an all-valid native DOUBLE lane for an ordinary flat view.
    pub(crate) fn flat_doubles(&self) -> Option<&[f64]> {
        match &self.encoding {
            Encoding::FlatDouble(values) => Some(&values[self.offset..self.offset + self.count]),
            _ => None,
        }
    }
    /// Borrow an all-valid BIGINT lane.  Other signed widths intentionally
    /// retain their declared physical representation.
    pub(crate) fn flat_bigints(&self) -> Option<&[i64]> {
        match &self.encoding {
            Encoding::FlatSigned(SignedLanes::Big(values)) => {
                Some(&values[self.offset..self.offset + self.count])
            }
            _ => None,
        }
    }
    /// Read one native signed coefficient without widening through `Value`.
    /// Only ordinary flat signed views are eligible.
    pub(crate) fn flat_signed_i64_at(&self, index: usize) -> Option<i64> {
        match &self.encoding {
            Encoding::FlatSigned(values) => values.i64_value(self.offset + index),
            _ => None,
        }
    }
    /// Read a logical signed coefficient without constructing a `Value`.
    /// Selection and chunk adapters recurse through their checked logical
    /// indices; unsupported physical forms stay explicit rather than falling
    /// through to the owned scalar seam.
    #[inline(always)]
    pub(crate) fn signed_i64_at(&self, index: usize) -> SignedI64At {
        if index >= self.count {
            return SignedI64At::Unsupported;
        }
        let index = self.offset + index;
        match &self.encoding {
            Encoding::FlatSigned(values) => values
                .i64_value(index)
                .map(SignedI64At::Value)
                .unwrap_or(SignedI64At::Unsupported),
            Encoding::FlatValues(values) => signed_i64_value(values.get(index)),
            Encoding::Constant(value) => signed_i64_value(Some(value)),
            Encoding::Dictionary(parent, selection) => selection
                .get(index)
                .map_or(SignedI64At::Unsupported, |&selected| {
                    parent.signed_i64_at(selected)
                }),
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= index)
                    .saturating_sub(1);
                chunks
                    .get(segment)
                    .map_or(SignedI64At::Unsupported, |chunk| {
                        chunk.signed_i64_at(index - offsets[segment])
                    })
            }
            Encoding::FlatDouble(_) | Encoding::FlatDecimalI64(_) => SignedI64At::Unsupported,
        }
    }
    /// Borrow the compact physical coefficients for a flat DECIMAL(1..=18)
    /// view. Logical values remain authoritative for every fallback and for
    /// encodings whose NULL/selection semantics need resolution.
    pub(crate) fn flat_decimal_i64(&self) -> Option<&[i64]> {
        match &self.encoding {
            Encoding::FlatDecimalI64(values) => {
                Some(&values[self.offset..self.offset + self.count])
            }
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
#[inline(always)]
fn signed_i64_value(value: Option<&Value>) -> SignedI64At {
    match value {
        Some(Value::Null) => SignedI64At::Null,
        Some(Value::Integer(value)) => i64::try_from(*value)
            .map(SignedI64At::Value)
            .unwrap_or(SignedI64At::Unsupported),
        _ => SignedI64At::Unsupported,
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn decimal(value: i128, width: u8) -> Value {
        Value::Decimal {
            value,
            width,
            scale: 2,
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn dictionary_append_borrows_every_signed_lane_through_parent_slices() -> Result<()> {
        for data_type in [
            DataType::TinyInt,
            DataType::SmallInt,
            DataType::Integer,
            DataType::BigInt,
            DataType::HugeInt,
        ] {
            let parent = Arc::new(
                Vector::flat(
                    data_type,
                    vec![
                        Value::Integer(10),
                        Value::Integer(11),
                        Value::Integer(12),
                        Value::Integer(13),
                    ],
                )?
                .slice(1, 3)?,
            );
            let dictionary = parent.select(vec![2, 0, 2, 1])?;
            let mut output = Vec::new();
            dictionary.append_to(&mut output);
            assert_eq!(
                output,
                vec![
                    Value::Integer(13),
                    Value::Integer(11),
                    Value::Integer(13),
                    Value::Integer(12),
                ]
            );
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn dictionary_parent_mapping_retains_nested_sliced_selection_and_rejects_shape_changes()
    -> Result<()> {
        let source = Arc::new(Vector::flat(
            DataType::Varchar,
            vec![
                Value::Varchar("a".into()),
                Value::Null,
                Value::Varchar("é🦆".into()),
                Value::Varchar("duck".into()),
            ],
        )?);
        let immediate = Arc::new(source.select(vec![2, 0, 1, 3])?);
        let nested = immediate.select(vec![3, 0, 2])?.slice(1, 2)?;
        let mapped = nested.map_dictionary_parent(DataType::BigInt, |parent| {
            Vector::flat(
                DataType::BigInt,
                parent
                    .values()
                    .map(|value| match value {
                        Value::Null => Value::Null,
                        Value::Varchar(value) => Value::Integer(value.chars().count() as i128),
                        _ => unreachable!("VARCHAR dictionary parent"),
                    })
                    .collect(),
            )
        })?;
        assert!(mapped.dictionary().is_some());
        assert_eq!(
            mapped.values().collect::<Vec<_>>(),
            vec![Value::Integer(2), Value::Null]
        );
        assert!(
            nested
                .map_dictionary_parent(DataType::BigInt, |_| {
                    Vector::flat(DataType::BigInt, vec![Value::Integer(1)])
                })
                .is_err()
        );
        assert!(
            nested
                .map_dictionary_parent(DataType::BigInt, |parent| {
                    Vector::flat(
                        DataType::Varchar,
                        vec![Value::Varchar("wrong".into()); parent.len()],
                    )
                })
                .is_err()
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
        assert!(
            flat.flat_values().is_none(),
            "narrow decimal lane is authoritative"
        );
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
            vector.values().collect::<Vec<_>>(),
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
        let ordered = Vector::decimal_i64_prevalidated(data_type.clone(), vec![-250, 0, 999], true);
        assert!(ordered.numeric_ascending());
        let unordered = Vector::decimal_i64_prevalidated(data_type, vec![999, -250], false);
        assert!(!unordered.numeric_ascending());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn bigint_constructor_records_only_all_valid_ascending_lanes() -> Result<()> {
        let ascending = Vector::try_bigints([Ok(Some(-2)), Ok(Some(-2)), Ok(Some(4))])?;
        let descending = Vector::try_bigints([Ok(Some(4)), Ok(Some(-2))])?;
        let nullable = Vector::try_bigints([Ok(Some(-2)), Ok(None), Ok(Some(4))])?;
        assert!(ascending.numeric_ascending());
        assert!(!descending.numeric_ascending());
        assert!(!nullable.numeric_ascending());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn signed_i64_accessor_resolves_nullable_encoded_and_chunked_views() -> Result<()> {
        let nullable = Vector::flat(
            DataType::BigInt,
            vec![Value::Integer(7), Value::Null, Value::Integer(-3)],
        )?;
        assert_eq!(nullable.signed_i64_at(0), SignedI64At::Value(7));
        assert_eq!(nullable.signed_i64_at(1), SignedI64At::Null);
        assert_eq!(nullable.signed_i64_at(2), SignedI64At::Value(-3));

        let value = Vector::constant(DataType::BigInt, Value::Integer(9), 3)?;
        let null = Vector::constant(DataType::BigInt, Value::Null, 3)?;
        assert_eq!(value.signed_i64_at(2), SignedI64At::Value(9));
        assert_eq!(null.signed_i64_at(2), SignedI64At::Null);

        let source = Arc::new(Vector::flat(
            DataType::BigInt,
            vec![
                Value::Integer(10),
                Value::Null,
                Value::Integer(30),
                Value::Integer(40),
            ],
        )?);
        let selected = Arc::new(source.select(vec![3, 1, 0, 2])?);
        let nested = selected.select(vec![2, 0, 1])?.slice(1, 2)?;
        assert_eq!(nested.signed_i64_at(0), SignedI64At::Value(40));
        assert_eq!(nested.signed_i64_at(1), SignedI64At::Null);

        let chunks = Vector::chunked(
            DataType::BigInt,
            vec![
                Vector::flat(DataType::BigInt, vec![Value::Null, Value::Integer(1)])?,
                Vector::flat(DataType::BigInt, vec![Value::Integer(2)])?,
            ],
        )?;
        assert_eq!(chunks.signed_i64_at(0), SignedI64At::Null);
        assert_eq!(chunks.signed_i64_at(1), SignedI64At::Value(1));
        assert_eq!(chunks.signed_i64_at(2), SignedI64At::Value(2));
        assert_eq!(chunks.signed_i64_at(3), SignedI64At::Unsupported);
        assert_eq!(
            Vector::flat(DataType::Double, vec![Value::Double(1.0)])?.signed_i64_at(0),
            SignedI64At::Unsupported
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn all_valid_signed_lanes_use_declared_widths_and_nullable_values_fallback() -> Result<()> {
        let cases = [
            (
                DataType::TinyInt,
                i128::from(i8::MIN),
                i128::from(i8::MAX),
                1,
            ),
            (
                DataType::SmallInt,
                i128::from(i16::MIN),
                i128::from(i16::MAX),
                2,
            ),
            (
                DataType::Integer,
                i128::from(i32::MIN),
                i128::from(i32::MAX),
                4,
            ),
            (
                DataType::BigInt,
                i128::from(i64::MIN),
                i128::from(i64::MAX),
                8,
            ),
            (DataType::HugeInt, i128::MIN, i128::MAX, 16),
        ];
        for (data_type, minimum, maximum, width) in cases {
            let signed = Vector::flat(
                data_type,
                vec![
                    Value::Integer(minimum),
                    Value::Integer(42),
                    Value::Integer(maximum),
                ],
            )?;
            assert!(signed.flat_values().is_none());
            assert_eq!(
                signed.values().collect::<Vec<_>>(),
                vec![
                    Value::Integer(minimum),
                    Value::Integer(42),
                    Value::Integer(maximum)
                ]
            );
            match &signed.encoding {
                Encoding::FlatSigned(SignedLanes::Tiny(values)) => {
                    assert_eq!(
                        width,
                        std::mem::size_of_val(values.as_slice()) / values.len()
                    )
                }
                Encoding::FlatSigned(SignedLanes::Small(values)) => {
                    assert_eq!(
                        width,
                        std::mem::size_of_val(values.as_slice()) / values.len()
                    )
                }
                Encoding::FlatSigned(SignedLanes::Integer(values)) => {
                    assert_eq!(
                        width,
                        std::mem::size_of_val(values.as_slice()) / values.len()
                    )
                }
                Encoding::FlatSigned(SignedLanes::Big(values)) => {
                    assert_eq!(
                        width,
                        std::mem::size_of_val(values.as_slice()) / values.len()
                    )
                }
                Encoding::FlatSigned(SignedLanes::Huge(values)) => {
                    assert_eq!(
                        width,
                        std::mem::size_of_val(values.as_slice()) / values.len()
                    )
                }
                _ => panic!("all-valid signed column requires a signed lane"),
            }
        }

        let nullable = Vector::flat(DataType::BigInt, vec![Value::Integer(1), Value::Null])?;
        assert!(nullable.flat_values().is_some());
        let wide = Vector::flat(
            DataType::Decimal {
                width: 19,
                scale: 0,
            },
            vec![Value::Decimal {
                value: 1,
                width: 19,
                scale: 0,
            }],
        )?;
        assert!(wide.flat_decimal_i64().is_none());
        assert!(wide.flat_values().is_some());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn narrow_signed_lanes_preserve_slices_dictionaries_and_concatenation() -> Result<()> {
        let source = Arc::new(Vector::flat(
            DataType::TinyInt,
            vec![
                Value::Integer(i128::from(i8::MIN)),
                Value::Integer(-1),
                Value::Integer(0),
                Value::Integer(i128::from(i8::MAX)),
            ],
        )?);
        let prefix = source.slice(0, 2)?;
        let suffix = source.slice(2, 2)?;
        let contiguous = Vector::concatenate(DataType::TinyInt, &[prefix, suffix])?;
        assert_eq!(
            contiguous.values().collect::<Vec<_>>(),
            source.values().collect::<Vec<_>>(),
        );
        assert!(contiguous.flat_values().is_none());
        let selected = source.select(vec![3, 0, 3])?;
        assert_eq!(
            selected.values().collect::<Vec<_>>(),
            vec![
                Value::Integer(i128::from(i8::MAX)),
                Value::Integer(i128::from(i8::MIN)),
                Value::Integer(i128::from(i8::MAX)),
            ]
        );
        assert!(selected.flat_values().is_none());
        let copied = Vector::concatenate(
            DataType::TinyInt,
            &[source.slice(3, 1)?, source.slice(0, 1)?],
        )?;
        assert_eq!(
            copied.values().collect::<Vec<_>>(),
            vec![
                Value::Integer(i128::from(i8::MAX)),
                Value::Integer(i128::from(i8::MIN))
            ]
        );
        assert!(copied.flat_values().is_none());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn chunked_storage_retains_segments_and_recovers_flat_scan_slices() -> Result<()> {
        let first = Vector::try_bigints([Ok(Some(10)), Ok(Some(11))])?;
        let second = Vector::try_bigints([Ok(Some(12)), Ok(Some(13))])?;
        let chunks = Vector::chunked(DataType::BigInt, vec![first.clone(), second.clone()])?;
        assert_eq!(
            chunks.values().collect::<Vec<_>>(),
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
            chunks.slice(1, 2)?.values().collect::<Vec<_>>(),
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
                .map(|v| v.value(i).expect("validated chunk cardinality"))
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
        row.extend(
            self.columns
                .iter()
                .map(|column| column.value(index).expect("validated chunk cardinality")),
        );
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
