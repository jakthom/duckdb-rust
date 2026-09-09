use super::*;
use crate::storage::recovery::{RecoveredChange, RecoveryTarget};

impl Snapshot {
    /// Restore physical identities without reusing deleted slots. The table
    /// must already exist and be empty. Failure leaves it unchanged.
    pub(crate) fn restore_rows(
        &mut self,
        name: &TableName,
        rows: Vec<(RowId, Row)>,
        next_id: RowId,
        context: &QueryContext,
    ) -> Result<()> {
        context.check_rows(rows.len())?;
        let mut table = self.get(name)?.clone();
        if table.next_id != 0 || !table.rows.is_empty() {
            return Err(Error::Corrupt("restoring a nonempty table".into()));
        }
        for (id, row) in rows {
            context.check()?;
            if id >= next_id || table.rows.insert(id, row).is_some() {
                return Err(Error::Corrupt("invalid restored row identity".into()));
            }
        }
        table.next_id = next_id;
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

impl RecoveryTarget for Snapshot {
    fn apply_committed(
        &mut self,
        changes: &[RecoveredChange],
        context: &QueryContext,
    ) -> Result<()> {
        let context = &context.clone().with_types(self.types.clone());
        let mut next = self.clone();
        let mut validity = BTreeMap::new();
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
                    }
                }
                RecoveredChange::Delete { table, ids } => {
                    let table = next.recovery_table(table)?;
                    for id in ids {
                        context.check()?;
                        if *id >= table.next_id {
                            return Err(Error::Corrupt("WAL delete row ID out of bounds".into()));
                        }
                        table.rows.remove(id);
                    }
                }
                RecoveredChange::Update {
                    table,
                    column,
                    values,
                } => {
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
                    let target = next.get(table)?;
                    for (id, valid) in values {
                        context.check()?;
                        if target
                            .rows
                            .get(id)
                            .and_then(|row| row.get(*column))
                            .is_none()
                        {
                            return Err(Error::Corrupt(
                                "WAL validity row or column out of bounds".into(),
                            ));
                        }
                        validity.insert((table.clone(), *column, *id), *valid);
                    }
                }
            }
        }
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
        // Rebuild derived state only after the entire durable transaction.
        next = next.with_indexes(self.indexes.clone(), context)?;
        *self = next;
        Ok(())
    }
}
