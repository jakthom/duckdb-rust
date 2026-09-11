use super::*;
use crate::{catalog::TableDefinition, storage::UpdateMetadata};
use std::collections::BTreeSet;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Catalog for SnapshotTransaction {
    fn identity(&self) -> Option<crate::catalog::CatalogIdentity> {
        self.snapshot.identity()
    }
    fn schemas(&self) -> Result<Vec<String>> {
        self.snapshot.schemas()
    }
    fn table(&self, name: &TableName) -> Result<TableDefinition> {
        self.snapshot.table(name)
    }
    fn tables(&self) -> Result<Vec<TableDefinition>> {
        self.snapshot.tables()
    }
    fn table_entry(&self, name: &TableName) -> Result<crate::catalog::ResolvedTable> {
        self.snapshot.table_entry(name)
    }
    fn table_by_identity(
        &self,
        identity: &crate::catalog::ObjectIdentity,
    ) -> Result<crate::catalog::ResolvedTable> {
        self.snapshot.table_by_identity(identity)
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
        let prepared = self.snapshot.prepare_alter(name, alteration, context)?;
        let materialized_rows = if self.journal.is_some() {
            self.snapshot.prepared_add_rows(name, &prepared, context)?
        } else {
            None
        };
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        basis.apply_prepared_alter(name, alteration, &prepared, context)?;
        let changed = snapshot.apply_prepared_alter(name, alteration, &prepared, context)?;
        if changed {
            ensure_catalog_views_match(&snapshot, &basis)?;
            self.snapshot = snapshot;
            self.catalog_basis = basis;
            self.record(TransactionChange::AlterTable {
                table: name.clone(),
                alteration: alteration.clone(),
                materialized_rows,
            });
        }
        Ok(changed)
    }
    fn create_schema(&mut self, name: &str, if_not_exists: bool) -> Result<()> {
        let Some(prepared) = self.snapshot.prepare_schema_creation(name, if_not_exists)? else {
            if self
                .catalog_basis
                .prepare_schema_creation(name, if_not_exists)?
                .is_some()
            {
                return Err(Error::Internal(
                    "transaction catalog views disagree about schema existence".into(),
                ));
            }
            return Ok(());
        };
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        snapshot.apply_schema_creation(name, &prepared)?;
        basis.apply_schema_creation(name, &prepared)?;
        ensure_catalog_views_match(&snapshot, &basis)?;
        self.snapshot = snapshot;
        self.catalog_basis = basis;
        if self.journal.is_some() {
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
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        snapshot.drop_schema(name, if_exists)?;
        basis.drop_schema(name, if_exists)?;
        ensure_catalog_views_match(&snapshot, &basis)?;
        self.snapshot = snapshot;
        self.catalog_basis = basis;
        if record {
            self.record(TransactionChange::DropSchema(name.to_ascii_lowercase()));
        }
        Ok(())
    }
    fn create_table(&mut self, definition: TableDefinition, if_not_exists: bool) -> Result<()> {
        let Some(prepared) = self
            .snapshot
            .prepare_table_creation(&definition, if_not_exists)?
        else {
            if self
                .catalog_basis
                .prepare_table_creation(&definition, if_not_exists)?
                .is_some()
            {
                return Err(Error::Internal(
                    "transaction catalog views disagree about table existence".into(),
                ));
            }
            return Ok(());
        };
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        snapshot.apply_table_creation(definition.clone(), &prepared)?;
        basis.apply_table_creation(definition.clone(), &prepared)?;
        ensure_catalog_views_match(&snapshot, &basis)?;
        self.snapshot = snapshot;
        self.catalog_basis = basis;
        if self.journal.is_some() {
            self.record(TransactionChange::CreateTable(definition));
        }
        Ok(())
    }
    fn drop_table(&mut self, name: &TableName, if_exists: bool) -> Result<()> {
        let record = self.journal.is_some() && self.snapshot.table(name).is_ok();
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        snapshot.drop_table(name, if_exists)?;
        basis.drop_table(name, if_exists)?;
        ensure_catalog_views_match(&snapshot, &basis)?;
        self.snapshot = snapshot;
        self.catalog_basis = basis;
        if record {
            self.record(TransactionChange::DropTable(name.clone()));
        }
        Ok(())
    }

    fn drop_table_identified(
        &mut self,
        table: &crate::catalog::TableBinding,
        if_exists: bool,
    ) -> Result<()> {
        let identity = table.identity().ok_or_else(|| {
            Error::InvalidInput("runtime transaction requires an identified table binding".into())
        })?;
        let name = match self.snapshot.table_by_identity(&identity) {
            Ok(resolved) => resolved.definition().name.clone(),
            Err(Error::Catalog(_)) if if_exists => return Ok(()),
            Err(error) => return Err(error),
        };
        self.drop_table(&name, if_exists)
    }

    fn alter_table_identified(
        &mut self,
        table: &crate::catalog::TableBinding,
        alteration: &crate::catalog::TableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        let identity = table.identity().ok_or_else(|| {
            Error::InvalidInput("runtime transaction requires an identified table binding".into())
        })?;
        let name = self
            .snapshot
            .table_by_identity(&identity)?
            .definition()
            .name
            .clone();
        self.alter_table(&name, alteration, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ensure_catalog_views_match(snapshot: &Snapshot, basis: &Snapshot) -> Result<()> {
    if !snapshot.has_same_runtime_catalog(basis) {
        return Err(Error::Internal(
            "transaction catalog views have different runtime identities".into(),
        ));
    }
    Ok(())
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
        metadata: &UpdateMetadata,
        rows: Vec<(RowId, Row)>,
        context: &QueryContext,
    ) -> Result<usize> {
        let record = self.journal.as_ref().map(|_| TransactionChange::Update {
            table: table.clone(),
            metadata: metadata.clone(),
            // Statement validation observes the last replacement of each row.
            rows: crate::storage::normalize_update_rows(rows.clone()),
        });
        let count = self.snapshot.update(table, metadata, rows, context)?;
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
