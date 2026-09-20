use std::{collections::HashSet, ops::Range, sync::Arc};

use super::{DataType, Error, NestedPayload, NestedType, NestedValue, Result, Row, Value};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn materialized_value_bytes(value: &Value) -> Result<usize> {
    std::mem::size_of::<Value>()
        .checked_add(match value {
            Value::Varchar(value) => value.len(),
            Value::Blob(value) => value.len(),
            _ => 0,
        })
        .ok_or_else(|| Error::Resource("materialized value size overflow".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn try_materialized_string(value: &str) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|_| Error::Resource("cannot allocate materialized string".into()))?;
    output.push_str(value);
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn try_clone_materialized_value(value: &Value) -> Result<Value> {
    Ok(match value {
        Value::Varchar(value) => Value::Varchar(try_materialized_string(value)?),
        Value::Blob(value) => {
            let mut output = Vec::new();
            output
                .try_reserve_exact(value.len())
                .map_err(|_| Error::Resource("cannot allocate materialized blob".into()))?;
            output.extend_from_slice(value);
            Value::Blob(output)
        }
        value => value.clone(),
    })
}

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
    /// Nullable signed values retain their declared physical width. A set
    /// validity bit identifies a lane with a logical value; NULL lanes contain
    /// an unspecified zero placeholder and must never be read without first
    /// consulting the bitmap.
    FlatNullableSigned(SignedLanes, Arc<Vec<u64>>),
    FlatDecimalI64(Arc<Vec<i64>>),
    /// VARCHAR payloads share one immutable UTF-8 arena. Row-order ranges may
    /// overlap or interleave other columns; `None` alone represents SQL NULL.
    FlatUtf8(Arc<String>, Arc<Vec<Option<Range<usize>>>>),
    /// Plain STRUCT rows retain their fields as independently encoded columns.
    /// The declared field metadata remains authoritative in `Vector.data_type`.
    FlatStruct {
        validity: Option<Arc<Vec<u64>>>,
        children: Arc<Vec<Vector>>,
    },
    /// Plain LIST rows retain checked row offsets into one child column. A
    /// clear validity bit and an equal adjacent offset represent NULL; equal
    /// offsets with a set bit represent an empty list.
    FlatList {
        validity: Option<Arc<Vec<u64>>>,
        offsets: Arc<Vec<usize>>,
        child: Arc<Vector>,
    },
    Constant(Value),
    Dictionary(Arc<Vector>, Arc<Vec<usize>>),
    /// Immutable table storage retains CTAS batches without first copying all
    /// payloads into a second table-wide flat allocation.  A scan-sized slice
    /// that lies within one segment becomes that segment's ordinary vector,
    /// so scalar and aggregate kernels retain their existing flat fast paths.
    Chunks(Arc<Vec<Vector>>, Arc<Vec<usize>>),
}

/// Borrowed physical row for recursive built-in consumers. Selected and
/// chunked vectors resolve to their owning row before this view is returned.
pub(crate) enum NestedRowRef<'a> {
    Null,
    Struct {
        children: &'a [Vector],
        index: usize,
    },
    List {
        child: &'a Vector,
        range: Range<usize>,
    },
    Scalar(&'a NestedPayload),
}

/// Private proof that newly allocated vector metadata was admitted before its
/// allocation. Callers cannot substitute an unrelated reservation token.
pub(crate) struct MetadataAdmission {
    reservations: Vec<crate::parallel::Reservation>,
    admitted: usize,
}

impl MetadataAdmission {
    pub(crate) fn new() -> Self {
        Self {
            reservations: Vec::new(),
            admitted: 0,
        }
    }

    pub(crate) fn try_reserve_vec<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
        query: &crate::parallel::QueryContext,
        message: &'static str,
    ) -> Result<()> {
        let needed = values
            .len()
            .checked_add(additional)
            .ok_or_else(|| Error::Resource(message.into()))?;
        if needed <= values.capacity() {
            return Ok(());
        }
        let old = values.capacity();
        // Repeated append producers must not allocate and retain one new guard
        // per element. Keep the first known-size allocation exact, then grow
        // geometrically while charging the complete chosen spare capacity
        // before asking the allocator for it.
        let target = if old == 0 {
            needed
        } else {
            old.checked_mul(2).unwrap_or(needed).max(needed)
        };
        let requested = target
            .checked_sub(old)
            .and_then(|slots| slots.checked_mul(std::mem::size_of::<T>()))
            .ok_or_else(|| Error::Resource(message.into()))?;
        let initial = query.memory_pool().reserve(requested, query)?;
        values
            .try_reserve_exact(target - values.len())
            .map_err(|_| Error::Resource(message.into()))?;
        let actual = values
            .capacity()
            .checked_sub(old)
            .and_then(|slots| slots.checked_mul(std::mem::size_of::<T>()))
            .ok_or_else(|| Error::Resource(message.into()))?;
        self.reservations.push(initial);
        self.admitted = self
            .admitted
            .checked_add(requested)
            .ok_or_else(|| Error::Resource(message.into()))?;
        if actual > requested {
            let rounding = query.memory_pool().reserve(actual - requested, query)?;
            self.reservations.push(rounding);
            self.admitted = self
                .admitted
                .checked_add(actual - requested)
                .ok_or_else(|| Error::Resource(message.into()))?;
        }
        Ok(())
    }

    pub(crate) fn try_reserve_string(
        &mut self,
        value: &mut String,
        additional: usize,
        query: &crate::parallel::QueryContext,
        message: &'static str,
    ) -> Result<()> {
        let needed = value
            .len()
            .checked_add(additional)
            .ok_or_else(|| Error::Resource(message.into()))?;
        if needed <= value.capacity() {
            return Ok(());
        }
        let old = value.capacity();
        let target = if old == 0 {
            needed
        } else {
            old.checked_mul(2).unwrap_or(needed).max(needed)
        };
        let requested = target - old;
        let initial = query.memory_pool().reserve(requested, query)?;
        value
            .try_reserve_exact(target - value.len())
            .map_err(|_| Error::Resource(message.into()))?;
        let actual = value.capacity() - old;
        self.reservations.push(initial);
        self.admitted = self
            .admitted
            .checked_add(requested)
            .ok_or_else(|| Error::Resource(message.into()))?;
        if actual > requested {
            let rounding = query.memory_pool().reserve(actual - requested, query)?;
            self.reservations.push(rounding);
            self.admitted = self
                .admitted
                .checked_add(actual - requested)
                .ok_or_else(|| Error::Resource(message.into()))?;
        }
        Ok(())
    }

    pub(crate) fn finish(
        mut self,
        actual: usize,
        query: &crate::parallel::QueryContext,
    ) -> Result<Option<crate::parallel::Reservation>> {
        if actual > self.admitted {
            self.reservations
                .push(query.memory_pool().reserve(actual - self.admitted, query)?);
            self.admitted = actual;
        }
        if self.admitted != actual {
            return Err(Error::Internal(
                "vector metadata admission differs from retained capacity".into(),
            ));
        }
        Ok((!self.reservations.is_empty())
            .then(|| crate::parallel::Reservation::merge(self.reservations)))
    }
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

/// A borrowed nullable BIGINT view. The values already describe the vector's
/// logical range, while `validity_offset` retains the position of that range
/// in its shared bitmap. Keeping this detail in the view makes sliced access
/// allocation-free without exposing bitmap arithmetic to consumers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NullableBigIntView<'a> {
    values: &'a [i64],
    validity: &'a [u64],
    validity_offset: usize,
}

/// A borrowed logical slice of validated packed VARCHAR storage. The arena is
/// UTF-8 and every retained range has checked character boundaries, so access
/// can return `&str` without allocation or unsafe conversion.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FlatUtf8<'a> {
    arena: &'a str,
    ranges: &'a [Option<Range<usize>>],
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> FlatUtf8<'a> {
    pub(crate) fn len(self) -> usize {
        self.ranges.len()
    }

    /// `None` is out of bounds; the boolean is false only for SQL NULL.
    pub(crate) fn is_valid(self, index: usize) -> Option<bool> {
        self.ranges.get(index).map(Option::is_some)
    }

    pub(crate) fn iter(self) -> impl ExactSizeIterator<Item = Option<&'a str>> + 'a {
        let arena = self.arena;
        self.ranges
            .iter()
            .map(move |range| range.as_ref().map(|range| &arena[range.clone()]))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NullableBigIntView<'_> {
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn is_valid(&self, index: usize) -> bool {
        index < self.len() && validity_is_set(self.validity, self.validity_offset + index)
    }

    /// Outer `None` is out of bounds; inner `None` is SQL NULL.
    pub(crate) fn value(&self, index: usize) -> Option<Option<i64>> {
        self.values
            .get(index)
            .map(|&value| self.is_valid(index).then_some(value))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline(always)]
fn validity_is_set(validity: &[u64], index: usize) -> bool {
    validity
        .get(index / u64::BITS as usize)
        .is_some_and(|word| word & (1_u64 << (index % u64::BITS as usize)) != 0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline(always)]
fn set_validity_bit(validity: &mut Vec<u64>, index: usize) {
    let word = index / u64::BITS as usize;
    if word == validity.len() {
        validity.push(0);
    }
    validity[word] |= 1_u64 << (index % u64::BITS as usize);
}

fn validate_parent_validity(validity: Option<&[u64]>, count: usize) -> Result<()> {
    let Some(validity) = validity else {
        return Ok(());
    };
    let words = count.div_ceil(u64::BITS as usize);
    if validity.len() != words {
        return Err(Error::Internal(
            "columnar nested validity length differs from cardinality".into(),
        ));
    }
    if let Some(&last) = validity.last() {
        let used = count % u64::BITS as usize;
        if used != 0 && last >> used != 0 {
            return Err(Error::Internal(
                "columnar nested validity has set trailing bits".into(),
            ));
        }
    }
    Ok(())
}

fn parent_all_valid(validity: Option<&[u64]>, count: usize) -> bool {
    validity.is_none_or(|validity| (0..count).all(|index| validity_is_set(validity, index)))
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

    fn from_nullable_values(data_type: &DataType, values: &[Value]) -> (Self, Arc<Vec<u64>>) {
        let mut validity = Vec::with_capacity(values.len().div_ceil(u64::BITS as usize));
        for (index, value) in values.iter().enumerate() {
            if !value.is_null() {
                set_validity_bit(&mut validity, index);
            } else if index / u64::BITS as usize == validity.len() {
                validity.push(0);
            }
        }
        let lanes = match data_type {
            DataType::TinyInt => Self::Tiny(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value as i8,
                        Value::Null => 0,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            DataType::SmallInt => Self::Small(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value as i16,
                        Value::Null => 0,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            DataType::Integer => Self::Integer(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value as i32,
                        Value::Null => 0,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            DataType::BigInt => Self::Big(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value as i64,
                        Value::Null => 0,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            DataType::HugeInt => Self::Huge(Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Integer(value) => *value,
                        Value::Null => 0,
                        _ => unreachable!("validated signed column"),
                    })
                    .collect(),
            )),
            _ => unreachable!("signed lanes require a signed integer type"),
        };
        (lanes, Arc::new(validity))
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

    fn append_nullable_values(
        &self,
        range: std::ops::Range<usize>,
        validity: &[u64],
        output: &mut Vec<Value>,
    ) {
        output.extend(range.map(|index| {
            if validity_is_set(validity, index) {
                Value::Integer(self.value(index).expect("validated signed lane index"))
            } else {
                Value::Null
            }
        }));
    }

    fn append_nullable_indices(
        &self,
        indices: &[usize],
        offset: usize,
        validity: &[u64],
        output: &mut Vec<Value>,
    ) {
        output.extend(indices.iter().map(|&selected| {
            let index = offset + selected;
            if validity_is_set(validity, index) {
                Value::Integer(self.value(index).expect("validated signed lane index"))
            } else {
                Value::Null
            }
        }));
    }

    fn same_backing(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Tiny(left), Self::Tiny(right)) => Arc::ptr_eq(left, right),
            (Self::Small(left), Self::Small(right)) => Arc::ptr_eq(left, right),
            (Self::Integer(left), Self::Integer(right)) => Arc::ptr_eq(left, right),
            (Self::Big(left), Self::Big(right)) => Arc::ptr_eq(left, right),
            (Self::Huge(left), Self::Huge(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    fn retained_bytes(&self, seen: &mut HashSet<usize>) -> Result<usize> {
        macro_rules! lane_bytes {
            ($values:expr, $ty:ty) => {{
                let key = Arc::as_ptr($values) as usize;
                if seen.insert(key) {
                    $values
                        .capacity()
                        .checked_mul(std::mem::size_of::<$ty>())
                        .ok_or_else(|| Error::Resource("vector backing size overflow".into()))
                } else {
                    Ok(0)
                }
            }};
        }
        match self {
            Self::Tiny(values) => lane_bytes!(values, i8),
            Self::Small(values) => lane_bytes!(values, i16),
            Self::Integer(values) => lane_bytes!(values, i32),
            Self::Big(values) => lane_bytes!(values, i64),
            Self::Huge(values) => lane_bytes!(values, i128),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn same_flat_backing(left: &Encoding, right: &Encoding) -> bool {
    match (left, right) {
        (Encoding::FlatValues(left), Encoding::FlatValues(right)) => Arc::ptr_eq(left, right),
        (Encoding::FlatDouble(left), Encoding::FlatDouble(right)) => Arc::ptr_eq(left, right),
        (Encoding::FlatSigned(left), Encoding::FlatSigned(right)) => left.same_backing(right),
        (
            Encoding::FlatNullableSigned(left_lanes, left_validity),
            Encoding::FlatNullableSigned(right_lanes, right_validity),
        ) => left_lanes.same_backing(right_lanes) && Arc::ptr_eq(left_validity, right_validity),
        (Encoding::FlatDecimalI64(left), Encoding::FlatDecimalI64(right)) => {
            Arc::ptr_eq(left, right)
        }
        (
            Encoding::FlatUtf8(left_arena, left_ranges),
            Encoding::FlatUtf8(right_arena, right_ranges),
        ) => Arc::ptr_eq(left_arena, right_arena) && Arc::ptr_eq(left_ranges, right_ranges),
        (
            Encoding::FlatStruct {
                validity: left_validity,
                children: left_children,
            },
            Encoding::FlatStruct {
                validity: right_validity,
                children: right_children,
            },
        ) => {
            option_arc_ptr_eq(left_validity, right_validity)
                && Arc::ptr_eq(left_children, right_children)
        }
        (
            Encoding::FlatList {
                validity: left_validity,
                offsets: left_offsets,
                child: left_child,
            },
            Encoding::FlatList {
                validity: right_validity,
                offsets: right_offsets,
                child: right_child,
            },
        ) => {
            option_arc_ptr_eq(left_validity, right_validity)
                && Arc::ptr_eq(left_offsets, right_offsets)
                && Arc::ptr_eq(left_child, right_child)
        }
        _ => false,
    }
}

fn option_arc_ptr_eq<T>(left: &Option<Arc<T>>, right: &Option<Arc<T>>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
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
    reservation: Option<crate::parallel::Reservation>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Vector {
    fn retain_reservation(&mut self, reservation: crate::parallel::Reservation) {
        self.reservation = Some(match self.reservation.take() {
            Some(existing) => crate::parallel::Reservation::merge(vec![existing, reservation]),
            None => reservation,
        });
    }
    /// Combine same-typed immutable input batches without copying their
    /// payloads.  This is storage-facing: execution still observes ordinary
    /// vectors after a scan slices a segment-sized batch.
    pub(crate) fn chunked(data_type: DataType, chunks: Vec<Self>) -> Result<Self> {
        let mut offsets = Vec::with_capacity(chunks.len().saturating_add(1));
        offsets.push(0);
        let mut count = 0usize;
        for chunk in &chunks {
            count = count
                .checked_add(chunk.len())
                .ok_or_else(|| Error::Resource("chunked vector size overflow".into()))?;
            offsets.push(count);
        }
        Self::chunked_with_offsets(data_type, chunks, offsets)
    }
    pub(crate) fn chunked_with_metadata_admission(
        data_type: DataType,
        chunks: Vec<Self>,
        offsets: Vec<usize>,
        admission: MetadataAdmission,
        query: &crate::parallel::QueryContext,
    ) -> Result<Self> {
        let actual = chunks
            .capacity()
            .checked_mul(std::mem::size_of::<Self>())
            .and_then(|bytes| {
                offsets
                    .capacity()
                    .checked_mul(std::mem::size_of::<usize>())
                    .and_then(|offsets| bytes.checked_add(offsets))
            })
            .ok_or_else(|| Error::Resource("chunked vector metadata overflow".into()))?;
        let reservation = admission.finish(actual, query)?;
        let mut result = Self::chunked_with_offsets(data_type, chunks, offsets)?;
        if let Some(reservation) = reservation {
            result.retain_reservation(reservation);
        }
        Ok(result)
    }

    fn chunked_with_offsets(
        data_type: DataType,
        chunks: Vec<Self>,
        offsets: Vec<usize>,
    ) -> Result<Self> {
        if offsets.len() != chunks.len().saturating_add(1) || offsets.first() != Some(&0) {
            return Err(Error::Internal("chunked vector offsets differ".into()));
        }
        let mut all_valid = true;
        let mut numeric_ascending = data_type.is_decimal() || data_type.is_unsigned_integer();
        let mut previous = None;
        let mut count = 0usize;
        for (index, chunk) in chunks.iter().enumerate() {
            if chunk.data_type != data_type || offsets[index] != count {
                return Err(Error::Internal(
                    "chunked vector type or offsets differ".into(),
                ));
            }
            count = count
                .checked_add(chunk.len())
                .ok_or_else(|| Error::Resource("chunked vector size overflow".into()))?;
            if offsets[index + 1] != count {
                return Err(Error::Internal("chunked vector offsets differ".into()));
            }
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
            reservation: None,
            data_type,
            encoding: Encoding::Chunks(Arc::new(chunks), Arc::new(offsets)),
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
                    | Encoding::FlatNullableSigned(_, _)
                    | Encoding::FlatDecimalI64(_)
                    | Encoding::FlatUtf8(_, _)
            )
        {
            let mut end = first.offset;
            if columns.iter().all(|column| {
                let contiguous =
                    column.offset == end && same_flat_backing(&first.encoding, &column.encoding);
                end = column.offset + column.count;
                contiguous
            }) {
                let reservations = columns
                    .iter()
                    .filter_map(|column| column.reservation.clone())
                    .collect::<Vec<_>>();
                return Ok(Self {
                    reservation: (!reservations.is_empty())
                        .then(|| crate::parallel::Reservation::merge(reservations)),
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
        let mut lanes = Vec::new();
        lanes
            .try_reserve(values.size_hint().0)
            .map_err(|_| Error::Resource("cannot allocate BIGINT column".into()))?;
        let mut validity = Vec::new();
        validity
            .try_reserve(values.size_hint().0.div_ceil(u64::BITS as usize))
            .map_err(|_| Error::Resource("cannot allocate BIGINT validity".into()))?;
        let mut all_valid = true;
        let mut numeric_ascending = true;
        let mut previous = None;
        for value in values {
            let index = lanes.len();
            lanes.push(match value? {
                Some(value) => {
                    set_validity_bit(&mut validity, index);
                    numeric_ascending &= previous.is_none_or(|previous| previous <= value);
                    previous = Some(value);
                    value
                }
                None => {
                    if index / u64::BITS as usize == validity.len() {
                        validity.push(0);
                    }
                    all_valid = false;
                    numeric_ascending = false;
                    0
                }
            });
        }
        let count = lanes.len();
        let lanes = SignedLanes::Big(Arc::new(lanes));
        let encoding = if all_valid {
            Encoding::FlatSigned(lanes)
        } else {
            Encoding::FlatNullableSigned(lanes, Arc::new(validity))
        };
        Ok(Self {
            reservation: None,
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
            reservation: None,
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
        let mut lanes = Vec::new();
        lanes
            .try_reserve(values.size_hint().0)
            .map_err(|_| Error::Resource("cannot allocate HUGEINT column".into()))?;
        let mut validity = Vec::new();
        validity
            .try_reserve(values.size_hint().0.div_ceil(u64::BITS as usize))
            .map_err(|_| Error::Resource("cannot allocate HUGEINT validity".into()))?;
        let mut all_valid = true;
        for value in values {
            let index = lanes.len();
            lanes.push(match value? {
                Some(value) => {
                    set_validity_bit(&mut validity, index);
                    value
                }
                None => {
                    if index / u64::BITS as usize == validity.len() {
                        validity.push(0);
                    }
                    all_valid = false;
                    0
                }
            });
        }
        let count = lanes.len();
        let lanes = SignedLanes::Huge(Arc::new(lanes));
        let encoding = if all_valid {
            Encoding::FlatSigned(lanes)
        } else {
            Encoding::FlatNullableSigned(lanes, Arc::new(validity))
        };
        Ok(Self {
            reservation: None,
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
            reservation: None,
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
            reservation: None,
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
            reservation: None,
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
            reservation: None,
            data_type: DataType::Double,
            count: values.len(),
            encoding: Encoding::FlatDouble(Arc::new(values)),
            offset: 0,
            all_valid: true,
            numeric_ascending: false,
        })
    }
    /// Construct plain VARCHAR storage whose payloads borrow one immutable
    /// UTF-8 arena. Ranges are in logical row order but need not be monotonic,
    /// contiguous or disjoint: a row-major producer can share one arena among
    /// several columns without copying their fields into column arenas.
    pub(crate) fn packed_utf8(
        arena: Arc<String>,
        ranges: Vec<Option<Range<usize>>>,
    ) -> Result<Self> {
        for range in ranges.iter().flatten() {
            if range.start > range.end
                || range.end > arena.len()
                || !arena.is_char_boundary(range.start)
                || !arena.is_char_boundary(range.end)
            {
                return Err(Error::Internal(
                    "packed VARCHAR range is invalid or splits UTF-8".into(),
                ));
            }
        }
        let all_valid = ranges.iter().all(Option::is_some);
        let count = ranges.len();
        Ok(Self {
            reservation: None,
            data_type: DataType::Varchar,
            encoding: Encoding::FlatUtf8(arena, Arc::new(ranges)),
            offset: 0,
            count,
            all_valid,
            numeric_ascending: false,
        })
    }
    /// Construct several packed VARCHAR columns sharing one arena and one
    /// independently admitted backing charge. The shared reservation is a
    /// lifetime guard, not repeated credit for each child.
    pub(crate) fn packed_utf8_columns(
        arena: String,
        columns: Vec<Vec<Option<Range<usize>>>>,
        admission: MetadataAdmission,
        mut column_admission: MetadataAdmission,
        query: &crate::parallel::QueryContext,
    ) -> Result<(Vec<Self>, MetadataAdmission)> {
        let bytes = columns.iter().try_fold(arena.capacity(), |bytes, ranges| {
            ranges
                .capacity()
                .checked_mul(std::mem::size_of::<Option<Range<usize>>>())
                .and_then(|ranges| bytes.checked_add(ranges))
                .ok_or_else(|| Error::Resource("packed VARCHAR backing overflow".into()))
        })?;
        let reservation = admission.finish(bytes, query)?;
        let arena = Arc::new(arena);
        let mut output = Vec::new();
        column_admission.try_reserve_vec(
            &mut output,
            columns.len(),
            query,
            "packed VARCHAR column metadata allocation failed",
        )?;
        for ranges in columns {
            let mut vector = Self::packed_utf8(arena.clone(), ranges)?;
            if let Some(reservation) = reservation.clone() {
                vector.retain_reservation(reservation);
            }
            output.push(vector);
        }
        Ok((output, column_admission))
    }
    /// Construct a physically columnar plain STRUCT. The constructor validates
    /// its complete immutable shape and independently admits the metadata it
    /// retains; child reservation guards are kept only for their own lifetime.
    pub(crate) fn flat_struct_checked(
        data_type: DataType,
        count: usize,
        validity: Option<Vec<u64>>,
        children: Vec<Self>,
        admission: MetadataAdmission,
        query: &crate::parallel::QueryContext,
    ) -> Result<Self> {
        let DataType::Nested(metadata) = &data_type else {
            return Err(Error::Internal(
                "columnar STRUCT requires nested type".into(),
            ));
        };
        let NestedType::Struct(fields) = metadata.as_ref() else {
            return Err(Error::Internal(
                "columnar STRUCT requires STRUCT type".into(),
            ));
        };
        super::type_registry::check_metadata(&data_type)?;
        if fields.len() != children.len()
            || fields
                .iter()
                .zip(&children)
                .any(|((_, expected), child)| expected != child.data_type() || child.len() != count)
        {
            return Err(Error::Internal(
                "columnar STRUCT children differ from declared shape".into(),
            ));
        }
        validate_parent_validity(validity.as_deref(), count)?;
        let metadata_bytes = children
            .capacity()
            .checked_mul(std::mem::size_of::<Self>())
            .and_then(|bytes| {
                validity
                    .as_ref()
                    .and_then(|validity| {
                        validity
                            .capacity()
                            .checked_mul(std::mem::size_of::<u64>())
                            .and_then(|validity| bytes.checked_add(validity))
                    })
                    .or_else(|| validity.is_none().then_some(bytes))
            })
            .ok_or_else(|| Error::Resource("columnar STRUCT metadata overflow".into()))?;
        let metadata_reservation = admission.finish(metadata_bytes, query)?;
        let all_valid = parent_all_valid(validity.as_deref(), count);
        let result = Self {
            reservation: metadata_reservation,
            data_type,
            encoding: Encoding::FlatStruct {
                validity: validity.map(Arc::new),
                children: Arc::new(children),
            },
            offset: 0,
            count,
            all_valid,
            numeric_ascending: false,
        };
        result.retained_backing_bytes()?;
        Ok(result)
    }

    /// Construct a physically columnar plain LIST after validating every
    /// offset and independently admitting the retained metadata capacities.
    pub(crate) fn flat_list_checked(
        data_type: DataType,
        count: usize,
        validity: Option<Vec<u64>>,
        offsets: Vec<usize>,
        child: Self,
        admission: MetadataAdmission,
        query: &crate::parallel::QueryContext,
    ) -> Result<Self> {
        let DataType::Nested(metadata) = &data_type else {
            return Err(Error::Internal("columnar LIST requires nested type".into()));
        };
        let NestedType::List(expected_child) = metadata.as_ref() else {
            return Err(Error::Internal("columnar LIST requires LIST type".into()));
        };
        super::type_registry::check_metadata(&data_type)?;
        if child.data_type() != expected_child
            || offsets.len()
                != count
                    .checked_add(1)
                    .ok_or_else(|| Error::Resource("columnar LIST cardinality overflow".into()))?
            || offsets.first() != Some(&0)
            || offsets.last() != Some(&child.len())
            || offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(Error::Internal(
                "columnar LIST child or offsets differ from declared shape".into(),
            ));
        }
        validate_parent_validity(validity.as_deref(), count)?;
        if let Some(validity) = validity.as_deref() {
            for index in 0..count {
                if !validity_is_set(validity, index) && offsets[index] != offsets[index + 1] {
                    return Err(Error::Internal(
                        "columnar LIST NULL row retains child values".into(),
                    ));
                }
            }
        }
        let metadata_bytes = offsets
            .capacity()
            .checked_mul(std::mem::size_of::<usize>())
            .and_then(|bytes| {
                validity
                    .as_ref()
                    .and_then(|validity| {
                        validity
                            .capacity()
                            .checked_mul(std::mem::size_of::<u64>())
                            .and_then(|validity| bytes.checked_add(validity))
                    })
                    .or_else(|| validity.is_none().then_some(bytes))
            })
            .ok_or_else(|| Error::Resource("columnar LIST metadata overflow".into()))?;
        let metadata_reservation = admission.finish(metadata_bytes, query)?;
        let all_valid = parent_all_valid(validity.as_deref(), count);
        let result = Self {
            reservation: metadata_reservation,
            data_type,
            encoding: Encoding::FlatList {
                validity: validity.map(Arc::new),
                offsets: Arc::new(offsets),
                child: Arc::new(child),
            },
            offset: 0,
            count,
            all_valid,
            numeric_ascending: false,
        };
        result.retained_backing_bytes()?;
        Ok(result)
    }

    /// Select rows while independently charging the retained selection
    /// capacity. Parent guards are preserved but never credited to this new
    /// metadata allocation.
    pub(crate) fn select_with_metadata_admission(
        self: &Arc<Self>,
        selection: Vec<usize>,
        admission: MetadataAdmission,
        query: &crate::parallel::QueryContext,
    ) -> Result<Self> {
        let bytes = selection
            .capacity()
            .checked_mul(std::mem::size_of::<usize>())
            .ok_or_else(|| Error::Resource("vector selection metadata overflow".into()))?;
        let reservation = admission.finish(bytes, query)?;
        let mut result = self.select(selection)?;
        if let Some(reservation) = reservation {
            result.retain_reservation(reservation);
        }
        Ok(result)
    }
    pub fn flat(data_type: DataType, values: Vec<Value>) -> Result<Self> {
        let mut all_valid = true;
        let mut numeric_ascending = data_type.is_decimal() || data_type.is_unsigned_integer();
        let mut previous = None;
        let mut nested_metadata_validated = false;
        for value in &values {
            let fits = if matches!(value, Value::Nested(_))
                && matches!(&data_type, DataType::Nested(_))
            {
                if !nested_metadata_validated {
                    if super::type_registry::check_metadata(&data_type).is_err() {
                        return Err(Error::Internal(
                            "vector values require explicit conversion to the declared type".into(),
                        ));
                    }
                    nested_metadata_validated = true;
                }
                value.fits_type_with_validated_metadata(&data_type)
            } else {
                value.fits_type(&data_type)
            };
            if !fits {
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
        // authoritative physical lane. Nullable signed columns retain their
        // declared-width lane beside a compact validity bitmap.
        let count = values.len();
        let encoding = if all_valid && data_type.is_signed_integer() {
            Encoding::FlatSigned(SignedLanes::from_values(&data_type, &values))
        } else if data_type.is_signed_integer() {
            let (lanes, validity) = SignedLanes::from_nullable_values(&data_type, &values);
            Encoding::FlatNullableSigned(lanes, validity)
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
            reservation: None,
            data_type,
            offset: 0,
            count,
            encoding,
            all_valid,
            numeric_ascending,
        })
    }
    pub fn constant(data_type: DataType, value: Value, count: usize) -> Result<Self> {
        let fits =
            if matches!(&value, Value::Nested(_)) && matches!(&data_type, DataType::Nested(_)) {
                super::type_registry::check_metadata(&data_type).is_ok()
                    && value.fits_type_with_validated_metadata(&data_type)
            } else {
                value.fits_type(&data_type)
            };
        if !fits {
            return Err(Error::Internal(
                "constant vector requires explicit conversion to the declared type".into(),
            ));
        }
        Ok(Self {
            reservation: None,
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
        Ok(self.selected(Arc::new(selection), ordered))
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
            reservation: self.reservation.clone(),
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
    fn selected(self: &Arc<Self>, selection: Arc<Vec<usize>>, ordered: bool) -> Self {
        if matches!(self.encoding, Encoding::Constant(_)) {
            return Self {
                offset: 0,
                count: selection.len(),
                ..self.as_ref().clone()
            };
        }
        Self {
            reservation: self.reservation.clone(),
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
                let mut result = chunk.slice(start - offsets[segment], count)?;
                if let Some(token) = &self.reservation {
                    result.retain_reservation(token.clone());
                }
                return Ok(result);
            }
        }
        Ok(Self {
            reservation: self.reservation.clone(),
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
    /// Resolve SQL NULL without constructing an owned logical value. This is
    /// the representation-transparent seam used by postcondition checks after
    /// an adapter has already produced or consumed a vector.
    pub(crate) fn is_null_at(&self, index: usize) -> Option<bool> {
        if index >= self.count {
            return None;
        }
        if self.all_valid {
            return Some(false);
        }
        let physical = self.offset + index;
        Some(match &self.encoding {
            Encoding::FlatValues(values) => values.get(physical)?.is_null(),
            Encoding::FlatDouble(_) | Encoding::FlatSigned(_) | Encoding::FlatDecimalI64(_) => {
                false
            }
            Encoding::FlatNullableSigned(_, validity) => !validity_is_set(validity, physical),
            Encoding::FlatUtf8(_, ranges) => ranges.get(physical)?.is_none(),
            Encoding::FlatStruct { validity, .. } | Encoding::FlatList { validity, .. } => validity
                .as_deref()
                .is_some_and(|validity| !validity_is_set(validity, physical)),
            Encoding::Constant(value) => value.is_null(),
            Encoding::Dictionary(parent, selection) => {
                return parent.is_null_at(*selection.get(physical)?);
            }
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= physical)
                    .saturating_sub(1);
                return chunks.get(segment)?.is_null_at(physical - offsets[segment]);
            }
        })
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
    pub(crate) fn nested_row_at(&self, index: usize) -> Option<NestedRowRef<'_>> {
        if index >= self.count || !matches!(self.data_type, DataType::Nested(_)) {
            return None;
        }
        let physical = self.offset + index;
        match &self.encoding {
            Encoding::FlatStruct { validity, children } => {
                if validity
                    .as_deref()
                    .is_some_and(|validity| !validity_is_set(validity, physical))
                {
                    Some(NestedRowRef::Null)
                } else {
                    Some(NestedRowRef::Struct {
                        children,
                        index: physical,
                    })
                }
            }
            Encoding::FlatList {
                validity,
                offsets,
                child,
            } => {
                if validity
                    .as_deref()
                    .is_some_and(|validity| !validity_is_set(validity, physical))
                {
                    Some(NestedRowRef::Null)
                } else {
                    Some(NestedRowRef::List {
                        child,
                        range: offsets[physical]..offsets[physical + 1],
                    })
                }
            }
            Encoding::FlatValues(values) => match values.get(physical)? {
                Value::Null => Some(NestedRowRef::Null),
                Value::Nested(value) => Some(NestedRowRef::Scalar(&value.payload)),
                _ => None,
            },
            Encoding::Constant(Value::Null) => Some(NestedRowRef::Null),
            Encoding::Constant(Value::Nested(value)) => Some(NestedRowRef::Scalar(&value.payload)),
            Encoding::Dictionary(parent, selection) => {
                parent.nested_row_at(*selection.get(physical)?)
            }
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= physical)
                    .saturating_sub(1);
                chunks
                    .get(segment)?
                    .nested_row_at(physical - offsets[segment])
            }
            _ => None,
        }
    }

    /// Whether every row in this immutable encoding can be borrowed through
    /// `nested_row_at`. Complete vector validation still owns logical payload
    /// checks; this only avoids probing each row once before an ordered batch
    /// comparison probes it again.
    pub(crate) fn has_nested_row_access(&self) -> bool {
        let DataType::Nested(metadata) = &self.data_type else {
            return false;
        };
        if !matches!(
            metadata.as_ref(),
            NestedType::Struct(_) | NestedType::List(_)
        ) {
            return false;
        }
        match &self.encoding {
            Encoding::FlatStruct { .. } => matches!(metadata.as_ref(), NestedType::Struct(_)),
            Encoding::FlatList { .. } => matches!(metadata.as_ref(), NestedType::List(_)),
            Encoding::FlatValues(values) => values
                .iter()
                .all(|value| matches!(value, Value::Null | Value::Nested(_))),
            Encoding::Constant(value) => matches!(value, Value::Null | Value::Nested(_)),
            Encoding::Dictionary(parent, _) => parent.has_nested_row_access(),
            Encoding::Chunks(chunks, _) => chunks.iter().all(Vector::has_nested_row_access),
            _ => false,
        }
    }

    pub fn value(&self, index: usize) -> Option<Value> {
        if index >= self.count {
            return None;
        }
        let index = self.offset + index;
        match &self.encoding {
            Encoding::FlatValues(v) => v.get(index).cloned(),
            Encoding::FlatDouble(v) => v.get(index).copied().map(Value::Double),
            Encoding::FlatSigned(v) => v.value(index).map(Value::Integer),
            Encoding::FlatNullableSigned(values, validity) => {
                if validity_is_set(validity, index) {
                    values.value(index).map(Value::Integer)
                } else {
                    Some(Value::Null)
                }
            }
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
            Encoding::FlatUtf8(arena, ranges) => ranges.get(index).map(|range| {
                range.as_ref().map_or(Value::Null, |range| {
                    Value::Varchar(arena[range.clone()].to_owned())
                })
            }),
            Encoding::FlatStruct { validity, children } => {
                if validity
                    .as_deref()
                    .is_some_and(|validity| !validity_is_set(validity, index))
                {
                    Some(Value::Null)
                } else {
                    let values = children
                        .iter()
                        .map(|child| child.value(index))
                        .collect::<Option<Vec<_>>>()?;
                    Some(Value::Nested(Arc::new(NestedValue {
                        data_type: self.data_type.clone(),
                        payload: NestedPayload::Struct(values),
                    })))
                }
            }
            Encoding::FlatList {
                validity,
                offsets,
                child,
            } => {
                if validity
                    .as_deref()
                    .is_some_and(|validity| !validity_is_set(validity, index))
                {
                    Some(Value::Null)
                } else {
                    let values = (offsets[index]..offsets[index + 1])
                        .map(|child_index| child.value(child_index))
                        .collect::<Option<Vec<_>>>()?;
                    Some(Value::Nested(Arc::new(NestedValue {
                        data_type: self.data_type.clone(),
                        payload: NestedPayload::Sequence(values),
                    })))
                }
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
    /// Bytes independently owned by materializing this logical column. Arc
    /// payloads remain shared; VARCHAR and BLOB allocate independent bytes.
    pub(crate) fn materialized_bytes(&self) -> Result<usize> {
        (0..self.len()).try_fold(0usize, |bytes, index| {
            bytes
                .checked_add(self.materialized_value_bytes(index)?)
                .ok_or_else(|| Error::Resource("materialized column size overflow".into()))
        })
    }

    /// Bytes retained by the immutable physical backing, without constructing
    /// scalar Values. Shared arenas, children and view parents are counted once.
    pub(crate) fn retained_backing_bytes(&self) -> Result<usize> {
        self.retained_backing_bytes_inner(&mut HashSet::new())
    }

    fn retained_backing_bytes_inner(&self, seen: &mut HashSet<usize>) -> Result<usize> {
        let add = |left: usize, right: usize| {
            left.checked_add(right)
                .ok_or_else(|| Error::Resource("vector backing size overflow".into()))
        };
        let arc_slice = |key: usize, bytes: usize, seen: &mut HashSet<usize>| {
            if seen.insert(key) { Ok(bytes) } else { Ok(0) }
        };
        match &self.encoding {
            Encoding::FlatValues(values) => arc_slice(
                Arc::as_ptr(values) as usize,
                values
                    .capacity()
                    .checked_mul(std::mem::size_of::<Value>())
                    .ok_or_else(|| Error::Resource("vector backing size overflow".into()))?,
                seen,
            )
            .and_then(|mut bytes| {
                if bytes != 0 {
                    for value in values.iter() {
                        bytes = add(bytes, retained_value_backing_bytes(value, seen)?)?;
                    }
                }
                Ok(bytes)
            }),
            Encoding::FlatDouble(values) => arc_slice(
                Arc::as_ptr(values) as usize,
                values
                    .capacity()
                    .checked_mul(std::mem::size_of::<f64>())
                    .ok_or_else(|| Error::Resource("vector backing size overflow".into()))?,
                seen,
            ),
            Encoding::FlatSigned(values) => values.retained_bytes(seen),
            Encoding::FlatNullableSigned(values, validity) => add(
                values.retained_bytes(seen)?,
                arc_slice(
                    Arc::as_ptr(validity) as usize,
                    validity
                        .capacity()
                        .checked_mul(std::mem::size_of::<u64>())
                        .ok_or_else(|| Error::Resource("vector backing size overflow".into()))?,
                    seen,
                )?,
            ),
            Encoding::FlatDecimalI64(values) => arc_slice(
                Arc::as_ptr(values) as usize,
                values
                    .capacity()
                    .checked_mul(std::mem::size_of::<i64>())
                    .ok_or_else(|| Error::Resource("vector backing size overflow".into()))?,
                seen,
            ),
            Encoding::FlatUtf8(arena, ranges) => add(
                arc_slice(Arc::as_ptr(arena) as usize, arena.capacity(), seen)?,
                arc_slice(
                    Arc::as_ptr(ranges) as *const () as usize,
                    ranges
                        .capacity()
                        .checked_mul(std::mem::size_of::<Option<Range<usize>>>())
                        .ok_or_else(|| Error::Resource("vector backing size overflow".into()))?,
                    seen,
                )?,
            ),
            Encoding::FlatStruct { validity, children } => {
                let mut bytes = arc_slice(
                    Arc::as_ptr(children) as usize,
                    children
                        .capacity()
                        .checked_mul(std::mem::size_of::<Self>())
                        .ok_or_else(|| Error::Resource("vector backing size overflow".into()))?,
                    seen,
                )?;
                if let Some(validity) = validity {
                    bytes = add(
                        bytes,
                        arc_slice(
                            Arc::as_ptr(validity) as usize,
                            validity
                                .capacity()
                                .checked_mul(std::mem::size_of::<u64>())
                                .ok_or_else(|| {
                                    Error::Resource("vector backing size overflow".into())
                                })?,
                            seen,
                        )?,
                    )?;
                }
                for child in children.iter() {
                    bytes = add(bytes, child.retained_backing_bytes_inner(seen)?)?;
                }
                Ok(bytes)
            }
            Encoding::FlatList {
                validity,
                offsets,
                child,
            } => {
                let mut bytes = arc_slice(
                    Arc::as_ptr(offsets) as usize,
                    offsets
                        .capacity()
                        .checked_mul(std::mem::size_of::<usize>())
                        .ok_or_else(|| Error::Resource("vector backing size overflow".into()))?,
                    seen,
                )?;
                if let Some(validity) = validity {
                    bytes = add(
                        bytes,
                        arc_slice(
                            Arc::as_ptr(validity) as usize,
                            validity
                                .capacity()
                                .checked_mul(std::mem::size_of::<u64>())
                                .ok_or_else(|| {
                                    Error::Resource("vector backing size overflow".into())
                                })?,
                            seen,
                        )?,
                    )?;
                }
                add(bytes, child.retained_backing_bytes_inner(seen)?)
            }
            Encoding::Constant(value) => retained_value_backing_bytes(value, seen),
            Encoding::Dictionary(parent, selection) => add(
                arc_slice(
                    Arc::as_ptr(selection) as *const () as usize,
                    selection
                        .capacity()
                        .checked_mul(std::mem::size_of::<usize>())
                        .ok_or_else(|| Error::Resource("vector backing size overflow".into()))?,
                    seen,
                )?,
                parent.retained_backing_bytes_inner(seen)?,
            ),
            Encoding::Chunks(chunks, offsets) => {
                let mut bytes = add(
                    arc_slice(
                        Arc::as_ptr(chunks) as *const () as usize,
                        chunks
                            .capacity()
                            .checked_mul(std::mem::size_of::<Self>())
                            .ok_or_else(|| {
                                Error::Resource("vector backing size overflow".into())
                            })?,
                        seen,
                    )?,
                    arc_slice(
                        Arc::as_ptr(offsets) as *const () as usize,
                        offsets
                            .capacity()
                            .checked_mul(std::mem::size_of::<usize>())
                            .ok_or_else(|| {
                                Error::Resource("vector backing size overflow".into())
                            })?,
                        seen,
                    )?,
                )?;
                for chunk in chunks.iter() {
                    bytes = add(bytes, chunk.retained_backing_bytes_inner(seen)?)?;
                }
                Ok(bytes)
            }
        }
    }

    fn materialized_value_bytes(&self, index: usize) -> Result<usize> {
        let physical = self.offset + index;
        match &self.encoding {
            Encoding::FlatValues(values) => materialized_value_bytes(&values[physical]),
            Encoding::Constant(value) => materialized_value_bytes(value),
            Encoding::FlatUtf8(_, ranges) => std::mem::size_of::<Value>()
                .checked_add(ranges[physical].as_ref().map_or(0, |range| range.len()))
                .ok_or_else(|| Error::Resource("materialized string size overflow".into())),
            Encoding::FlatStruct { validity, children } => {
                if validity
                    .as_deref()
                    .is_some_and(|validity| !validity_is_set(validity, physical))
                {
                    return Ok(std::mem::size_of::<Value>());
                }
                children.iter().try_fold(
                    std::mem::size_of::<Value>()
                        .checked_add(std::mem::size_of::<NestedValue>())
                        .ok_or_else(|| Error::Resource("materialized STRUCT overflow".into()))?,
                    |bytes, child| {
                        bytes
                            .checked_add(child.materialized_value_bytes(physical)?)
                            .ok_or_else(|| Error::Resource("materialized STRUCT overflow".into()))
                    },
                )
            }
            Encoding::FlatList {
                validity,
                offsets,
                child,
            } => {
                if validity
                    .as_deref()
                    .is_some_and(|validity| !validity_is_set(validity, physical))
                {
                    return Ok(std::mem::size_of::<Value>());
                }
                (offsets[physical]..offsets[physical + 1]).try_fold(
                    std::mem::size_of::<Value>()
                        .checked_add(std::mem::size_of::<NestedValue>())
                        .ok_or_else(|| Error::Resource("materialized LIST overflow".into()))?,
                    |bytes, child_index| {
                        bytes
                            .checked_add(child.materialized_value_bytes(child_index)?)
                            .ok_or_else(|| Error::Resource("materialized LIST overflow".into()))
                    },
                )
            }
            Encoding::Dictionary(parent, selection) => {
                parent.materialized_value_bytes(selection[physical])
            }
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= physical)
                    .saturating_sub(1);
                chunks[segment].materialized_value_bytes(physical - offsets[segment])
            }
            _ => Ok(std::mem::size_of::<Value>()),
        }
    }

    pub(crate) fn try_materialized_value(&self, index: usize) -> Result<Value> {
        let physical = self.offset + index;
        match &self.encoding {
            Encoding::FlatValues(values) => try_clone_materialized_value(&values[physical]),
            Encoding::Constant(value) => try_clone_materialized_value(value),
            Encoding::FlatUtf8(arena, ranges) => match &ranges[physical] {
                Some(range) => try_materialized_string(&arena[range.clone()]).map(Value::Varchar),
                None => Ok(Value::Null),
            },
            Encoding::FlatStruct { validity, children } => {
                if validity
                    .as_deref()
                    .is_some_and(|validity| !validity_is_set(validity, physical))
                {
                    return Ok(Value::Null);
                }
                let mut values = Vec::new();
                values
                    .try_reserve_exact(children.len())
                    .map_err(|_| Error::Resource("cannot allocate materialized STRUCT".into()))?;
                for child in children.iter() {
                    values.push(child.try_materialized_value(physical)?);
                }
                Ok(Value::Nested(Arc::new(NestedValue {
                    data_type: self.data_type.clone(),
                    payload: NestedPayload::Struct(values),
                })))
            }
            Encoding::FlatList {
                validity,
                offsets,
                child,
            } => {
                if validity
                    .as_deref()
                    .is_some_and(|validity| !validity_is_set(validity, physical))
                {
                    return Ok(Value::Null);
                }
                let range = offsets[physical]..offsets[physical + 1];
                let mut values = Vec::new();
                values
                    .try_reserve_exact(range.len())
                    .map_err(|_| Error::Resource("cannot allocate materialized LIST".into()))?;
                for child_index in range {
                    values.push(child.try_materialized_value(child_index)?);
                }
                Ok(Value::Nested(Arc::new(NestedValue {
                    data_type: self.data_type.clone(),
                    payload: NestedPayload::Sequence(values),
                })))
            }
            Encoding::Dictionary(parent, selection) => {
                parent.try_materialized_value(selection[physical])
            }
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= physical)
                    .saturating_sub(1);
                chunks[segment].try_materialized_value(physical - offsets[segment])
            }
            _ => self
                .value(index)
                .ok_or_else(|| Error::Internal("invalid materialized row index".into())),
        }
    }
    /// Borrow a VARCHAR through any validated physical encoding. Outer None
    /// means wrong type/out of bounds; inner None is SQL NULL.
    pub(crate) fn varchar_at(&self, index: usize) -> Option<Option<&str>> {
        if self.data_type != DataType::Varchar || index >= self.count {
            return None;
        }
        self.varchar_at_validated(index)
    }

    /// Borrow VARCHAR after an exact bound type has validated this vector.
    /// The caller supplies the physical-type proof; bounds and SQL NULL remain
    /// checked here, including through immutable views.
    pub(crate) fn varchar_at_validated(&self, index: usize) -> Option<Option<&str>> {
        if index >= self.count {
            return None;
        }
        let index = self.offset + index;
        match &self.encoding {
            Encoding::FlatValues(values) => match values.get(index)? {
                Value::Varchar(value) => Some(Some(value.as_str())),
                Value::Null => Some(None),
                _ => None,
            },
            Encoding::FlatUtf8(arena, ranges) => ranges
                .get(index)
                .map(|range| range.as_ref().map(|range| &arena[range.clone()])),
            Encoding::Constant(Value::Varchar(value)) => Some(Some(value.as_str())),
            Encoding::Constant(Value::Null) => Some(None),
            Encoding::Dictionary(parent, selection) => {
                parent.varchar_at_validated(*selection.get(index)?)
            }
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= index)
                    .saturating_sub(1);
                chunks
                    .get(segment)?
                    .varchar_at_validated(index - offsets[segment])
            }
            _ => None,
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
            Encoding::FlatNullableSigned(values, validity) => values.append_nullable_values(
                self.offset..self.offset + self.count,
                validity,
                output,
            ),
            Encoding::FlatUtf8(arena, ranges) => output.extend(
                ranges[self.offset..self.offset + self.count]
                    .iter()
                    .map(|range| {
                        range.as_ref().map_or(Value::Null, |range| {
                            Value::Varchar(arena[range.clone()].to_owned())
                        })
                    }),
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
                } else if let Encoding::FlatNullableSigned(values, validity) = &parent.encoding {
                    values.append_nullable_indices(selection, parent.offset, validity, output);
                } else if let Some(values) = parent.flat_values() {
                    output.extend(selection.iter().map(|&index| values[index].clone()));
                } else {
                    output.extend(self.values());
                }
            }
            Encoding::FlatSigned(_)
            | Encoding::FlatDecimalI64(_)
            | Encoding::FlatStruct { .. }
            | Encoding::FlatList { .. }
            | Encoding::Chunks(_, _) => output.extend(self.values()),
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
    /// Borrow the exact logical range of an ordinary packed VARCHAR view.
    /// Selected, constant and chunked encodings retain their generic access
    /// paths; immediate dictionary consumers may map the packed parent once.
    pub(crate) fn flat_utf8(&self) -> Option<FlatUtf8<'_>> {
        match &self.encoding {
            Encoding::FlatUtf8(arena, ranges) => Some(FlatUtf8 {
                arena,
                ranges: &ranges[self.offset..self.offset + self.count],
            }),
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
    /// Borrow all all-valid BIGINT segments of this exact chunked view. This
    /// remains crate-private because callers must retain the generic path for
    /// selections, dictionaries, constants, nullable lanes, and mixed widths.
    pub(crate) fn flat_bigint_segments(&self) -> Option<Vec<&[i64]>> {
        let Encoding::Chunks(chunks, offsets) = &self.encoding else {
            return None;
        };
        if self.data_type != DataType::BigInt || !self.all_valid || self.count == 0 {
            return None;
        }
        let start = self.offset;
        let end = start.checked_add(self.count)?;
        let mut result = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let chunk_start = *offsets.get(index)?;
            let chunk_end = *offsets.get(index + 1)?;
            let from = start.max(chunk_start);
            let to = end.min(chunk_end);
            if from >= to {
                continue;
            }
            let child = chunk.flat_bigints()?;
            result.push(child.get(from - chunk_start..to - chunk_start)?);
        }
        (!result.is_empty()
            && result.iter().map(|segment| segment.len()).sum::<usize>() == self.count)
            .then_some(result)
    }
    /// Borrow a nullable native BIGINT lane and its aligned validity view.
    /// All-valid BIGINT columns intentionally remain available only through
    /// `flat_bigints`, preserving that accessor's established contract.
    pub(crate) fn flat_nullable_bigints(&self) -> Option<NullableBigIntView<'_>> {
        match &self.encoding {
            Encoding::FlatNullableSigned(SignedLanes::Big(values), validity) => {
                Some(NullableBigIntView {
                    values: &values[self.offset..self.offset + self.count],
                    validity,
                    validity_offset: self.offset,
                })
            }
            _ => None,
        }
    }
    /// Read one native signed coefficient without widening through `Value`.
    /// Only ordinary flat signed views are eligible.
    pub(crate) fn flat_signed_i64_at(&self, index: usize) -> Option<i64> {
        if index >= self.count {
            return None;
        }
        match &self.encoding {
            Encoding::FlatSigned(values) => values.i64_value(self.offset + index),
            Encoding::FlatNullableSigned(values, validity) => {
                let index = self.offset + index;
                validity_is_set(validity, index)
                    .then(|| values.i64_value(index))
                    .flatten()
            }
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
            Encoding::FlatNullableSigned(values, validity) => {
                if validity_is_set(validity, index) {
                    values
                        .i64_value(index)
                        .map(SignedI64At::Value)
                        .unwrap_or(SignedI64At::Unsupported)
                } else {
                    SignedI64At::Null
                }
            }
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
            Encoding::FlatDouble(_)
            | Encoding::FlatDecimalI64(_)
            | Encoding::FlatUtf8(_, _)
            | Encoding::FlatStruct { .. }
            | Encoding::FlatList { .. } => SignedI64At::Unsupported,
        }
    }
    /// Read one logical BOOLEAN without constructing or cloning a `Value`.
    /// Outer `None` means unsupported encoding/type or an out-of-bounds row;
    /// inner `None` is SQL NULL.
    #[inline(always)]
    pub(crate) fn boolean_at(&self, index: usize) -> Option<Option<bool>> {
        if self.data_type != DataType::Boolean || index >= self.count {
            return None;
        }
        let index = self.offset + index;
        match &self.encoding {
            Encoding::FlatValues(values) => boolean_value(values.get(index)),
            Encoding::Constant(value) => boolean_value(Some(value)),
            Encoding::Dictionary(parent, selection) => parent.boolean_at(*selection.get(index)?),
            Encoding::Chunks(chunks, offsets) => {
                let segment = offsets
                    .partition_point(|&end| end <= index)
                    .saturating_sub(1);
                chunks.get(segment)?.boolean_at(index - offsets[segment])
            }
            Encoding::FlatDouble(_)
            | Encoding::FlatSigned(_)
            | Encoding::FlatNullableSigned(_, _)
            | Encoding::FlatDecimalI64(_)
            | Encoding::FlatUtf8(_, _)
            | Encoding::FlatStruct { .. }
            | Encoding::FlatList { .. } => None,
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
#[inline(always)]
fn boolean_value(value: Option<&Value>) -> Option<Option<bool>> {
    match value? {
        Value::Null => Some(None),
        Value::Boolean(value) => Some(Some(*value)),
        _ => None,
    }
}

fn retained_value_backing_bytes(value: &Value, seen: &mut HashSet<usize>) -> Result<usize> {
    let add = |left: usize, right: usize| {
        left.checked_add(right)
            .ok_or_else(|| Error::Resource("value backing size overflow".into()))
    };
    match value {
        Value::Varchar(value) => Ok(value.capacity()),
        Value::Blob(value) => Ok(value.capacity()),
        Value::Extension(value) => Ok(value.bytes.capacity()),
        Value::Nested(value) => {
            let key = Arc::as_ptr(value) as usize;
            if !seen.insert(key) {
                return Ok(0);
            }
            let mut bytes = std::mem::size_of::<NestedValue>();
            match &value.payload {
                NestedPayload::Sequence(values) | NestedPayload::Struct(values) => {
                    bytes = add(
                        bytes,
                        values
                            .capacity()
                            .checked_mul(std::mem::size_of::<Value>())
                            .ok_or_else(|| Error::Resource("value backing size overflow".into()))?,
                    )?;
                    for value in values {
                        bytes = add(bytes, retained_value_backing_bytes(value, seen)?)?;
                    }
                }
                NestedPayload::Map(entries) => {
                    bytes = add(
                        bytes,
                        entries
                            .capacity()
                            .checked_mul(std::mem::size_of::<(Value, Value)>())
                            .ok_or_else(|| Error::Resource("value backing size overflow".into()))?,
                    )?;
                    for (key, value) in entries {
                        bytes = add(bytes, retained_value_backing_bytes(key, seen)?)?;
                        bytes = add(bytes, retained_value_backing_bytes(value, seen)?)?;
                    }
                }
                NestedPayload::Union { value, .. } | NestedPayload::Variant { value, .. } => {
                    bytes = add(bytes, retained_value_backing_bytes(value, seen)?)?;
                }
            }
            Ok(bytes)
        }
        _ => Ok(0),
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

    fn struct_type() -> DataType {
        NestedType::Struct(vec![
            ("a".into(), DataType::Varchar),
            ("b".into(), DataType::Varchar),
        ])
        .data_type()
    }

    fn checked_struct(
        children: [Vector; 2],
        count: usize,
        query: &crate::parallel::QueryContext,
    ) -> Result<Vector> {
        let mut admission = MetadataAdmission::new();
        let mut output = Vec::new();
        admission.try_reserve_vec(&mut output, children.len(), query, "test STRUCT metadata")?;
        output.extend(children);
        Vector::flat_struct_checked(struct_type(), count, None, output, admission, query)
    }

    fn checked_list(
        data_type: DataType,
        validity: Option<&[u64]>,
        offsets: &[usize],
        child: Vector,
        query: &crate::parallel::QueryContext,
    ) -> Result<Vector> {
        let mut admission = MetadataAdmission::new();
        let validity = validity
            .map(|validity| {
                let mut output = Vec::new();
                admission.try_reserve_vec(
                    &mut output,
                    validity.len(),
                    query,
                    "test LIST validity",
                )?;
                output.extend_from_slice(validity);
                Ok::<Vec<u64>, Error>(output)
            })
            .transpose()?;
        let mut output_offsets = Vec::new();
        admission.try_reserve_vec(
            &mut output_offsets,
            offsets.len(),
            query,
            "test LIST offsets",
        )?;
        output_offsets.extend_from_slice(offsets);
        Vector::flat_list_checked(
            data_type,
            offsets.len().saturating_sub(1),
            validity,
            output_offsets,
            child,
            admission,
            query,
        )
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn packed_utf8_validates_ranges_and_preserves_views_materialization_and_shared_ownership()
    -> Result<()> {
        let arena = Arc::new(String::from("prefix|é|a\0🦆|suffix"));
        let range = |text: &str| {
            let start = arena.find(text).expect("test substring");
            start..start + text.len()
        };
        let empty = arena.find('|').expect("test delimiter");
        let packed = Vector::packed_utf8(
            arena.clone(),
            vec![
                Some(range("suffix")),
                None,
                Some(range("é")),
                Some(empty..empty),
                Some(range("a\0🦆")),
                Some(range("é")),
            ],
        )?;
        assert!(!packed.all_valid());
        assert_eq!(
            packed
                .flat_utf8()
                .expect("packed flat view")
                .iter()
                .collect::<Vec<_>>(),
            vec![
                Some("suffix"),
                None,
                Some("é"),
                Some(""),
                Some("a\0🦆"),
                Some("é")
            ]
        );
        assert_eq!(
            packed.values().collect::<Vec<_>>(),
            vec![
                Value::Varchar("suffix".into()),
                Value::Null,
                Value::Varchar("é".into()),
                Value::Varchar(String::new()),
                Value::Varchar("a\0🦆".into()),
                Value::Varchar("é".into()),
            ]
        );
        assert_eq!(packed.varchar_at(0), Some(Some("suffix")));
        assert_eq!(packed.varchar_at(1), Some(None));

        let sliced = packed.slice(1, 4)?;
        assert_eq!(
            sliced
                .flat_utf8()
                .expect("sliced packed view")
                .iter()
                .collect::<Vec<_>>(),
            vec![None, Some("é"), Some(""), Some("a\0🦆")]
        );
        let selected = Arc::new(packed.clone()).select(vec![4, 0, 1, 2])?;
        assert!(selected.flat_utf8().is_none());
        assert_eq!(
            selected.values().collect::<Vec<_>>(),
            vec![
                Value::Varchar("a\0🦆".into()),
                Value::Varchar("suffix".into()),
                Value::Null,
                Value::Varchar("é".into()),
            ]
        );
        assert_eq!(selected.varchar_at(0), Some(Some("a\0🦆")));
        assert_eq!(selected.varchar_at(2), Some(None));

        let contiguous = Vector::concatenate(
            DataType::Varchar,
            &[packed.slice(0, 2)?, packed.slice(2, 2)?],
        )?;
        assert!(contiguous.flat_utf8().is_some());
        assert_eq!(
            contiguous.values().collect::<Vec<_>>(),
            packed.slice(0, 4)?.values().collect::<Vec<_>>()
        );

        let chunked = Vector::chunked(
            DataType::Varchar,
            vec![packed.slice(0, 3)?, packed.slice(3, 3)?],
        )?;
        assert!(chunked.flat_utf8().is_none());
        assert!(chunked.slice(3, 2)?.flat_utf8().is_some());
        assert_eq!(
            chunked.values().collect::<Vec<_>>(),
            packed.values().collect::<Vec<_>>()
        );
        assert_eq!(chunked.varchar_at(2), Some(Some("é")));
        assert_eq!(chunked.varchar_at(4), Some(Some("a\0🦆")));

        let primary = Vector::packed_utf8(
            arena.clone(),
            vec![Some(range("prefix")), Some(range("suffix"))],
        )?;
        let sibling = Vector::packed_utf8(arena.clone(), vec![Some(range("é")), None])?;
        let chunk = DataChunk::new(vec![primary, sibling], 2)?;
        let retained = chunk.project(&[1])?;
        drop(chunk);
        drop(arena);
        assert_eq!(
            retained.columns()[0].value(0),
            Some(Value::Varchar("é".into()))
        );
        assert_eq!(retained.columns()[0].value(1), Some(Value::Null));

        let utf8 = Arc::new(String::from("é"));
        assert!(Vector::packed_utf8(utf8.clone(), vec![Some(2..1)]).is_err());
        assert!(Vector::packed_utf8(utf8.clone(), vec![Some(0..3)]).is_err());
        assert!(Vector::packed_utf8(utf8, vec![Some(1..2)]).is_err());
        Ok(())
    }

    #[test]
    fn columnar_nested_materializes_and_resolves_views_without_losing_null_or_empty() -> Result<()>
    {
        let query = crate::parallel::QueryContext::background();
        let arena = Arc::new(String::from("a0b0a1b1"));
        let structs = checked_struct(
            [
                Vector::packed_utf8(arena.clone(), vec![Some(0..2), Some(4..6)])?,
                Vector::packed_utf8(arena, vec![Some(2..4), Some(6..8)])?,
            ],
            2,
            &query,
        )?;
        let selected = Arc::new(structs.clone()).select(vec![1, 0])?;
        let sliced = selected.slice(1, 1)?;
        assert!(matches!(
            sliced.nested_row_at(0),
            Some(NestedRowRef::Struct { .. })
        ));
        assert_eq!(sliced.value(0), structs.value(0));

        let list_type = NestedType::List(struct_type()).data_type();
        let lists = checked_list(
            list_type.clone(),
            Some(&[0b101]),
            &[0, 1, 1, 2],
            structs,
            &query,
        )?;
        assert!(matches!(
            lists.nested_row_at(0),
            Some(NestedRowRef::List { .. })
        ));
        assert!(matches!(lists.nested_row_at(1), Some(NestedRowRef::Null)));
        assert!(matches!(
            lists.nested_row_at(2),
            Some(NestedRowRef::List { .. })
        ));
        assert_eq!(lists.value(1), Some(Value::Null));

        let empty_child = Vector::flat(struct_type(), Vec::new())?;
        let empty = checked_list(list_type, None, &[0, 0], empty_child, &query)?;
        assert!(matches!(
            empty.value(0),
            Some(Value::Nested(value)) if matches!(&value.payload, NestedPayload::Sequence(values) if values.is_empty())
        ));
        Ok(())
    }

    #[test]
    fn null_access_resolves_physical_views_without_materializing_nested_values() -> Result<()> {
        let query = crate::parallel::QueryContext::background();
        let child = checked_struct(
            [
                Vector::packed_utf8(Arc::new("ab".into()), vec![Some(0..1), Some(1..2)])?,
                Vector::packed_utf8(Arc::new("cd".into()), vec![Some(0..1), Some(1..2)])?,
            ],
            2,
            &query,
        )?;
        let list_type = NestedType::List(struct_type()).data_type();
        let parent = checked_list(
            list_type.clone(),
            Some(&[0b101]),
            &[0, 1, 1, 2],
            child,
            &query,
        )?;
        assert!(parent.has_nested_row_access());
        assert_eq!(parent.is_null_at(0), Some(false));
        assert_eq!(parent.is_null_at(1), Some(true));
        assert_eq!(parent.is_null_at(2), Some(false));
        assert_eq!(parent.is_null_at(3), None);
        assert_eq!(parent.slice(1, 2)?.is_null_at(0), Some(true));

        let selected = Arc::new(parent.clone()).select(vec![2, 1, 0])?;
        assert!(selected.has_nested_row_access());
        assert_eq!(selected.is_null_at(0), Some(false));
        assert_eq!(selected.is_null_at(1), Some(true));
        assert_eq!(selected.is_null_at(2), Some(false));
        let chunked = Vector::chunked(
            list_type.clone(),
            vec![parent.slice(0, 2)?, parent.slice(2, 1)?],
        )?;
        assert!(chunked.has_nested_row_access());
        assert_eq!(chunked.is_null_at(0), Some(false));
        assert_eq!(chunked.is_null_at(1), Some(true));
        assert_eq!(chunked.is_null_at(2), Some(false));
        assert_eq!(chunked.is_null_at(3), None);
        let constant = Vector::constant(list_type, Value::Null, 2)?;
        assert!(constant.has_nested_row_access());
        assert_eq!(constant.is_null_at(1), Some(true));
        assert!(!Vector::flat(DataType::Integer, vec![Value::Integer(1)])?.has_nested_row_access());
        Ok(())
    }

    #[test]
    fn exact_nested_compare_and_render_recurse_without_parent_materialization() -> Result<()> {
        use crate::common::{
            cast::{CastMode, CastRegistry},
            type_registry::TypeRegistry,
        };

        let query = crate::parallel::QueryContext::background();
        let strings = |text: &str, ranges: Vec<Option<Range<usize>>>| {
            Vector::packed_utf8(Arc::new(text.into()), ranges)
        };
        let left_structs = checked_struct(
            [
                strings("a0a1", vec![Some(0..2), Some(2..4)])?,
                strings("b0b1", vec![Some(0..2), Some(2..4)])?,
            ],
            2,
            &query,
        )?;
        let right_structs = checked_struct(
            [
                strings("a0a1", vec![Some(0..2), Some(2..4)])?,
                strings("b0b2", vec![Some(0..2), Some(2..4)])?,
            ],
            2,
            &query,
        )?;
        let list_type = NestedType::List(struct_type()).data_type();
        let left = checked_list(list_type.clone(), None, &[0, 1, 2], left_structs, &query)?;
        let right = checked_list(list_type.clone(), None, &[0, 1, 2], right_structs, &query)?;
        let bound = TypeRegistry::builtins().bind(&list_type)?;
        assert_eq!(
            bound.compare_batch(&left, &right, &query)?,
            vec![
                Some(std::cmp::Ordering::Equal),
                Some(std::cmp::Ordering::Less)
            ]
        );
        assert!(
            matches!(left.nested_row_at(0), Some(NestedRowRef::List { child, .. }) if matches!(child.nested_row_at(0), Some(NestedRowRef::Struct { .. })))
        );
        assert_eq!(
            bound.select_comparison(
                &left,
                &right,
                crate::common::type_registry::ComparisonPredicate {
                    less: true,
                    equal: false,
                    greater: false,
                },
                &query,
            )?,
            vec![1]
        );

        let scalar_struct = NestedValue::value(
            struct_type(),
            NestedPayload::Struct(vec![
                Value::Varchar("a0".into()),
                Value::Varchar("b0".into()),
            ]),
        )?;
        let scalar_list = NestedValue::value(
            list_type.clone(),
            NestedPayload::Sequence(vec![scalar_struct]),
        )?;
        let scalar = Vector::flat(list_type.clone(), vec![scalar_list, Value::Null])?;
        let nullable_child = match left.nested_row_at(0) {
            Some(NestedRowRef::List { child, .. }) => child.slice(0, 1)?,
            _ => panic!("columnar LIST child"),
        };
        let nullable = checked_list(
            list_type.clone(),
            Some(&[0b01]),
            &[0, 1, 1],
            nullable_child,
            &query,
        )?;
        assert_eq!(
            bound.compare_batch(&nullable, &scalar, &query)?,
            vec![Some(std::cmp::Ordering::Equal), None]
        );
        assert!(
            matches!(nullable.nested_row_at(0), Some(NestedRowRef::List { child, .. }) if matches!(child.nested_row_at(0), Some(NestedRowRef::Struct { .. })))
        );

        let cast = CastRegistry::builtins().bind(
            &list_type,
            &DataType::Varchar,
            CastMode::Explicit,
            query.types(),
        )?;
        let rendered = cast.apply_batch(&left, &query)?;
        assert_eq!(rendered.varchar_at(0), Some(Some("[{'a': a0, 'b': b0}]")));
        assert_eq!(rendered.varchar_at(1), Some(Some("[{'a': a1, 'b': b1}]")));
        assert!(
            matches!(left.nested_row_at(1), Some(NestedRowRef::List { child, .. }) if matches!(child.nested_row_at(1), Some(NestedRowRef::Struct { .. })))
        );
        Ok(())
    }

    #[test]
    fn columnar_nested_rejects_malformed_shape_and_accounts_shared_backing_once() -> Result<()> {
        let query = crate::parallel::QueryContext::background();
        let arena = Arc::new(String::from("aabb"));
        let left = Vector::packed_utf8(arena.clone(), vec![Some(0..1), Some(1..2)])?;
        let right = Vector::packed_utf8(arena.clone(), vec![Some(2..3), Some(3..4)])?;
        let mut struct_admission = MetadataAdmission::new();
        let mut children = Vec::new();
        struct_admission.try_reserve_vec(&mut children, 4, &query, "test STRUCT spare metadata")?;
        children.push(left);
        children.push(right);
        let child_capacity = children.capacity();
        let structs = Vector::flat_struct_checked(
            struct_type(),
            2,
            None,
            children,
            struct_admission,
            &query,
        )?;
        let expected = arena.capacity()
            + 4 * std::mem::size_of::<Option<Range<usize>>>()
            + child_capacity * std::mem::size_of::<Vector>();
        assert_eq!(structs.retained_backing_bytes()?, expected);
        assert!(
            checked_list(
                NestedType::List(struct_type()).data_type(),
                None,
                &[0, 1, 1],
                structs.clone(),
                &query,
            )
            .is_err()
        );
        assert!(
            checked_list(
                NestedType::List(struct_type()).data_type(),
                Some(&[0b10]),
                &[0, 1, 2],
                structs,
                &query,
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn retained_backing_counts_mixed_scalar_payloads_and_deduplicates_nested_arcs() -> Result<()> {
        let mut text = String::with_capacity(31);
        text.push_str("payload");
        let text_capacity = text.capacity();
        let mut payload = Vec::with_capacity(3);
        payload.push(Value::Varchar(text));
        let payload_capacity = payload.capacity();
        let nested_type = NestedType::List(DataType::Varchar).data_type();
        let shared = Arc::new(NestedValue {
            data_type: nested_type.clone(),
            payload: NestedPayload::Sequence(payload),
        });
        let mut values = Vec::with_capacity(4);
        values.push(Value::Nested(shared.clone()));
        values.push(Value::Nested(shared));
        let values_capacity = values.capacity();
        let vector = Vector::flat(nested_type, values)?;
        assert_eq!(
            vector.retained_backing_bytes()?,
            values_capacity * std::mem::size_of::<Value>()
                + std::mem::size_of::<NestedValue>()
                + payload_capacity * std::mem::size_of::<Value>()
                + text_capacity
        );

        let mut constant = String::with_capacity(47);
        constant.push('x');
        let capacity = constant.capacity();
        let vector = Vector::constant(DataType::Varchar, Value::Varchar(constant), 100)?;
        assert_eq!(vector.retained_backing_bytes()?, capacity);
        Ok(())
    }

    #[test]
    fn columnar_metadata_admission_is_atomic_and_views_retain_the_charge() -> Result<()> {
        use crate::parallel::{MemoryPool, QueryContext};

        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let children = [
            Vector::flat(DataType::Varchar, vec![Value::Varchar("x".into())])?,
            Vector::flat(DataType::Varchar, vec![Value::Varchar("y".into())])?,
        ];
        let bytes = children.len() * std::mem::size_of::<Vector>();
        pool.publish_limit(Some(bytes.saturating_sub(1)))?;
        let mut rejected_admission = MetadataAdmission::new();
        let mut rejected_children = Vec::<Vector>::new();
        assert!(
            rejected_admission
                .try_reserve_vec(
                    &mut rejected_children,
                    children.len(),
                    &query,
                    "test rejected STRUCT metadata",
                )
                .is_err()
        );
        assert_eq!(pool.used()?, 0);

        pool.publish_limit(Some(bytes))?;
        let mut admission = MetadataAdmission::new();
        let mut admitted_children = Vec::new();
        admission.try_reserve_vec(
            &mut admitted_children,
            children.len(),
            &query,
            "test STRUCT metadata",
        )?;
        admitted_children.extend(children);
        let parent = Vector::flat_struct_checked(
            struct_type(),
            1,
            None,
            admitted_children,
            admission,
            &query,
        )?;
        assert_eq!(pool.used()?, bytes);
        let view = Arc::new(parent.clone()).select(vec![0])?;
        drop(parent);
        assert_eq!(pool.used()?, bytes);
        drop(view);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[test]
    fn metadata_admission_geometric_growth_charges_spare_capacity_before_allocation() -> Result<()>
    {
        use crate::parallel::{MemoryPool, QueryContext};

        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let mut admission = MetadataAdmission::new();
        let mut values = Vec::<u64>::new();
        admission.try_reserve_vec(&mut values, 1, &query, "test vector growth")?;
        values.push(1);
        let first = values.capacity() * std::mem::size_of::<u64>();
        assert_eq!(pool.used()?, first);

        pool.publish_limit(Some(first + std::mem::size_of::<u64>() - 1))?;
        assert!(
            admission
                .try_reserve_vec(&mut values, 1, &query, "test vector growth")
                .is_err()
        );
        assert_eq!(values.capacity() * std::mem::size_of::<u64>(), first);
        assert_eq!(pool.used()?, first);

        pool.publish_limit(Some(first * 2))?;
        admission.try_reserve_vec(&mut values, 1, &query, "test vector growth")?;
        assert!(values.capacity() >= 2);
        let bytes = values.capacity() * std::mem::size_of::<u64>();
        assert_eq!(pool.used()?, bytes);
        let guard = admission.finish(bytes, &query)?.expect("growth charge");
        drop(values);
        assert_eq!(pool.used()?, bytes);
        drop(guard);
        assert_eq!(pool.used()?, 0);

        let mut admission = MetadataAdmission::new();
        let mut text = String::new();
        admission.try_reserve_string(&mut text, 3, &query, "test string growth")?;
        text.push_str("abc");
        admission.try_reserve_string(&mut text, 1, &query, "test string growth")?;
        assert!(text.capacity() >= 6);
        let bytes = text.capacity();
        assert_eq!(pool.used()?, bytes);
        let guard = admission
            .finish(bytes, &query)?
            .expect("string growth charge");
        drop(text);
        drop(guard);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

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
        assert_eq!(ascending.flat_bigints(), Some(&[-2, -2, 4][..]));
        assert!(ascending.flat_nullable_bigints().is_none());
        assert!(!descending.numeric_ascending());
        assert!(!nullable.numeric_ascending());
        assert!(nullable.flat_bigints().is_none());
        assert!(nullable.flat_nullable_bigints().is_some());
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
    fn signed_lanes_use_declared_widths_for_valid_and_nullable_values() -> Result<()> {
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
        assert!(nullable.flat_values().is_none());
        assert!(nullable.flat_bigints().is_none());
        let nullable_view = nullable
            .flat_nullable_bigints()
            .expect("nullable BIGINT lane");
        assert_eq!(nullable_view.len(), 2);
        assert_eq!(nullable_view.value(0), Some(Some(1)));
        assert_eq!(nullable_view.value(1), Some(None));
        assert_eq!(nullable_view.value(2), None);
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
    fn nullable_signed_lanes_preserve_every_width_and_extrema() -> Result<()> {
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
            let vector = Vector::flat(
                data_type,
                vec![
                    Value::Integer(minimum),
                    Value::Null,
                    Value::Integer(maximum),
                ],
            )?;
            assert_eq!(
                vector.values().collect::<Vec<_>>(),
                vec![
                    Value::Integer(minimum),
                    Value::Null,
                    Value::Integer(maximum)
                ]
            );
            assert_eq!(vector.signed_i64_at(1), SignedI64At::Null);
            match &vector.encoding {
                Encoding::FlatNullableSigned(lanes, validity) => {
                    let lane_width = match lanes {
                        SignedLanes::Tiny(values) => {
                            std::mem::size_of_val(values.as_slice()) / values.len()
                        }
                        SignedLanes::Small(values) => {
                            std::mem::size_of_val(values.as_slice()) / values.len()
                        }
                        SignedLanes::Integer(values) => {
                            std::mem::size_of_val(values.as_slice()) / values.len()
                        }
                        SignedLanes::Big(values) => {
                            std::mem::size_of_val(values.as_slice()) / values.len()
                        }
                        SignedLanes::Huge(values) => {
                            std::mem::size_of_val(values.as_slice()) / values.len()
                        }
                    };
                    assert_eq!(lane_width, width);
                    assert_eq!(validity.as_slice(), &[0b101]);
                }
                _ => panic!("nullable signed column requires compact lanes"),
            }
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn nullable_bigint_bitmap_and_slices_cross_63_64_65_boundaries() -> Result<()> {
        for count in [63_usize, 64, 65] {
            let vector = Vector::try_bigints(
                (0..count).map(|index| Ok((index + 1 != count).then_some(index as i64))),
            )?;
            let view = vector
                .flat_nullable_bigints()
                .expect("nullable BIGINT lane");
            assert_eq!(view.len(), count);
            assert_eq!(view.value(count - 1), Some(None));
            assert_eq!(view.value(count), None);
            let Encoding::FlatNullableSigned(_, validity) = &vector.encoding else {
                panic!("nullable BIGINT encoding");
            };
            assert_eq!(validity.len(), count.div_ceil(64));
        }

        let nulls = [0_usize, 62, 63, 64, 65, 127, 128, 129];
        let vector = Vector::try_bigints(
            (0..130).map(|index| Ok((!nulls.contains(&index)).then_some(index as i64))),
        )?;
        for index in 0..130 {
            assert_eq!(
                vector.signed_i64_at(index),
                if nulls.contains(&index) {
                    SignedI64At::Null
                } else {
                    SignedI64At::Value(index as i64)
                }
            );
        }
        let sliced = vector.slice(62, 5)?;
        let view = sliced
            .flat_nullable_bigints()
            .expect("sliced nullable BIGINT lane");
        assert_eq!(view.len(), 5);
        assert_eq!(view.value(0), Some(None));
        assert_eq!(view.value(1), Some(None));
        assert_eq!(view.value(2), Some(None));
        assert_eq!(view.value(3), Some(None));
        assert_eq!(view.value(4), Some(Some(66)));
        assert!(!view.is_valid(0));
        assert!(view.is_valid(4));
        assert_eq!(sliced.flat_signed_i64_at(4), Some(66));
        assert_eq!(sliced.flat_signed_i64_at(5), None);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn nullable_signed_views_survive_dictionary_chunks_and_concatenation() -> Result<()> {
        let source = Arc::new(Vector::try_bigints([
            Ok(Some(i64::MIN)),
            Ok(None),
            Ok(Some(7)),
            Ok(Some(i64::MAX)),
        ])?);
        let contiguous = Vector::concatenate(
            DataType::BigInt,
            &[source.slice(0, 2)?, source.slice(2, 2)?],
        )?;
        assert!(contiguous.flat_nullable_bigints().is_some());
        assert_eq!(
            contiguous.values().collect::<Vec<_>>(),
            source.values().collect::<Vec<_>>()
        );

        let dictionary = source.select(vec![3, 1, 0, 2, 1])?;
        let mut appended = Vec::new();
        dictionary.append_to(&mut appended);
        assert_eq!(
            appended,
            vec![
                Value::Integer(i128::from(i64::MAX)),
                Value::Null,
                Value::Integer(i128::from(i64::MIN)),
                Value::Integer(7),
                Value::Null,
            ]
        );
        assert_eq!(dictionary.signed_i64_at(0), SignedI64At::Value(i64::MAX));
        assert_eq!(dictionary.signed_i64_at(1), SignedI64At::Null);

        let copied = Vector::concatenate(
            DataType::BigInt,
            &[source.slice(3, 1)?, source.slice(1, 2)?],
        )?;
        let copied_view = copied
            .flat_nullable_bigints()
            .expect("copied nullable BIGINT lane");
        assert_eq!(copied_view.value(0), Some(Some(i64::MAX)));
        assert_eq!(copied_view.value(1), Some(None));
        assert_eq!(copied_view.value(2), Some(Some(7)));

        let chunks = Vector::chunked(
            DataType::BigInt,
            vec![source.slice(0, 2)?, source.slice(2, 2)?],
        )?;
        assert_eq!(chunks.signed_i64_at(0), SignedI64At::Value(i64::MIN));
        assert_eq!(chunks.signed_i64_at(1), SignedI64At::Null);
        assert_eq!(chunks.signed_i64_at(3), SignedI64At::Value(i64::MAX));
        assert_eq!(
            chunks.slice(1, 2)?.values().collect::<Vec<_>>(),
            vec![Value::Null, Value::Integer(7),]
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn nullable_hugeint_constructor_keeps_full_width_lane() -> Result<()> {
        let vector = Vector::try_hugeints([Ok(Some(i128::MIN)), Ok(None), Ok(Some(i128::MAX))])?;
        assert_eq!(
            vector.values().collect::<Vec<_>>(),
            vec![
                Value::Integer(i128::MIN),
                Value::Null,
                Value::Integer(i128::MAX)
            ]
        );
        assert!(matches!(
            vector.encoding,
            Encoding::FlatNullableSigned(SignedLanes::Huge(_), _)
        ));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn boolean_accessor_resolves_flat_constant_dictionary_and_chunks() -> Result<()> {
        let flat = Arc::new(Vector::flat(
            DataType::Boolean,
            vec![Value::Boolean(true), Value::Null, Value::Boolean(false)],
        )?);
        assert_eq!(flat.boolean_at(0), Some(Some(true)));
        assert_eq!(flat.boolean_at(1), Some(None));
        assert_eq!(flat.boolean_at(2), Some(Some(false)));
        assert_eq!(flat.boolean_at(3), None);
        assert_eq!(flat.slice(1, 2)?.boolean_at(0), Some(None));
        assert_eq!(flat.slice(1, 2)?.boolean_at(1), Some(Some(false)));

        let value = Vector::constant(DataType::Boolean, Value::Boolean(true), 2)?;
        let null = Vector::constant(DataType::Boolean, Value::Null, 2)?;
        assert_eq!(value.boolean_at(1), Some(Some(true)));
        assert_eq!(null.boolean_at(1), Some(None));

        let selected = Arc::new(flat.select(vec![2, 1, 0, 2])?);
        let nested = selected.select(vec![2, 0, 1])?.slice(1, 2)?;
        assert_eq!(nested.boolean_at(0), Some(Some(false)));
        assert_eq!(nested.boolean_at(1), Some(None));

        let chunks = Vector::chunked(
            DataType::Boolean,
            vec![flat.slice(0, 2)?, flat.slice(2, 1)?],
        )?;
        assert_eq!(chunks.boolean_at(0), Some(Some(true)));
        assert_eq!(chunks.boolean_at(1), Some(None));
        assert_eq!(chunks.boolean_at(2), Some(Some(false)));
        assert_eq!(chunks.boolean_at(3), None);
        assert_eq!(
            Vector::flat(DataType::BigInt, vec![Value::Integer(1)])?.boolean_at(0),
            None
        );
        assert_eq!(
            Vector::constant(DataType::BigInt, Value::Null, 1)?.boolean_at(0),
            None
        );
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

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn chunked_bigint_segments_honor_outer_and_child_slices_and_reject_other_encodings()
    -> Result<()> {
        let first = Vector::try_bigints((0..5).map(|v| Ok(Some(v))))?.slice(1, 3)?;
        let second = Vector::try_bigints((5..10).map(|v| Ok(Some(v))))?.slice(1, 3)?;
        let chunks = Vector::chunked(DataType::BigInt, vec![first, second])?.slice(1, 4)?;
        assert_eq!(
            chunks.flat_bigint_segments().unwrap().concat(),
            vec![2, 3, 6, 7]
        );
        assert!(
            Vector::chunked(
                DataType::BigInt,
                vec![Vector::try_bigints([Ok(Some(1)), Ok(None)])?]
            )?
            .flat_bigint_segments()
            .is_none()
        );
        let flat = Arc::new(Vector::try_bigints([Ok(Some(1)), Ok(Some(2))])?);
        let selected = flat.select(vec![1, 0])?;
        assert!(selected.flat_bigint_segments().is_none());
        assert!(
            Vector::chunked(DataType::BigInt, vec![selected])?
                .flat_bigint_segments()
                .is_none()
        );
        assert!(
            Vector::chunked(
                DataType::BigInt,
                vec![Vector::constant(DataType::BigInt, Value::Integer(1), 2)?]
            )?
            .flat_bigint_segments()
            .is_none()
        );
        let nested = Vector::chunked(DataType::BigInt, vec![flat.as_ref().clone()])?;
        assert!(
            Vector::chunked(DataType::BigInt, vec![nested])?
                .flat_bigint_segments()
                .is_none()
        );
        assert!(
            Vector::constant(DataType::BigInt, Value::Integer(1), 2)?
                .flat_bigint_segments()
                .is_none()
        );
        assert!(
            Vector::chunked(DataType::BigInt, vec![Vector::try_bigints([])?])?
                .flat_bigint_segments()
                .is_none()
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct DataChunk {
    columns: Vec<Vector>,
    count: usize,
    // Keep zero-column batches accountable. Columns also retain the charge,
    // so independently cloned column views cannot outlive their accounting.
    reservation: Option<crate::parallel::Reservation>,
}

/// A complete single-column flat value allocation and its retained execution
/// charge. This is an ownership handoff between engine consumers; exposing the
/// fields outside the common module would permit uncharged application export.
pub(crate) struct OwnedFlatValues {
    pub(crate) values: Vec<Value>,
    pub(crate) reservation: Option<crate::parallel::Reservation>,
}

pub(crate) enum OwnedFlatValuesHandoff {
    Owned(OwnedFlatValues),
    Unsupported(DataChunk),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DataChunk {
    pub fn new(columns: Vec<Vector>, count: usize) -> Result<Self> {
        if columns.iter().any(|v| v.len() != count) {
            return Err(Error::Internal(
                "chunk columns differ in cardinality".into(),
            ));
        }
        Ok(Self {
            columns,
            count,
            reservation: None,
        })
    }
    pub(crate) fn with_reservation(mut self, reservation: crate::parallel::Reservation) -> Self {
        for column in &mut self.columns {
            column.retain_reservation(reservation.clone());
        }
        self.reservation = Some(reservation);
        self
    }
    pub(crate) fn reservation_guard(&self) -> crate::parallel::Reservation {
        crate::parallel::Reservation::merge(
            self.reservation
                .iter()
                .cloned()
                .chain(
                    self.columns
                        .iter()
                        .filter_map(|column| column.reservation.clone()),
                )
                .collect(),
        )
    }
    pub(crate) fn reservation_pool(&self) -> Option<&Arc<crate::parallel::MemoryPool>> {
        self.reservation
            .as_ref()
            .and_then(crate::parallel::Reservation::memory_pool)
            .or_else(|| {
                self.columns.iter().find_map(|column| {
                    column
                        .reservation
                        .as_ref()
                        .and_then(crate::parallel::Reservation::memory_pool)
                })
            })
    }
    /// Consume a complete one-column generic flat allocation without cloning
    /// heap payloads. Slices, selected encodings, shared flats and typed lanes
    /// return the original chunk for the ordinary materialization path.
    pub(crate) fn into_owned_single_flat_values(self) -> OwnedFlatValuesHandoff {
        let eligible = match self.columns.as_slice() {
            [column] if column.offset == 0 && column.count == self.count => {
                matches!(
                    &column.encoding,
                    Encoding::FlatValues(values)
                        if values.len() == self.count && Arc::strong_count(values) == 1
                )
            }
            _ => false,
        };
        if !eligible {
            return OwnedFlatValuesHandoff::Unsupported(self);
        }
        let Self {
            mut columns,
            reservation,
            ..
        } = self;
        let Vector {
            encoding,
            reservation: column_reservation,
            ..
        } = columns.pop().expect("eligible single column");
        let Encoding::FlatValues(values) = encoding else {
            unreachable!("eligible generic flat column")
        };
        let values = Arc::try_unwrap(values)
            .unwrap_or_else(|_| unreachable!("eligible flat allocation became shared"));
        OwnedFlatValuesHandoff::Owned(OwnedFlatValues {
            values,
            // A chunk-level token is also retained by its column. Prefer the
            // column guard because it additionally includes any earlier guard.
            reservation: column_reservation.or(reservation),
        })
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
    /// Copy participating materialized rows while retaining both source and
    /// destination charges during the transpose. Generated scalar expressions
    /// and enduring source storage remain outside this execution-copy domain.
    pub(crate) fn copy_rows_with_reservation(
        types: &[DataType],
        rows: &[Row],
        pool: &Arc<crate::parallel::MemoryPool>,
        query: &crate::parallel::QueryContext,
    ) -> Result<Self> {
        let overflow = || Error::Resource("materialized chunk size overflow".into());
        let bytes = rows.iter().flatten().try_fold(0usize, |bytes, value| {
            bytes
                .checked_add(materialized_value_bytes(value)?)
                .ok_or_else(overflow)
        })?;
        let reservation = pool.reserve(bytes, query)?;
        let slots = rows
            .len()
            .checked_mul(types.len())
            .and_then(|count| count.checked_mul(std::mem::size_of::<Value>()))
            .ok_or_else(overflow)?;
        let row_slots = rows
            .len()
            .checked_mul(std::mem::size_of::<Row>())
            .ok_or_else(overflow)?;
        let _temporary = pool.reserve(slots.checked_add(row_slots).ok_or_else(overflow)?, query)?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(rows.len())
            .map_err(|_| Error::Resource("cannot allocate materialized chunk rows".into()))?;
        for row in rows {
            let mut values = Vec::new();
            values
                .try_reserve_exact(row.len())
                .map_err(|_| Error::Resource("cannot allocate materialized chunk values".into()))?;
            for value in row {
                values.push(try_clone_materialized_value(value)?);
            }
            copied.push(values);
        }
        Ok(Self::from_owned_rows(types, copied)?.with_reservation(reservation))
    }
    /// Transpose rows whose ownership ends at this boundary without cloning
    /// heap-owning scalar payloads. Width is checked before any row is moved;
    /// the ordinary vector constructors retain type and encoding validation.
    pub(crate) fn from_owned_rows(types: &[DataType], rows: Vec<Row>) -> Result<Self> {
        if rows.iter().any(|row| row.len() != types.len()) {
            return Err(Error::Internal("row width differs from schema".into()));
        }
        let count = rows.len();
        let mut values = Vec::new();
        values
            .try_reserve(types.len())
            .map_err(|_| Error::Resource("chunk column allocation failed".into()))?;
        for _ in types {
            let mut column = Vec::new();
            column
                .try_reserve(count)
                .map_err(|_| Error::Resource("chunk column allocation failed".into()))?;
            values.push(column);
        }
        for row in rows {
            for (column, value) in values.iter_mut().zip(row) {
                column.push(value);
            }
        }
        let columns = types
            .iter()
            .zip(values)
            .map(|(data_type, values)| Vector::flat(data_type.clone(), values))
            .collect::<Result<_>>()?;
        Self::new(columns, count)
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
        let mut result = Self::new(
            self.columns
                .iter()
                .map(|column| column.slice(offset, count))
                .collect::<Result<_>>()?,
            count,
        )?;
        result.reservation = self.reservation.clone();
        Ok(result)
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
        let mut result = Self::new(columns, self.count)?;
        result.reservation = self.reservation.clone();
        Ok(result)
    }
    pub fn select(&self, selection: &[usize]) -> Result<Self> {
        if selection.iter().any(|&index| index >= self.count) {
            return Err(Error::Internal("chunk selection out of bounds".into()));
        }
        if selection.iter().copied().eq(0..self.count) {
            return Ok(self.clone());
        }
        let ordered = selection.windows(2).all(|pair| pair[0] <= pair[1]);
        let selection = Arc::new(selection.to_vec());
        let columns = self
            .columns
            .iter()
            .map(|column| Arc::new(column.clone()).selected(selection.clone(), ordered))
            .collect();
        let mut result = Self::new(columns, selection.len())?;
        result.reservation = self.reservation.clone();
        Ok(result)
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

#[cfg(test)]
mod data_chunk_tests {
    use super::*;
    use crate::common::{NestedPayload, NestedType, NestedValue};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn retained_chunk_and_column_views_keep_reservations_until_last_owner() -> Result<()> {
        use crate::parallel::{MemoryPool, QueryContext};
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let charge = pool.reserve(128, &query)?;
        let original = DataChunk::new(
            vec![Vector::flat(
                DataType::Varchar,
                vec![Value::Varchar("one".into()), Value::Varchar("two".into())],
            )?],
            2,
        )?
        .with_reservation(charge);
        let sliced = original.slice(1, 1)?;
        let projected = original.project(&[0, 0])?;
        let selected = original.select(&[1, 0, 1])?;
        let empty_projection = original.project(&[])?;
        let column = Arc::new(selected.columns()[0].clone());
        let column_view = column.select(vec![2, 0])?.slice(0, 1)?;
        drop((
            original,
            sliced,
            projected,
            selected,
            empty_projection,
            column,
        ));
        assert_eq!(pool.used()?, 128);
        assert!(matches!(
            pool.publish_limit(Some(127)),
            Err(Error::Resource(_))
        ));
        assert_eq!(column_view.value(0), Some(Value::Varchar("two".into())));
        drop(column_view);
        assert_eq!(pool.used()?, 0);
        pool.publish_limit(Some(0))?;
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn chunked_segment_slice_retains_parent_reservation() -> Result<()> {
        use crate::parallel::{MemoryPool, QueryContext};
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let mut combined = Vector::chunked(
            DataType::Varchar,
            vec![
                Vector::flat(DataType::Varchar, vec![Value::Varchar("a".into())])?,
                Vector::flat(DataType::Varchar, vec![Value::Varchar("b".into())])?,
            ],
        )?;
        combined.retain_reservation(pool.reserve(32, &query)?);
        let slice = combined.slice(1, 1)?;
        drop(combined);
        assert_eq!(pool.used()?, 32);
        assert_eq!(slice.value(0), Some(Value::Varchar("b".into())));
        drop(slice);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn owned_rows_preserve_cardinality_width_types_and_heap_ownership() -> Result<()> {
        let empty = DataChunk::from_owned_rows(&[DataType::Varchar], Vec::new())?;
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.columns().len(), 1);

        let zero_width = DataChunk::from_owned_rows(&[], vec![vec![], vec![]])?;
        assert_eq!(zero_width.len(), 2);
        assert!(zero_width.columns().is_empty());
        assert!(DataChunk::from_owned_rows(&[DataType::Varchar], vec![vec![]]).is_err());
        assert!(
            DataChunk::from_owned_rows(&[DataType::Varchar], vec![vec![Value::Integer(1)]],)
                .is_err()
        );

        let data_type = NestedType::List(DataType::Varchar).data_type();
        let nested = NestedValue::value(
            data_type.clone(),
            NestedPayload::Sequence(vec![Value::Varchar("child".into()), Value::Null]),
        )?;
        let text = String::from("moved-é\0text");
        let text_pointer = text.as_ptr();
        let chunk = DataChunk::from_owned_rows(
            &[DataType::Varchar, DataType::Varchar, data_type],
            vec![vec![Value::Varchar(text), Value::Null, nested.clone()]],
        )?;
        let values = chunk.columns()[0]
            .flat_values()
            .expect("owned VARCHAR flat values");
        let Value::Varchar(text) = &values[0] else {
            panic!("owned VARCHAR payload");
        };
        assert_eq!(text.as_ptr(), text_pointer);
        assert_eq!(chunk.columns()[1].value(0), Some(Value::Null));
        assert_eq!(chunk.columns()[2].value(0), Some(nested));
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
