use std::{ops::Index, sync::Arc};

use super::{
    Error, Result, Row, Value,
    vector::{
        DataChunk, OwnedFlatValuesHandoff, materialized_value_bytes, try_clone_materialized_value,
    },
};
use crate::parallel::{MemoryPool, QueryContext, Reservation};

/// Materialized rows share immutable backing when cloned. Mutations are fallible
/// and copy shared backing only after admitting an independent reservation.
#[derive(Clone)]
pub struct RowCollection {
    width: usize,
    count: usize,
    backing: Arc<Backing>,
}
struct Backing {
    values: Vec<Value>,
    charges: Vec<Reservation>,
    pool: Option<Arc<MemoryPool>>,
    payload_bytes: usize,
    slot_capacity: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn allocation_error() -> Error {
    Error::Resource("cannot allocate materialized rows".into())
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn add_bytes(left: usize, right: usize) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| Error::Resource("materialized size overflow".into()))
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn slot_bytes(count: usize) -> Result<usize> {
    count
        .checked_mul(std::mem::size_of::<Value>())
        .ok_or_else(allocation_error)
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn owned_payload_capacity_bytes(values: &[Value]) -> Result<usize> {
    values.iter().try_fold(0usize, |bytes, value| {
        add_bytes(
            bytes,
            match value {
                Value::Varchar(value) => value.capacity(),
                Value::Blob(value) => value.capacity(),
                _ => 0,
            },
        )
    })
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn grown_slot_capacity(current: usize, required: usize) -> Result<usize> {
    if required <= current {
        return Ok(current);
    }
    let doubled = current.checked_mul(2).unwrap_or(required);
    Ok(required.max(doubled))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RowCollection {
    pub fn new(width: usize) -> Self {
        Self {
            width,
            count: 0,
            backing: Arc::new(Backing {
                values: Vec::new(),
                charges: Vec::new(),
                pool: None,
                payload_bytes: 0,
                slot_capacity: 0,
            }),
        }
    }
    pub fn from_rows(width: usize, rows: Vec<Row>) -> Result<Self> {
        if rows.iter().any(|row| row.len() != width) {
            return Err(Error::Internal(
                "materialized row width differs from schema".into(),
            ));
        }
        let mut output = Self::new(width);
        output.reserve_uncharged(rows.len())?;
        output.count = rows.len();
        Arc::get_mut(&mut output.backing)
            .expect("unique new backing")
            .values
            .extend(rows.into_iter().flatten());
        Ok(output)
    }
    fn value_count(&self, count: usize) -> Result<usize> {
        self.count.checked_add(count).ok_or_else(allocation_error)?;
        self.width.checked_mul(count).ok_or_else(allocation_error)
    }
    fn reserve_uncharged(&mut self, count: usize) -> Result<()> {
        let values = self.value_count(count)?;
        Arc::get_mut(&mut self.backing)
            .expect("unique uncharged backing")
            .values
            .try_reserve(values)
            .map_err(|_| allocation_error())
    }
    pub fn append(&mut self, chunk: &DataChunk) -> Result<()> {
        self.append_impl(chunk, None)
    }
    pub(crate) fn append_with_context(
        &mut self,
        chunk: &DataChunk,
        query: &QueryContext,
    ) -> Result<()> {
        self.append_impl(chunk, Some(query))
    }
    /// Consume the ordinary single-flat publication path without cloning its
    /// VARCHAR/BLOB payloads. Unsupported encodings retain the borrowed path.
    pub(crate) fn append_owned_with_context(
        &mut self,
        chunk: DataChunk,
        query: &QueryContext,
    ) -> Result<()> {
        if chunk.columns().len() != self.width {
            return Err(Error::Internal(
                "materialized chunk width differs from schema".into(),
            ));
        }
        if self.width != 1 || Arc::strong_count(&self.backing) != 1 {
            return self.append_with_context(&chunk, query);
        }
        let compatible_destination = self
            .backing
            .pool
            .as_ref()
            .is_none_or(|pool| Arc::ptr_eq(pool, query.memory_pool()));
        if !compatible_destination {
            return self.append_with_context(&chunk, query);
        }
        let incoming = match chunk.into_owned_single_flat_values() {
            OwnedFlatValuesHandoff::Owned(incoming) => incoming,
            OwnedFlatValuesHandoff::Unsupported(chunk) => {
                return self.append_with_context(&chunk, query);
            }
        };
        let count = incoming.values.len();
        self.value_count(count)?;
        let incoming_capacity = incoming.values.capacity();
        let incoming_payload = owned_payload_capacity_bytes(&incoming.values)?;
        let pool = query.memory_pool().clone();
        let previous_charged = self.backing.pool.is_some();
        let previous_payload = if previous_charged {
            self.backing.payload_bytes
        } else {
            owned_payload_capacity_bytes(&self.backing.values)?
        };
        let total_payload = add_bytes(previous_payload, incoming_payload)?;

        if self.backing.values.is_empty() {
            let admit = add_bytes(incoming_payload, slot_bytes(incoming_capacity)?)?;
            let reservation = pool.reserve(admit, query)?;
            let mut charges = Vec::new();
            charges
                .try_reserve_exact(1)
                .map_err(|_| allocation_error())?;
            charges.push(reservation);
            let backing = Arc::get_mut(&mut self.backing).expect("unique checked backing");
            // All fallible work is complete. Replacing the charge vector now
            // releases any accounted spare allocation retained by a prior
            // zero-row result together with its discarded values buffer.
            backing.values = incoming.values;
            backing.charges = charges;
            backing.pool = Some(pool);
            backing.payload_bytes = incoming_payload;
            backing.slot_capacity = incoming_capacity;
            self.count += count;
            drop(incoming.reservation);
            return Ok(());
        }

        let required_slots = add_bytes(self.backing.values.len(), count)?;
        let grows = self.backing.values.capacity() < required_slots;
        let planned_slots = if grows {
            grown_slot_capacity(
                self.backing
                    .values
                    .capacity()
                    .max(self.backing.values.len()),
                required_slots,
            )?
        } else {
            self.backing.values.capacity()
        };
        let covered_slots = if previous_charged {
            self.backing.slot_capacity
        } else {
            0
        };
        let planned_added_slots = planned_slots.saturating_sub(covered_slots);
        let planned_admit = add_bytes(
            add_bytes(
                if previous_charged {
                    0
                } else {
                    previous_payload
                },
                incoming_payload,
            )?,
            slot_bytes(planned_added_slots)?,
        )?;
        let planned_reservation = if planned_admit == 0 {
            None
        } else {
            Some(pool.reserve(planned_admit, query)?)
        };
        // The retained charge plus the persistent final-capacity delta now
        // covers the planned new allocation. Admit the old allocation only
        // for the interval in which staged growth keeps both buffers alive.
        let old_temporary = grows
            .then(|| pool.reserve(slot_bytes(self.backing.values.capacity())?, query))
            .transpose()?;
        // Stage growth separately so allocator rounding is known and admitted
        // before the destination's values or bookkeeping change.
        let mut staged = grows.then(Vec::new);
        if let Some(staged) = &mut staged {
            staged
                .try_reserve_exact(planned_slots)
                .map_err(|_| allocation_error())?;
        }
        let target_slots = staged
            .as_ref()
            .map_or(self.backing.values.capacity(), Vec::capacity);
        let rounded_reservation = if target_slots > planned_slots {
            Some(pool.reserve(slot_bytes(target_slots - planned_slots)?, query)?)
        } else {
            None
        };
        let retained_reservations =
            usize::from(planned_reservation.is_some()) + usize::from(rounded_reservation.is_some());
        let backing = Arc::get_mut(&mut self.backing).expect("unique checked backing");
        backing
            .charges
            .try_reserve(retained_reservations)
            .map_err(|_| allocation_error())?;
        if let Some(mut staged) = staged {
            staged.append(&mut backing.values);
            staged.extend(incoming.values);
            backing.values = staged;
        } else {
            backing.values.extend(incoming.values);
        }
        backing.charges.extend(planned_reservation);
        backing.charges.extend(rounded_reservation);
        backing.pool = Some(pool);
        backing.payload_bytes = total_payload;
        backing.slot_capacity = target_slots;
        self.count += count;
        drop(old_temporary);
        drop(incoming.reservation);
        Ok(())
    }
    fn append_impl(&mut self, chunk: &DataChunk, query: Option<&QueryContext>) -> Result<()> {
        if chunk.columns().len() != self.width {
            return Err(Error::Internal(
                "materialized chunk width differs from schema".into(),
            ));
        }
        let pool = self
            .backing
            .pool
            .clone()
            .or_else(|| chunk.reservation_pool().cloned());
        if pool.is_none() && Arc::strong_count(&self.backing) == 1 {
            self.reserve_uncharged(chunk.len())?;
            let values = &mut Arc::get_mut(&mut self.backing)
                .expect("unique backing")
                .values;
            if let [column] = chunk.columns() {
                column.append_to(values);
            } else {
                for index in 0..chunk.len() {
                    values.extend(
                        chunk
                            .columns()
                            .iter()
                            .map(|column| column.get(index).expect("validated chunk cardinality")),
                    );
                }
            }
            self.count += chunk.len();
            return Ok(());
        }
        let bytes = chunk.columns().iter().try_fold(0, |bytes, column| {
            add_bytes(bytes, column.materialized_bytes()?)
        })?;
        self.extend_materialized(chunk.len(), bytes, pool, query, |values| {
            for index in 0..chunk.len() {
                for column in chunk.columns() {
                    values.push(column.try_materialized_value(index)?);
                }
            }
            Ok(())
        })
    }
    /// Append a borrowed row. Failed admission or allocation leaves all shared
    /// owners and the destination's values unchanged.
    pub fn push(&mut self, row: &[Value]) -> Result<()> {
        if row.len() != self.width {
            return Err(Error::Internal(
                "materialized row width differs from schema".into(),
            ));
        }
        if self.backing.pool.is_none() && Arc::strong_count(&self.backing) == 1 {
            self.reserve_uncharged(1)?;
            Arc::get_mut(&mut self.backing)
                .expect("unique backing")
                .values
                .extend_from_slice(row);
            self.count += 1;
            return Ok(());
        }
        let bytes = row.iter().try_fold(0, |bytes, value| {
            add_bytes(bytes, materialized_value_bytes(value)?)
        })?;
        self.extend_materialized(1, bytes, self.backing.pool.clone(), None, |values| {
            for value in row {
                values.push(try_clone_materialized_value(value)?);
            }
            Ok(())
        })
    }
    fn extend_materialized(
        &mut self,
        count: usize,
        incoming_bytes: usize,
        pool: Option<Arc<MemoryPool>>,
        query: Option<&QueryContext>,
        fill: impl FnOnce(&mut Vec<Value>) -> Result<()>,
    ) -> Result<()> {
        let incoming_values = self.value_count(count)?;
        let background;
        let query = match query {
            Some(query) => query,
            None => {
                background = QueryContext::background();
                &background
            }
        };
        let shared = Arc::strong_count(&self.backing) != 1;
        let incoming_slots = slot_bytes(incoming_values)?;
        let incoming_payload = incoming_bytes
            .checked_sub(incoming_slots)
            .ok_or_else(allocation_error)?;
        let previous_payload = if self.backing.pool.is_some() {
            self.backing.payload_bytes
        } else {
            let bytes = self.backing.values.iter().try_fold(0, |bytes, value| {
                add_bytes(bytes, materialized_value_bytes(value)?)
            })?;
            bytes
                .checked_sub(slot_bytes(self.backing.values.len())?)
                .ok_or_else(allocation_error)?
        };
        let total_payload = add_bytes(previous_payload, incoming_payload)?;
        let required_slots = add_bytes(self.backing.values.len(), incoming_values)?;
        // A shared copy needs its entire independent payload. The first charged
        // append also adopts existing uncharged values and retained slot budget.
        let target_slots = if shared {
            grown_slot_capacity(self.backing.values.len(), required_slots)?
        } else if self.backing.values.capacity() < required_slots {
            grown_slot_capacity(
                self.backing.slot_capacity.max(self.backing.values.len()),
                required_slots,
            )?
        } else {
            self.backing.slot_capacity.max(required_slots)
        };
        let added_slots = if shared || self.backing.pool.is_none() {
            target_slots
        } else {
            target_slots
                .checked_sub(self.backing.slot_capacity)
                .ok_or_else(allocation_error)?
        };
        let admit = add_bytes(
            if shared || self.backing.pool.is_none() {
                total_payload
            } else {
                incoming_payload
            },
            slot_bytes(added_slots)?,
        )?;
        let reservation = pool
            .as_ref()
            .map(|pool| pool.reserve(admit, query))
            .transpose()?;
        if shared {
            let mut values = Vec::new();
            values
                .try_reserve_exact(target_slots)
                .map_err(|_| allocation_error())?;
            for value in &self.backing.values {
                values.push(try_clone_materialized_value(value)?);
            }
            fill(&mut values)?;
            self.backing = Arc::new(Backing {
                values,
                charges: reservation.into_iter().collect(),
                pool,
                payload_bytes: total_payload,
                slot_capacity: target_slots,
            });
        } else {
            // Stage before touching the destination. During reallocation both
            // its old capacity and the new allocation can coexist; payloads move.
            let grows = self.backing.values.capacity() < required_slots;
            let old_slots = if grows {
                slot_bytes(self.backing.values.capacity())?
            } else {
                0
            };
            let temporary_bytes = add_bytes(incoming_slots, old_slots)?;
            let _temporary = pool
                .as_ref()
                .map(|pool| pool.reserve(temporary_bytes, query))
                .transpose()?;
            let mut incoming = Vec::new();
            incoming
                .try_reserve_exact(incoming_values)
                .map_err(|_| allocation_error())?;
            fill(&mut incoming)?;
            let backing = Arc::get_mut(&mut self.backing).expect("unique backing");
            backing
                .charges
                .try_reserve(usize::from(reservation.is_some()))
                .map_err(|_| allocation_error())?;
            let reserve_slots = if grows {
                target_slots
                    .checked_sub(backing.values.len())
                    .ok_or_else(allocation_error)?
            } else {
                0
            };
            backing
                .values
                .try_reserve_exact(reserve_slots)
                .map_err(|_| allocation_error())?;
            backing.values.extend(incoming);
            backing.charges.extend(reservation);
            backing.pool = pool;
            backing.payload_bytes = total_payload;
            backing.slot_capacity = target_slots;
        }
        self.count += count;
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
    pub(crate) fn memory_pool(&self) -> Option<&Arc<MemoryPool>> {
        self.backing.pool.as_ref()
    }
    #[inline]
    pub fn get(&self, index: usize) -> Option<&[Value]> {
        (index < self.count)
            .then(|| &self.backing.values[index * self.width..(index + 1) * self.width])
    }
    pub fn iter(&self) -> Rows<'_> {
        Rows {
            collection: self,
            positions: 0..self.count,
        }
    }
    /// Explicit application-owned export. Returned Vec allocations and any
    /// clone needed for another shared owner leave engine result accounting.
    /// Engine storage handoffs must use `with_owned_rows` instead.
    pub fn into_rows(self) -> Vec<Row> {
        self.into_iter().collect()
    }

    /// Retain execution ownership through the complete storage insertion. A
    /// successful callback transfers payloads to the existing unbudgeted source
    /// storage domain; errors release all temporary and result reservations.
    pub(crate) fn with_owned_rows<T>(
        self,
        query: &QueryContext,
        consume: impl FnOnce(Vec<Row>) -> Result<T>,
    ) -> Result<T> {
        let pool = self.backing.pool.clone();
        let shared = Arc::strong_count(&self.backing) != 1;
        let row_slots = self
            .count
            .checked_mul(std::mem::size_of::<Row>())
            .ok_or_else(allocation_error)?;
        let transfer_bytes = add_bytes(row_slots, slot_bytes(self.backing.values.len())?)?;
        let copy_bytes = if shared {
            add_bytes(
                self.backing.payload_bytes,
                slot_bytes(self.backing.values.len())?,
            )?
        } else {
            0
        };
        let _transfer = pool
            .as_ref()
            .map(|pool| pool.reserve(add_bytes(transfer_bytes, copy_bytes)?, query))
            .transpose()?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(self.count)
            .map_err(|_| allocation_error())?;
        let (values, retained) = match Arc::try_unwrap(self.backing) {
            Ok(backing) => (backing.values, backing.charges),
            Err(backing) => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(backing.values.len())
                    .map_err(|_| allocation_error())?;
                for value in &backing.values {
                    values.push(try_clone_materialized_value(value)?);
                }
                (values, Vec::new())
            }
        };
        let mut values = values.into_iter();
        for _ in 0..self.count {
            let mut row = Vec::new();
            row.try_reserve_exact(self.width)
                .map_err(|_| allocation_error())?;
            row.extend(values.by_ref().take(self.width));
            rows.push(row);
        }
        let result = consume(rows);
        drop(retained);
        result
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DoubleEndedIterator for Rows<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.positions
            .next_back()
            .and_then(|index| self.collection.get(index))
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExactSizeIterator for Rows<'_> {}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::iter::FusedIterator for Rows<'_> {}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExactSizeIterator for IntoRows {}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::iter::FusedIterator for IntoRows {}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IntoIterator for RowCollection {
    type Item = Row;
    type IntoIter = IntoRows;
    /// Like `into_rows`, consuming iteration is an explicit application-owned
    /// export, including values still held by the returned iterator.
    fn into_iter(self) -> IntoRows {
        IntoRows {
            width: self.width,
            remaining: self.count,
            values: match Arc::try_unwrap(self.backing) {
                Ok(backing) => backing.values,
                Err(backing) => backing.values.clone(),
            }
            .into_iter(),
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for RowCollection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self).finish()
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PartialEq for RowCollection {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other)
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PartialEq<Vec<Row>> for RowCollection {
    fn eq(&self, other: &Vec<Row>) -> bool {
        self.iter().eq(other.iter().map(Vec::as_slice))
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PartialEq<RowCollection> for Vec<Row> {
    fn eq(&self, other: &RowCollection) -> bool {
        other == self
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl serde::Serialize for RowCollection {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::common::{DataType, vector::Vector};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn charged_strings(pool: &Arc<MemoryPool>, query: &QueryContext) -> Result<DataChunk> {
        Ok(DataChunk::new(
            vec![Vector::flat(
                DataType::Varchar,
                vec![Value::Varchar("payload".into())],
            )?],
            1,
        )?
        .with_reservation(pool.reserve(7, query)?))
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn charged_clone_shares_payload_and_cow_is_independently_admitted() -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let chunk = charged_strings(&pool, &query)?;
        let mut original = RowCollection::new(1);
        original.append(&chunk)?;
        drop(chunk);
        let bytes = materialized_value_bytes(&Value::Varchar("payload".into()))?;
        assert_eq!(pool.used()?, bytes);
        let mut copy = original.clone();
        assert!(std::ptr::eq(original[0].as_ptr(), copy[0].as_ptr()));
        assert_eq!(pool.used()?, bytes);
        pool.publish_limit(Some(bytes))?;
        assert!(matches!(
            copy.push(&[Value::Varchar("another".into())]),
            Err(Error::Resource(_))
        ));
        assert_eq!(copy, original);
        assert!(std::ptr::eq(original[0].as_ptr(), copy[0].as_ptr()));
        assert_eq!(pool.used()?, bytes);
        pool.publish_limit(None)?;
        copy.push(&[Value::Varchar("another".into())])?;
        assert_eq!(copy.len(), 2);
        assert_eq!(original.len(), 1);
        assert_eq!(pool.used()?, bytes * 3);
        drop(copy);
        assert_eq!(pool.used()?, bytes);
        original.push(&[Value::Varchar("another".into())])?;
        assert_eq!(pool.used()?, bytes * 2);
        drop(original);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn collection_copy_peak_rejection_preserves_source_and_releases_admission() -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let chunk = charged_strings(&pool, &query)?;
        let bytes = materialized_value_bytes(&Value::Varchar("payload".into()))?;
        pool.publish_limit(Some(7 + bytes))?;
        let mut rows = RowCollection::new(1);
        assert!(matches!(
            rows.append_with_context(&chunk, &query),
            Err(Error::Resource(_))
        ));
        assert!(rows.is_empty());
        assert_eq!(
            chunk.columns()[0].value(0),
            Some(Value::Varchar("payload".into()))
        );
        assert_eq!(pool.used()?, 7);
        pool.publish_limit(None)?;
        rows.append_with_context(&chunk, &query)?;
        let retained = pool.used()?;
        pool.publish_limit(Some(retained + bytes))?;
        assert!(matches!(
            rows.append_with_context(&chunk, &query),
            Err(Error::Resource(_))
        ));
        assert_eq!(rows.len(), 1);
        assert_eq!(pool.used()?, retained);
        drop(rows);
        drop(chunk);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn internal_transfer_retains_charge_until_callback_returns_or_errors() -> Result<()> {
        for shared in [false, true] {
            let pool = Arc::new(MemoryPool::default());
            let query = QueryContext::background().with_memory_pool(pool.clone());
            let chunk = charged_strings(&pool, &query)?;
            let mut rows = RowCollection::new(1);
            rows.append(&chunk)?;
            drop(chunk);
            let owner = shared.then(|| rows.clone());
            let retained = pool.used()?;
            let result: Result<()> = rows.with_owned_rows(&query, |rows| {
                assert_eq!(rows, vec![vec![Value::Varchar("payload".into())]]);
                assert!(pool.used()? > retained);
                assert!(matches!(
                    pool.publish_limit(Some(retained)),
                    Err(Error::Resource(_))
                ));
                Err(Error::Internal("insertion failed".into()))
            });
            assert!(
                matches!(result, Err(Error::Internal(message)) if message == "insertion failed")
            );
            assert_eq!(pool.used()?, if shared { retained } else { 0 });
            drop(owner);
            assert_eq!(pool.used()?, 0);
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn internal_transfer_admission_failure_does_not_call_storage() -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let chunk = charged_strings(&pool, &query)?;
        let mut rows = RowCollection::new(1);
        rows.append(&chunk)?;
        drop(chunk);
        pool.publish_limit(Some(pool.used()?))?;
        let result: Result<()> = rows.with_owned_rows(&query, |_| panic!("storage must not run"));
        assert!(matches!(result, Err(Error::Resource(_))));
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn charged_arena_dictionary_copy_counts_payload_and_shared_append_is_atomic() -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let parent =
            Vector::packed_utf8(Arc::new("éXYZ".into()), vec![Some(0..2), None, Some(2..5)])?;
        let selected = Arc::new(parent.slice(1, 2)?).select(vec![1, 0, 1])?;
        let chunk = DataChunk::new(vec![selected], 3)?.with_reservation(pool.reserve(7, &query)?);
        let mut rows = RowCollection::new(1);
        rows.append(&chunk)?;
        let bytes = 3 * std::mem::size_of::<Value>() + 6;
        assert_eq!(pool.used()?, bytes + 7);
        assert_eq!(
            rows,
            vec![
                vec![Value::Varchar("XYZ".into())],
                vec![Value::Null],
                vec![Value::Varchar("XYZ".into())]
            ]
        );
        let mut clone = rows.clone();
        pool.publish_limit(Some(pool.used()?))?;
        assert!(matches!(clone.append(&chunk), Err(Error::Resource(_))));
        assert_eq!(clone, rows);
        assert_eq!(pool.used()?, bytes + 7);
        pool.publish_limit(None)?;
        clone.append(&chunk)?;
        assert_eq!(clone.len(), 6);
        assert_eq!(pool.used()?, bytes * 3 + 7);
        drop(rows);
        drop(clone);
        drop(chunk);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn charged_many_batch_growth_retains_admitted_spare_capacity_and_releases_last_owner()
    -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let chunk = charged_strings(&pool, &query)?;
        let mut rows = RowCollection::new(1);
        for _ in 0..9 {
            rows.append_with_context(&chunk, &query)?;
        }
        assert_eq!(rows.backing.slot_capacity, 16);
        assert!(rows.backing.values.capacity() >= rows.backing.slot_capacity);
        let expected = 7 + 9 * 7 + slot_bytes(rows.backing.slot_capacity)?;
        assert_eq!(pool.used()?, expected);
        drop(chunk);
        assert_eq!(pool.used()?, expected - 7);
        let retained = rows.clone();
        drop(rows);
        assert_eq!(pool.used()?, expected - 7);
        drop(retained);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn owned_flat_append_moves_payload_and_keeps_failed_growth_atomic() -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let payload = String::from("owned-é\0");
        let pointer = payload.as_ptr();
        let bytes = materialized_value_bytes(&Value::Varchar(payload.clone()))?;
        let first = DataChunk::new(
            vec![Vector::flat(
                DataType::Varchar,
                vec![Value::Varchar(payload)],
            )?],
            1,
        )?
        .with_reservation(pool.reserve(bytes, &query)?);
        let mut rows = RowCollection::new(1);
        rows.append_owned_with_context(first, &query)?;
        let Value::Varchar(retained) = &rows[0][0] else {
            panic!("owned VARCHAR result")
        };
        assert_eq!(retained.as_ptr(), pointer);
        assert_eq!(pool.used()?, bytes);

        let second = DataChunk::new(
            vec![Vector::flat(
                DataType::Varchar,
                vec![Value::Varchar("second".into())],
            )?],
            1,
        )?
        .with_reservation(pool.reserve(bytes, &query)?);
        let before = pool.used()?;
        pool.publish_limit(Some(before))?;
        assert!(matches!(
            rows.append_owned_with_context(second, &query),
            Err(Error::Resource(_))
        ));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], Value::Varchar("owned-é\0".into()));
        assert_eq!(pool.used()?, bytes);
        drop(rows);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn owned_flat_append_independently_admits_unrelated_guard_and_spare_capacity() -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let mut payload = String::with_capacity(32);
        payload.push('x');
        let pointer = payload.as_ptr();
        let mut values = Vec::with_capacity(8);
        values.push(Value::Varchar(payload));
        let chunk = DataChunk::new(vec![Vector::flat(DataType::Varchar, values)?], 1)?
            .with_reservation(pool.reserve(1, &query)?);
        let mut rows = RowCollection::new(1);
        rows.append_owned_with_context(chunk, &query)?;
        let expected = slot_bytes(8)? + 32;
        assert_eq!(pool.used()?, expected);
        assert_eq!(rows.backing.slot_capacity, 8);
        let Value::Varchar(retained) = &rows[0][0] else {
            panic!("owned VARCHAR result")
        };
        assert_eq!(retained.as_ptr(), pointer);
        assert_eq!(retained.capacity(), 32);

        let mut shared = rows.clone();
        pool.publish_limit(Some(expected))?;
        assert!(matches!(
            shared.push(&[Value::Varchar("blocked".into())]),
            Err(Error::Resource(_))
        ));
        assert_eq!(shared, rows);
        assert_eq!(pool.used()?, expected);
        drop((shared, rows));
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn owned_flat_append_replaces_charged_zero_row_spare_capacity() -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let empty_capacity = 8;
        let empty = DataChunk::new(
            vec![Vector::flat(
                DataType::Varchar,
                Vec::with_capacity(empty_capacity),
            )?],
            0,
        )?
        .with_reservation(pool.reserve(1, &query)?);
        let mut rows = RowCollection::new(1);
        rows.append_owned_with_context(empty, &query)?;
        assert_eq!(pool.used()?, slot_bytes(empty_capacity)?);
        assert_eq!(rows.backing.slot_capacity, empty_capacity);

        let mut payload = String::with_capacity(12);
        payload.push('x');
        let payload_capacity = payload.capacity();
        let pointer = payload.as_ptr();
        let mut values = Vec::with_capacity(1);
        values.push(Value::Varchar(payload));
        let value_capacity = values.capacity();
        let replacement = DataChunk::new(vec![Vector::flat(DataType::Varchar, values)?], 1)?
            .with_reservation(pool.reserve(1, &query)?);
        rows.append_owned_with_context(replacement, &query)?;

        let expected = add_bytes(slot_bytes(value_capacity)?, payload_capacity)?;
        assert_eq!(pool.used()?, expected);
        assert_eq!(rows.backing.slot_capacity, value_capacity);
        let Value::Varchar(retained) = &rows[0][0] else {
            panic!("owned VARCHAR result")
        };
        assert_eq!(retained.as_ptr(), pointer);
        drop(rows);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn owned_flat_growth_succeeds_at_exact_old_plus_new_slot_peak() -> Result<()> {
        let pool = Arc::new(MemoryPool::default());
        let query = QueryContext::background().with_memory_pool(pool.clone());
        let mut first_values = Vec::with_capacity(1);
        first_values.push(Value::Varchar(String::new()));
        let first = DataChunk::new(vec![Vector::flat(DataType::Varchar, first_values)?], 1)?
            .with_reservation(pool.reserve(1, &query)?);
        let mut rows = RowCollection::new(1);
        rows.append_owned_with_context(first, &query)?;
        let old_capacity = rows.backing.values.capacity();
        let covered_capacity = rows.backing.slot_capacity;

        let required = rows.backing.values.len() + 1;
        let planned = grown_slot_capacity(old_capacity, required)?;
        let mut allocation_probe = Vec::<Value>::new();
        allocation_probe
            .try_reserve_exact(planned)
            .map_err(|_| allocation_error())?;
        let target_capacity = allocation_probe.capacity();
        drop(allocation_probe);

        let mut second_values = Vec::with_capacity(1);
        second_values.push(Value::Varchar(String::new()));
        let second = DataChunk::new(vec![Vector::flat(DataType::Varchar, second_values)?], 1)?
            .with_reservation(pool.reserve(1, &query)?);
        let peak = add_bytes(
            pool.used()?,
            add_bytes(
                slot_bytes(target_capacity.saturating_sub(covered_capacity))?,
                slot_bytes(old_capacity)?,
            )?,
        )?;
        pool.publish_limit(Some(peak))?;
        rows.append_owned_with_context(second, &query)?;

        assert_eq!(
            rows,
            vec![
                vec![Value::Varchar(String::new())],
                vec![Value::Varchar(String::new())]
            ]
        );
        assert_eq!(rows.backing.slot_capacity, target_capacity);
        assert_eq!(pool.used()?, slot_bytes(target_capacity)?);
        drop(rows);
        assert_eq!(pool.used()?, 0);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn uncharged_clone_and_zero_width_rows_preserve_value_semantics() -> Result<()> {
        let mut empty_rows = RowCollection::from_rows(0, vec![vec![], vec![]])?;
        let clone = empty_rows.clone();
        empty_rows.push(&[])?;
        assert_eq!(clone.into_rows(), vec![vec![], vec![]]);
        assert_eq!(empty_rows.into_rows(), vec![vec![], vec![], vec![]]);
        let mut rows = RowCollection::from_rows(1, vec![vec![Value::Varchar("old".into())]])?;
        let clone = rows.clone();
        rows.push(&[Value::Varchar("new".into())])?;
        assert_eq!(clone.into_rows(), vec![vec![Value::Varchar("old".into())]]);
        assert_eq!(rows.len(), 2);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn append_preserves_dictionary_selected_bigint_parent_slice() -> Result<()> {
        let parent = Arc::new(
            Vector::flat(
                DataType::BigInt,
                vec![
                    Value::Integer(10),
                    Value::Integer(11),
                    Value::Integer(12),
                    Value::Integer(13),
                ],
            )?
            .slice(1, 3)?,
        );
        let dictionary = parent.select(vec![2, 0, 2])?;
        let chunk = DataChunk::new(vec![dictionary], 3)?;
        let mut rows = RowCollection::new(1);
        rows.append(&chunk)?;
        assert_eq!(
            rows.into_rows(),
            vec![
                vec![Value::Integer(13)],
                vec![Value::Integer(11)],
                vec![Value::Integer(13)],
            ]
        );
        Ok(())
    }
}
