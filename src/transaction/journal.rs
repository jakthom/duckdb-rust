use super::*;
use crate::catalog::TableDefinition;
use std::collections::{BTreeMap, BTreeSet};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Catalog for SnapshotTransaction {
    fn schemas(&self) -> Result<Vec<String>> {
        self.snapshot.schemas()
    }
    fn table(&self, name: &TableName) -> Result<TableDefinition> {
        self.snapshot.table(name)
    }
    fn tables(&self) -> Result<Vec<TableDefinition>> {
        self.snapshot.tables()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CatalogMut for SnapshotTransaction {
    fn alter_table(
        &mut self,
        name: &TableName,
        alteration: &crate::catalog::TableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        let mut basis = self.catalog_basis.clone();
        basis.alter_table(name, alteration, context)?;
        let changed = self.snapshot.alter_table(name, alteration, context)?;
        if changed {
            self.catalog_basis = basis;
            self.record(TransactionChange::AlterTable {
                table: name.clone(),
                alteration: alteration.clone(),
            });
        }
        Ok(changed)
    }
    fn create_schema(&mut self, name: &str, if_not_exists: bool) -> Result<()> {
        let record = self.journal.is_some()
            && !self
                .snapshot
                .schemas()?
                .contains(&name.to_ascii_lowercase());
        self.snapshot.create_schema(name, if_not_exists)?;
        self.catalog_basis.create_schema(name, if_not_exists)?;
        if record {
            self.record(TransactionChange::CreateSchema(name.to_ascii_lowercase()));
        }
        Ok(())
    }
    fn drop_schema(&mut self, name: &str, if_exists: bool) -> Result<()> {
        let record = self.journal.is_some()
            && self
                .snapshot
                .schemas()?
                .contains(&name.to_ascii_lowercase());
        self.snapshot.drop_schema(name, if_exists)?;
        self.catalog_basis.drop_schema(name, if_exists)?;
        if record {
            self.record(TransactionChange::DropSchema(name.to_ascii_lowercase()));
        }
        Ok(())
    }
    fn create_table(&mut self, definition: TableDefinition, if_not_exists: bool) -> Result<()> {
        let record = (self.journal.is_some() && self.snapshot.table(&definition.name).is_err())
            .then(|| TransactionChange::CreateTable(definition.clone()));
        self.snapshot
            .create_table(definition.clone(), if_not_exists)?;
        self.catalog_basis.create_table(definition, if_not_exists)?;
        if let Some(change) = record {
            self.record(change);
        }
        Ok(())
    }
    fn drop_table(&mut self, name: &TableName, if_exists: bool) -> Result<()> {
        let record = self.journal.is_some() && self.snapshot.table(name).is_ok();
        self.snapshot.drop_table(name, if_exists)?;
        self.catalog_basis.drop_table(name, if_exists)?;
        if record {
            self.record(TransactionChange::DropTable(name.clone()));
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotTransaction {
    fn record(&mut self, change: TransactionChange) {
        if let Some(journal) = &mut self.journal {
            journal.push(change);
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableStorageMut for SnapshotTransaction {
    fn insert(
        &mut self,
        table: &TableName,
        rows: Vec<Row>,
        context: &QueryContext,
    ) -> Result<usize> {
        let record = self.journal.as_ref().map(|_| TransactionChange::Insert {
            table: table.clone(),
            rows: rows.clone(),
        });
        let count = self.snapshot.insert(table, rows, context)?;
        if count != 0
            && let Some(change) = record
        {
            self.record(change);
        }
        Ok(count)
    }
    fn update(
        &mut self,
        table: &TableName,
        rows: Vec<(RowId, Row)>,
        context: &QueryContext,
    ) -> Result<usize> {
        let record = self.journal.as_ref().map(|_| TransactionChange::Update {
            table: table.clone(),
            // Statement validation observes the last replacement of each row.
            rows: rows
                .iter()
                .cloned()
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        });
        let count = self.snapshot.update(table, rows, context)?;
        if count != 0
            && let Some(change) = record
        {
            self.record(change);
        }
        Ok(count)
    }
    fn delete(
        &mut self,
        table: &TableName,
        ids: &[RowId],
        context: &QueryContext,
    ) -> Result<usize> {
        let record = if self.journal.is_some() {
            let ids: Vec<_> = ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let visible = self.snapshot.fetch(table, &ids, context)?;
            Some(TransactionChange::Delete {
                table: table.clone(),
                ids: ids
                    .into_iter()
                    .zip(visible)
                    .filter_map(|(id, row)| row.map(|_| id))
                    .collect(),
            })
        } else {
            None
        };
        let count = self.snapshot.delete(table, ids, context)?;
        if count != 0
            && let Some(change) = record
        {
            self.record(change);
        }
        Ok(count)
    }
}
