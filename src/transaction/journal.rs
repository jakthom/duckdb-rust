use super::*;
use crate::{
    catalog::{
        CreateConflictPolicy, DropBehavior, ResolvedType, TableDefinition, TypeBinding,
        TypeDefinition, TypeName, ViewBinding, ViewDefinition,
    },
    storage::UpdateMetadata,
};
use std::collections::BTreeSet;

/// Compact conflict domains retained by the transaction manager after a winner
/// publishes. They intentionally name logical rows/objects, never a snapshot
/// generation: readers therefore remain valid and disjoint writers can rebase.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ConflictDomain {
    Row(TableName, RowId),
    Append(TableName),
    /// Tables and views share the relation namespace.  DDL on either blocks
    /// stale row/append writes and replacement of the same relation name.
    Relation(TableName),
    /// A retained view definition depends on this relation's catalog shape.
    Dependency(TableName),
    Type(TypeName),
    Schema(String),
}

pub(super) fn conflict_domains(changes: &[TransactionChange]) -> Vec<ConflictDomain> {
    let mut domains = BTreeSet::new();
    for change in changes {
        match change {
            TransactionChange::Insert { table, .. }
            | TransactionChange::InsertChunks { table, .. } => {
                domains.insert(ConflictDomain::Append(table.clone()));
            }
            TransactionChange::Update { table, rows, .. } => {
                for (id, _) in rows {
                    domains.insert(ConflictDomain::Row(table.clone(), *id));
                }
            }
            TransactionChange::Delete { table, ids } => {
                for id in ids {
                    domains.insert(ConflictDomain::Row(table.clone(), *id));
                }
            }
            TransactionChange::CreateTable(definition) => {
                domains.insert(ConflictDomain::Relation(definition.name.clone()));
            }
            TransactionChange::DropTable(table) => {
                domains.insert(ConflictDomain::Relation(table.clone()));
            }
            TransactionChange::AlterTable {
                table, alteration, ..
            } => {
                domains.insert(ConflictDomain::Relation(table.clone()));
                if let crate::catalog::TableAlteration::RenameTable(name) = alteration {
                    domains.insert(ConflictDomain::Relation(TableName::new(
                        &table.schema,
                        name,
                    )));
                }
            }
            TransactionChange::CreateSchema(name) | TransactionChange::DropSchema(name) => {
                domains.insert(ConflictDomain::Schema(name.clone()));
            }
            TransactionChange::CreateView { definition, .. } => {
                domains.insert(ConflictDomain::Relation(definition.name.clone()));
                for dependency in &definition.dependencies {
                    let relation = match dependency {
                        crate::catalog::ViewDependency::Table(name)
                        | crate::catalog::ViewDependency::View(name) => name,
                    };
                    domains.insert(ConflictDomain::Dependency(relation.clone()));
                }
            }
            TransactionChange::DropView(name) => {
                domains.insert(ConflictDomain::Relation(name.clone()));
            }
            TransactionChange::CreateType { definition, .. } => {
                domains.insert(ConflictDomain::Type(definition.name.clone()));
            }
            TransactionChange::DropType(name) => {
                domains.insert(ConflictDomain::Type(name.clone()));
            }
        }
    }
    domains.into_iter().collect()
}

pub(super) fn domains_conflict(left: &[ConflictDomain], right: &[ConflictDomain]) -> bool {
    left.iter().any(|a| {
        right.iter().any(|b| match (a, b) {
            (ConflictDomain::Row(table_a, row_a), ConflictDomain::Row(table_b, row_b)) => {
                table_a == table_b && row_a == row_b
            }
            (ConflictDomain::Relation(a), ConflictDomain::Relation(b))
            | (ConflictDomain::Dependency(a), ConflictDomain::Dependency(b)) => a == b,
            (ConflictDomain::Relation(relation), ConflictDomain::Row(table, _))
            | (ConflictDomain::Row(table, _), ConflictDomain::Relation(relation)) => {
                relation == table
            }
            (ConflictDomain::Relation(relation), ConflictDomain::Append(table))
            | (ConflictDomain::Append(table), ConflictDomain::Relation(relation)) => {
                relation == table
            }
            (ConflictDomain::Relation(relation), ConflictDomain::Dependency(dependency))
            | (ConflictDomain::Dependency(dependency), ConflictDomain::Relation(relation)) => {
                relation == dependency
            }
            (ConflictDomain::Type(a), ConflictDomain::Type(b)) => a == b,
            (ConflictDomain::Schema(schema), domain) | (domain, ConflictDomain::Schema(schema)) => {
                match domain {
                    ConflictDomain::Schema(other) => schema == other,
                    ConflictDomain::Row(table, _)
                    | ConflictDomain::Append(table)
                    | ConflictDomain::Relation(table)
                    | ConflictDomain::Dependency(table) => schema == &table.schema,
                    ConflictDomain::Type(name) => schema == &name.schema,
                }
            }
            _ => false,
        })
    })
}

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
        let materialized_rows = if self.durability.requires_journal() {
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
        self.record(TransactionChange::CreateSchema(name.to_ascii_lowercase()));
        Ok(())
    }
    fn drop_schema(&mut self, name: &str, if_exists: bool) -> Result<()> {
        let record = self
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
        self.record(TransactionChange::CreateTable(definition));
        Ok(())
    }
    fn drop_table(&mut self, name: &TableName, if_exists: bool) -> Result<()> {
        let record = self.snapshot.table(name).is_ok();
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
        let definition = std::sync::Arc::new(definition);
        let mut snapshot = self.snapshot.clone();
        let mut basis = self.catalog_basis.clone();
        let changed = Snapshot::apply_view_creation_pair(
            &mut snapshot,
            &mut basis,
            definition.clone(),
            &prepared,
        )?;
        if changed {
            ensure_catalog_views_match(&snapshot, &basis)?;
            self.snapshot = snapshot;
            self.catalog_basis = basis;
            self.record(TransactionChange::CreateView {
                definition: definition.as_ref().clone(),
                conflict,
            });
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
            self.record(TransactionChange::DropView(name.clone()));
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
            self.record(TransactionChange::CreateType {
                definition,
                conflict,
            });
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
            self.record(TransactionChange::DropType(name.clone()));
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
        self.journal.push(change);
    }

    fn retain_rebase_context(&mut self, context: &QueryContext) {
        self.rebase_context = context.clone();
    }

    /// Replay this transaction's validated operations on a newer committed
    /// snapshot. Conflict domains have already excluded overlapping rows and
    /// catalog names; replay failure is therefore a conservative conflict and
    /// cannot publish a partial successor.
    pub(super) fn rebase(
        &self,
        basis: Snapshot,
        context: &QueryContext,
    ) -> Result<(Snapshot, Vec<TransactionChange>)> {
        let mut rebased = SnapshotTransaction {
            state: self.state.clone(),
            durability: self.durability.clone(),
            generation: 0,
            snapshot: basis.clone(),
            catalog_basis: basis,
            dirty: true,
            journal: Vec::new(),
            rebase_context: context.clone(),
            // Replay is an internal candidate and never registers an external
            // reader generation. Its enclosing transaction owns that lease.
            owner: OwnerState::Replay,
        };
        let mut remap = std::collections::BTreeMap::<(String, RowId), RowId>::new();
        for change in &self.journal {
            let applied = match change {
                TransactionChange::CreateSchema(name) => rebased.create_schema(name, false),
                TransactionChange::DropSchema(name) => rebased.drop_schema(name, false),
                TransactionChange::CreateTable(definition) => {
                    rebased.create_table(definition.clone(), false)
                }
                TransactionChange::DropTable(name) => rebased.drop_table(name, false),
                TransactionChange::CreateView {
                    definition,
                    conflict,
                } => rebased
                    .create_view(definition.clone(), conflict.clone())
                    .map(|_| ()),
                TransactionChange::DropView(name) => rebased
                    .drop_view(name, false, DropBehavior::Restrict)
                    .map(|_| ()),
                TransactionChange::CreateType {
                    definition,
                    conflict,
                } => rebased
                    .create_type(definition.clone(), conflict.clone())
                    .map(|_| ()),
                TransactionChange::DropType(name) => rebased
                    .drop_type(name, false, DropBehavior::Restrict)
                    .map(|_| ()),
                TransactionChange::AlterTable {
                    table, alteration, ..
                } => rebased.alter_table(table, alteration, context).map(|_| ()),
                TransactionChange::Insert {
                    table,
                    first_id,
                    rows,
                } => {
                    let new_first = rebased.snapshot.next_row_id(table)?;
                    for offset in 0..rows.len() {
                        remap.insert(
                            (table.to_string(), first_id.saturating_add(offset as RowId)),
                            new_first.saturating_add(offset as RowId),
                        );
                    }
                    rebased.insert(table, rows.clone(), context).map(|_| ())
                }
                TransactionChange::InsertChunks {
                    table,
                    first_id,
                    chunks,
                } => {
                    let new_first = rebased.snapshot.next_row_id(table)?;
                    let count = chunks.iter().try_fold(0usize, |count, chunk| {
                        let count = count.checked_add(chunk.len()).ok_or_else(|| {
                            Error::Resource("insert chunk row count overflow".into())
                        })?;
                        context.check_rows(count)?;
                        Ok::<_, Error>(count)
                    })?;
                    for offset in 0..count {
                        remap.insert(
                            (table.to_string(), first_id.saturating_add(offset as RowId)),
                            new_first.saturating_add(offset as RowId),
                        );
                    }
                    rebased
                        .insert_chunks(table, chunks.clone(), context)
                        .map(|_| ())
                }
                TransactionChange::Update {
                    table,
                    metadata,
                    rows,
                } => {
                    let rows = rows
                        .iter()
                        .map(|(id, row)| {
                            (
                                remap.get(&(table.to_string(), *id)).copied().unwrap_or(*id),
                                row.clone(),
                            )
                        })
                        .collect();
                    rebased.update(table, metadata, rows, context).map(|_| ())
                }
                TransactionChange::Delete { table, ids } => {
                    let ids = ids
                        .iter()
                        .map(|id| remap.get(&(table.to_string(), *id)).copied().unwrap_or(*id))
                        .collect::<Vec<_>>();
                    rebased.delete(table, &ids, context).map(|_| ())
                }
            };
            // Cancellation and resource limits belong to the caller.  A
            // semantic replay mismatch is the only condition converted to a
            // serialization conflict.
            if let Err(error) = applied {
                match error {
                    Error::Interrupted | Error::Resource(_) => return Err(error),
                    _ => return Err(Error::Conflict),
                }
            }
        }
        Ok((
            rebased.snapshot.clone(),
            std::mem::take(&mut rebased.journal),
        ))
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
        self.retain_rebase_context(context);
        let first_id = self.snapshot.next_row_id(table)?;
        let record = TransactionChange::Insert {
            table: table.clone(),
            first_id,
            rows: rows.clone(),
        };
        let count = self.snapshot.insert(table, rows, context)?;
        if count != 0 {
            self.record(record);
        }
        Ok(count)
    }
    fn insert_chunks(
        &mut self,
        table: &TableName,
        chunks: Vec<crate::common::vector::DataChunk>,
        context: &QueryContext,
    ) -> Result<usize> {
        self.retain_rebase_context(context);
        let first_id = self.snapshot.next_row_id(table)?;
        let count = chunks.iter().try_fold(0usize, |count, chunk| {
            let count = count
                .checked_add(chunk.len())
                .ok_or_else(|| Error::Resource("insert chunk row count overflow".into()))?;
            context.check_rows(count)?;
            Ok::<_, Error>(count)
        })?;
        let record = TransactionChange::InsertChunks {
            table: table.clone(),
            first_id,
            chunks: chunks.clone(),
        };
        let inserted = self.snapshot.insert_chunks(table, chunks, context)?;
        if inserted != 0 {
            debug_assert_eq!(inserted, count);
            self.record(record);
        }
        Ok(inserted)
    }
    fn update(
        &mut self,
        table: &TableName,
        metadata: &UpdateMetadata,
        rows: Vec<(RowId, Row)>,
        context: &QueryContext,
    ) -> Result<usize> {
        self.retain_rebase_context(context);
        let record = TransactionChange::Update {
            table: table.clone(),
            metadata: metadata.clone(),
            // Statement validation observes the last replacement of each row.
            rows: crate::storage::normalize_update_rows(rows.clone()),
        };
        let count = self.snapshot.update(table, metadata, rows, context)?;
        if count != 0 {
            self.record(record);
        }
        Ok(count)
    }
    fn delete(
        &mut self,
        table: &TableName,
        ids: &[RowId],
        context: &QueryContext,
    ) -> Result<usize> {
        self.retain_rebase_context(context);
        let record = {
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
