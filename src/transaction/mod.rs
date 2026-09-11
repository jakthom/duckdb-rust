use std::sync::{Arc, Mutex};

use crate::{
    catalog::{Catalog, CatalogMut, TableName},
    common::{Error, Result, Row},
    execution::index::{HashIndexFactory, IndexFactory},
    parallel::QueryContext,
    storage::{
        RowId, StorageCapabilities, TableStorage, TableStorageMut,
        checkpoint::Durability,
        log::{Commit, TransactionChange},
        table::Snapshot,
    },
};

mod journal;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Transaction-local catalog and data share visibility and publication. Dropping
/// an uncommitted transaction discards all its changes.
pub trait Transaction: Send {
    fn catalog(&self) -> &dyn Catalog;
    fn catalog_mut(&mut self) -> Result<&mut dyn CatalogMut>;
    fn storage(&self) -> &dyn TableStorage;
    fn storage_mut(&mut self) -> Result<&mut dyn TableStorageMut>;
    fn commit(self: Box<Self>) -> Result<()>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait TransactionManager: Send + Sync {
    fn types(&self) -> Arc<crate::common::type_registry::TypeRegistry>;
    fn name(&self) -> &'static str;
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        vec![("transactions", self.name())]
    }
    fn begin(&self) -> Result<Box<dyn Transaction>>;
    /// Checkpoint acknowledged state, serialized with write commits. Existing
    /// snapshots retain their rows/IDs. Implementations that cannot coordinate
    /// maintenance reject it before effects.
    fn checkpoint(&self, _context: &QueryContext) -> Result<()> {
        Err(Error::Unsupported(
            "checkpoint on this transaction manager".into(),
        ))
    }
}

struct Committed {
    generation: u64,
    snapshot: Snapshot,
    failure: Option<Failure>,
}

enum Failure {
    CommitUnknown,
    RecoveryRequired,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Committed {
    fn check(&self) -> Result<()> {
        match self.failure {
            None => Ok(()),
            Some(Failure::CommitUnknown) => {
                Err(Error::CommitUnknown("previous publication failed".into()))
            }
            Some(Failure::RecoveryRequired) => Err(Error::RecoveryRequired(
                "previous storage maintenance failed".into(),
            )),
        }
    }
    fn failed(&mut self, error: &Error) {
        self.failure = match error {
            Error::CommitUnknown(_) => Some(Failure::CommitUnknown),
            Error::RecoveryRequired(_) => Some(Failure::RecoveryRequired),
            _ => None,
        };
    }
}

/// Optimistic serializable snapshots: any intervening write conflicts with a
/// writer. Read-only transactions retain their snapshot and never conflict.
pub struct SnapshotTransactions {
    state: Arc<Mutex<Committed>>,
    durability: Arc<dyn Durability>,
    indexes: Arc<dyn IndexFactory>,
    types: Arc<crate::common::type_registry::TypeRegistry>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotTransactions {
    pub fn new(durability: Arc<dyn Durability>) -> Result<Self> {
        Self::with_indexes(durability, Arc::new(HashIndexFactory))
    }
    pub fn with_indexes(
        durability: Arc<dyn Durability>,
        indexes: Arc<dyn IndexFactory>,
    ) -> Result<Self> {
        Self::configured(
            durability,
            indexes,
            crate::common::type_registry::builtin_types(),
        )
    }
    pub fn configured(
        durability: Arc<dyn Durability>,
        indexes: Arc<dyn IndexFactory>,
        types: Arc<crate::common::type_registry::TypeRegistry>,
    ) -> Result<Self> {
        Self::configured_with_context(
            durability,
            indexes,
            &QueryContext::background().with_types(types),
        )
    }
    /// Compose recovery with the same explicitly selected services as startup.
    pub fn configured_with_context(
        durability: Arc<dyn Durability>,
        indexes: Arc<dyn IndexFactory>,
        context: &QueryContext,
    ) -> Result<Self> {
        context.check()?;
        let snapshot = durability
            .load_with_context(context)?
            .with_indexes(indexes.clone(), context)?;
        context.check()?;
        Ok(Self {
            state: Arc::new(Mutex::new(Committed {
                generation: 0,
                snapshot,
                failure: None,
            })),
            durability,
            indexes,
            types: context.type_registry(),
        })
    }
}

struct SnapshotTransaction {
    state: Arc<Mutex<Committed>>,
    durability: Arc<dyn Durability>,
    generation: u64,
    snapshot: Snapshot,
    dirty: bool,
    // Catalog changes applied to the starting rows, without uncommitted DML.
    // New constraints must also hold for committed versions still visible to
    // other readers and native recovery's catalog phase.
    catalog_basis: Snapshot,
    journal: Option<Vec<TransactionChange>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TransactionManager for SnapshotTransactions {
    fn checkpoint(&self, context: &QueryContext) -> Result<()> {
        context.check()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("transaction mutex poisoned".into()))?;
        state.check()?;
        let compacted = state.snapshot.reclaim_for_checkpoint();
        if let Err(error) = self.durability.checkpoint(&compacted, context) {
            state.failed(&error);
            return Err(error);
        }
        state.snapshot = compacted;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::Resource("transaction identity exhausted".into()))?;
        Ok(())
    }
    fn types(&self) -> Arc<crate::common::type_registry::TypeRegistry> {
        self.types.clone()
    }
    fn name(&self) -> &'static str {
        "optimistic-snapshot"
    }
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let mut adapters = vec![
            ("transactions", self.name()),
            ("catalog", "snapshot-catalog"),
            ("storage", "snapshot-table"),
            ("indexes", self.indexes.name()),
        ];
        adapters.extend(self.types.adapters());
        adapters.extend(self.durability.adapters());
        adapters
    }
    fn begin(&self) -> Result<Box<dyn Transaction>> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("transaction mutex poisoned".into()))?;
        state.check()?;
        Ok(Box::new(SnapshotTransaction {
            state: self.state.clone(),
            durability: self.durability.clone(),
            generation: state.generation,
            snapshot: state.snapshot.clone(),
            catalog_basis: state.snapshot.clone(),
            dirty: false,
            journal: self.durability.requires_journal().then(Vec::new),
        }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Transaction for SnapshotTransaction {
    fn catalog(&self) -> &dyn Catalog {
        &self.snapshot
    }
    fn catalog_mut(&mut self) -> Result<&mut dyn CatalogMut> {
        if !self.durability.writable() {
            return Err(Error::Unsupported(
                "catalog mutation on a read-only database".into(),
            ));
        }
        self.dirty = true;
        Ok(self)
    }
    fn storage(&self) -> &dyn TableStorage {
        self
    }
    fn storage_mut(&mut self) -> Result<&mut dyn TableStorageMut> {
        if !self.durability.writable() {
            return Err(Error::Unsupported(
                "mutation on a read-only database".into(),
            ));
        }
        self.dirty = true;
        Ok(self)
    }
    fn commit(self: Box<Self>) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("transaction mutex poisoned".into()))?;
        state.check()?;
        if state.generation != self.generation {
            return Err(Error::Conflict);
        }
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::Resource("transaction identity exhausted".into()))?;
        if let Err(error) = self.durability.publish(Commit {
            before: &state.snapshot,
            snapshot: &self.snapshot,
            changes: self.journal.as_deref(),
        }) {
            state.failed(&error);
            return Err(error);
        }
        state.snapshot = self.snapshot;
        state.generation = generation;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableStorage for SnapshotTransaction {
    fn row_count(&self, table: &TableName) -> Result<Option<usize>> {
        self.snapshot.row_count(table)
    }
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities {
            mutable: self.durability.writable(),
            ..self.snapshot.capabilities()
        }
    }
    fn key_columns(&self, table: &TableName) -> Result<Vec<Vec<usize>>> {
        self.snapshot.key_columns(table)
    }
    fn open_scan(
        &self,
        table: &TableName,
    ) -> Result<Box<dyn crate::storage::scan::TableScan + '_>> {
        self.snapshot.open_scan(table)
    }
    fn fetch(
        &self,
        table: &TableName,
        ids: &[RowId],
        context: &QueryContext,
    ) -> Result<Vec<Option<Row>>> {
        self.snapshot.fetch(table, ids, context)
    }
    fn lookup(
        &self,
        table: &TableName,
        columns: &[usize],
        key: &Row,
        context: &QueryContext,
    ) -> Result<Vec<(RowId, Row)>> {
        self.snapshot.lookup(table, columns, key, context)
    }
}
