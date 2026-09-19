use super::*;
use crate::{
    catalog::{
        CreateConflictPolicy, DropBehavior, ResolvedType, TableDefinition, TypeBinding,
        TypeDefinition, TypeName, ViewBinding, ViewDefinition,
    },
    storage::UpdateMetadata,
};
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
    fn view(&self, name: &TableName) -> Result<ViewDefinition> {
        self.snapshot.view(name)
    }
    fn views(&self) -> Result<Vec<ViewDefinition>> {
        self.snapshot.views()
    }
    fn view_entry(&self, name: &TableName) -> Result<crate::catalog::ResolvedView> {
        self.snapshot.view_entry(name)
    }
    fn view_entry_if_exists(
        &self,
        name: &TableName,
    ) -> Result<Option<crate::catalog::ResolvedView>> {
        self.snapshot.view_entry_if_exists(name)
    }
    fn view_by_identity(
        &self,
        identity: &crate::catalog::ObjectIdentity,
    ) -> Result<crate::catalog::ResolvedView> {
        self.snapshot.view_by_identity(identity)
    }
    fn view_by_identity_if_exists(
        &self,
        identity: &crate::catalog::ObjectIdentity,
    ) -> Result<Option<crate::catalog::ResolvedView>> {
        self.snapshot.view_by_identity_if_exists(identity)
    }
    fn named_type(&self, name: &TypeName) -> Result<TypeDefinition> {
        self.snapshot.named_type(name)
    }
    fn named_types(&self) -> Result<Vec<TypeDefinition>> {
        self.snapshot.named_types()
    }
    fn table_entry(&self, name: &TableName) -> Result<crate::catalog::ResolvedTable> {
        self.snapshot.table_entry(name)
    }
    fn table_entry_if_exists(
        &self,
        name: &TableName,
    ) -> Result<Option<crate::catalog::ResolvedTable>> {
        self.snapshot.table_entry_if_exists(name)
    }
    fn table_by_identity(
        &self,
        identity: &crate::catalog::ObjectIdentity,
    ) -> Result<crate::catalog::ResolvedTable> {
        self.snapshot.table_by_identity(identity)
    }
    fn table_by_identity_if_exists(
        &self,
        identity: &crate::catalog::ObjectIdentity,
    ) -> Result<Option<crate::catalog::ResolvedTable>> {
        self.snapshot.table_by_identity_if_exists(identity)
    }
    fn type_entry(&self, name: &TypeName) -> Result<ResolvedType> {
        self.snapshot.type_entry(name)
    }
    fn type_entry_if_exists(&self, name: &TypeName) -> Result<Option<ResolvedType>> {
        self.snapshot.type_entry_if_exists(name)
    }
    fn type_by_identity(&self, identity: &crate::catalog::ObjectIdentity) -> Result<ResolvedType> {
        self.snapshot.type_by_identity(identity)
    }
    fn type_by_identity_if_exists(
        &self,
        identity: &crate::catalog::ObjectIdentity,
    ) -> Result<Option<ResolvedType>> {
        self.snapshot.type_by_identity_if_exists(identity)
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

    fn create_view(
        &mut self,
        definition: ViewDefinition,
        conflict: CreateConflictPolicy,
    ) -> Result<bool> {
        let prepared = self.snapshot.prepare_view_creation(&definition, conflict)?;
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        let changed = snapshot.apply_view_creation(definition.clone(), &prepared)?;
        let basis_changed = basis.apply_view_creation(definition.clone(), &prepared)?;
        if changed != basis_changed {
            return Err(Error::Internal(
                "transaction catalog views disagree about view creation".into(),
            ));
        }
        if changed {
            ensure_catalog_views_match(&snapshot, &basis)?;
            self.snapshot = snapshot;
            self.catalog_basis = basis;
            if self.journal.is_some() {
                self.record(TransactionChange::CreateView {
                    definition,
                    conflict,
                });
            }
        }
        Ok(changed)
    }

    fn drop_view(
        &mut self,
        name: &TableName,
        if_exists: bool,
        behavior: DropBehavior,
    ) -> Result<bool> {
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        let changed = snapshot.drop_view(name, if_exists, behavior)?;
        let basis_changed = basis.drop_view(name, if_exists, behavior)?;
        if changed != basis_changed {
            return Err(Error::Internal(
                "transaction catalog views disagree about view existence".into(),
            ));
        }
        if changed {
            ensure_catalog_views_match(&snapshot, &basis)?;
            self.snapshot = snapshot;
            self.catalog_basis = basis;
            if self.journal.is_some() {
                self.record(TransactionChange::DropView(name.clone()));
            }
        }
        Ok(changed)
    }

    fn drop_view_identified(
        &mut self,
        view: &ViewBinding,
        if_exists: bool,
        behavior: DropBehavior,
    ) -> Result<bool> {
        if view.identity().is_none() {
            return Err(Error::InvalidInput(
                "runtime transaction requires an identified view binding".into(),
            ));
        }
        let name = match self.snapshot.resolve_view_binding_if_exists(view)? {
            Some(resolved) => resolved.definition().name.clone(),
            None if if_exists => return Ok(false),
            None => return Err(Error::Catalog(format!("view {view} does not exist"))),
        };
        self.drop_view(&name, if_exists, behavior)
    }

    fn create_type(
        &mut self,
        definition: TypeDefinition,
        conflict: CreateConflictPolicy,
    ) -> Result<bool> {
        let prepared = self.snapshot.prepare_type_creation(&definition, conflict)?;
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        let changed = snapshot.apply_type_creation(definition.clone(), &prepared)?;
        let basis_changed = basis.apply_type_creation(definition.clone(), &prepared)?;
        if changed != basis_changed {
            return Err(Error::Internal(
                "transaction catalog views disagree about named type creation".into(),
            ));
        }
        if changed {
            ensure_catalog_views_match(&snapshot, &basis)?;
            self.snapshot = snapshot;
            self.catalog_basis = basis;
            if self.journal.is_some() {
                self.record(TransactionChange::CreateType {
                    definition,
                    conflict,
                });
            }
        }
        Ok(changed)
    }

    fn drop_type(
        &mut self,
        name: &TypeName,
        if_exists: bool,
        behavior: DropBehavior,
    ) -> Result<bool> {
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        let changed = snapshot.drop_type(name, if_exists, behavior)?;
        let basis_changed = basis.drop_type(name, if_exists, behavior)?;
        if changed != basis_changed {
            return Err(Error::Internal(
                "transaction catalog views disagree about named type existence".into(),
            ));
        }
        if changed {
            ensure_catalog_views_match(&snapshot, &basis)?;
            self.snapshot = snapshot;
            self.catalog_basis = basis;
            if self.journal.is_some() {
                self.record(TransactionChange::DropType(name.clone()));
            }
        }
        Ok(changed)
    }

    fn drop_type_identified(
        &mut self,
        type_: &TypeBinding,
        if_exists: bool,
        behavior: DropBehavior,
    ) -> Result<bool> {
        if type_.identity().is_none() {
            return Err(Error::InvalidInput(
                "runtime transaction requires an identified type binding".into(),
            ));
        }
        let name = match self.snapshot.resolve_type_binding_if_exists(type_)? {
            Some(resolved) => resolved.definition().name.clone(),
            None if if_exists => return Ok(false),
            None => return Err(Error::Catalog(format!("type {type_} does not exist"))),
        };
        self.drop_type(&name, if_exists, behavior)
    }

    fn drop_table_identified(
        &mut self,
        table: &crate::catalog::TableBinding,
        if_exists: bool,
    ) -> Result<()> {
        if table.identity().is_none() {
            return Err(Error::InvalidInput(
                "runtime transaction requires an identified table binding".into(),
            ));
        }
        let name = match self.snapshot.resolve_table_binding_if_exists(table)? {
            Some(resolved) => resolved.definition().name.clone(),
            None if if_exists => return Ok(()),
            None => return Err(Error::Catalog(format!("table {table} does not exist"))),
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
    fn insert_chunks(
        &mut self,
        table: &TableName,
        chunks: Vec<crate::common::vector::DataChunk>,
        context: &QueryContext,
    ) -> Result<usize> {
        if self.journal.is_some() {
            let mut rows = Vec::new();
            for chunk in chunks {
                context.check_rows(rows.len().saturating_add(chunk.len()))?;
                rows.extend(chunk.rows());
            }
            self.insert(table, rows, context)
        } else {
            self.snapshot.insert_chunks(table, chunks, context)
        }
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
