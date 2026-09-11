use super::*;
use crate::storage::recovery::{RecoveredChange, RecoveryTarget};
mod nested;

/// A native checkpoint slot retains its physical identity even when its row is
/// deleted. Decoders must not discard that provenance before restoration.
#[derive(Debug)]
pub(crate) enum RestoredSlot {
    Live(RowId, Row),
    Deleted(RowId),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    /// Restore physical identities without reusing deleted slots. The table
    /// must already exist and be empty. Failure leaves it unchanged.
    pub(crate) fn restore_slots(
        &mut self,
        name: &TableName,
        slots: Vec<RestoredSlot>,
        next_id: RowId,
        context: &QueryContext,
    ) -> Result<()> {
        context.check_rows(slots.len())?;
        let mut table = self.get(name)?.clone();
        if table.next_id != 0 || !table.rows.is_empty() {
            return Err(Error::Corrupt("restoring a nonempty table".into()));
        }
        let mut physical = std::collections::BTreeSet::new();
        for slot in slots {
            context.check()?;
            let (id, row) = match slot {
                RestoredSlot::Live(id, row) => (id, Some(row)),
                RestoredSlot::Deleted(id) => (id, None),
            };
            if id >= next_id || !physical.insert(id) || row.as_ref().is_some_and(|row| table.rows.insert(id, row.clone()).is_some()) {
                return Err(Error::Corrupt("invalid restored row identity".into()));
            }
        }
        table.next_id = next_id;
        table.physical_slots = (0..next_id)
            .map(|id| table.rows.contains_key(&id).then_some(id))
            .collect();
        table.validate(
            self.indexes.as_ref(),
            &context.clone().with_types(self.types.clone()),
        )?;
        self.tables.insert(name.key(), Arc::new(table));
        Ok(())
    }

    fn recovery_table(&mut self, name: &TableName) -> Result<&mut TableData> {
        self.tables
            .get_mut(&name.key())
            .map(Arc::make_mut)
            .ok_or_else(|| Error::Corrupt(format!("WAL references missing table {name}")))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RecoveryTarget for Snapshot {
    fn apply_committed(
        &mut self,
        changes: &[RecoveredChange],
        context: &QueryContext,
    ) -> Result<()> {
        let context = &context.clone().with_types(self.types.clone());
        let mut next = self.clone();
        let mut validity = BTreeMap::new();
        let mut physical = nested::Pending::new();
        for change in changes {
            context.check()?;
            match change {
                RecoveredChange::CreateSchema(name) => next.create_schema(name, false)?,
                RecoveredChange::DropSchema(name) => next.drop_schema(name, false)?,
                RecoveredChange::CreateTable(definition) => {
                    next.create_table(definition.clone(), false)?
                }
                RecoveredChange::DropTable(name) => {
                    next.drop_table(name, false)?;
                    validity.retain(|(table, _, _), _| table != name);
                    physical.discard_table(name);
                }
                RecoveredChange::AlterTable { table, alteration } => {
                    physical.finish(&mut next, context)?;
                    apply_validity(&mut next, std::mem::take(&mut validity), context)?;
                    next.alter_table(table, alteration, context)?;
                }
                RecoveredChange::Insert { table, rows } => {
                    let table = next.recovery_table(table)?;
                    context.check_rows(table.rows.len().saturating_add(rows.len()))?;
                    for row in rows {
                        context.check()?;
                        let id = table.next_id;
                        table.next_id = id
                            .checked_add(1)
                            .ok_or_else(|| Error::Resource("row identity exhausted".into()))?;
                        table.rows.insert(id, row.clone());
                        table.physical_slots.push(Some(id));
                    }
                }
                RecoveredChange::Delete { table, ids } => {
                    let table = next.recovery_table(table)?;
                    for id in ids {
                        context.check()?;
                        if *id >= table.next_id {
                            return Err(Error::Corrupt("WAL delete row ID out of bounds".into()));
                        }
                        if table.rows.remove(id).is_some() {
                            let slot = table
                                .physical_slots
                                .iter_mut()
                                .find(|slot| **slot == Some(*id))
                                .ok_or_else(|| {
                                    Error::Corrupt("live row has no physical slot".into())
                                })?;
                            *slot = None;
                        }
                    }
                }
                RecoveredChange::Update {
                    table,
                    column,
                    values,
                } => {
                    physical.finish(&mut next, context)?;
                    let table = next.recovery_table(table)?;
                    for (id, value) in values {
                        context.check()?;
                        let target = table
                            .rows
                            .get_mut(id)
                            .and_then(|row| row.get_mut(*column))
                            .ok_or_else(|| {
                                Error::Corrupt("WAL update row or column out of bounds".into())
                            })?;
                        *target = value.clone();
                    }
                }
                RecoveredChange::Validity {
                    table,
                    column,
                    values,
                } => {
                    physical.finish(&mut next, context)?;
                    let target = next.get(table)?;
                    for (id, valid) in values {
                        context.check()?;
                        if target
                            .rows
                            .get(id)
                            .is_none_or(|row| row.get(*column).is_none())
                        {
                            return Err(Error::Corrupt(
                                "WAL validity row or column out of bounds".into(),
                            ));
                        }
                        validity.insert((table.clone(), *column, *id), *valid);
                    }
                }
                RecoveredChange::NestedUpdate {
                    table,
                    column,
                    path,
                    values,
                } => {
                    physical.update(&next, table, *column, path, values, context)?;
                }
                RecoveredChange::NestedValidity {
                    table,
                    column,
                    path,
                    values,
                } => {
                    physical.validity(&next, table, *column, path, values, context)?;
                }
            }
        }
        physical.finish(&mut next, context)?;
        apply_validity(&mut next, validity, context)?;
        // Rebuild derived state only after the entire durable transaction.
        next = next.with_indexes(self.indexes.clone(), context)?;
        *self = next;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn apply_validity(
    next: &mut Snapshot,
    validity: BTreeMap<(TableName, usize, RowId), bool>,
    context: &QueryContext,
) -> Result<()> {
    for ((table, column, id), valid) in validity {
        context.check()?;
        if let Some(row) = next.recovery_table(&table)?.rows.get_mut(&id) {
            if !valid {
                row[column] = crate::common::Value::Null;
            } else if row[column].is_null() {
                return Err(Error::Corrupt(
                    "WAL makes a NULL slot valid without a value".into(),
                ));
            }
        }
    }
    Ok(())
}
