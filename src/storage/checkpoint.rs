use std::{path::Path, sync::Arc};

use super::{
    filesystem::{CheckpointStorage, LocalCheckpointStorage, OpenMode},
    format::SnapshotFormat,
    log::Commit,
    recovery::{Recovery, RecoveryInput},
    table::Snapshot,
};
use crate::common::{Error, Result};
pub mod policy;

/// Publication must complete before a transaction becomes visible. An uncertain
/// publication returns CommitUnknown, preventing further commits until recovery.
pub trait Durability: Send + Sync {
    fn name(&self) -> &'static str;
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        vec![("durability", self.name())]
    }
    fn writable(&self) -> bool {
        true
    }
    fn load(&self, types: Arc<crate::common::type_registry::TypeRegistry>) -> Result<Snapshot>;
    fn requires_journal(&self) -> bool {
        false
    }
    fn publish(&self, commit: Commit<'_>) -> Result<()>;
    /// Persist acknowledged state without changing logical identities. The
    /// transaction manager serializes this with commits. Preparation errors
    /// leave the writer usable; publication failures can require recovery.
    fn checkpoint(
        &self,
        _snapshot: &Snapshot,
        _context: &crate::parallel::QueryContext,
    ) -> Result<()> {
        Err(Error::Unsupported(
            "checkpoint maintenance on this durability adapter".into(),
        ))
    }
}

#[derive(Default)]
pub struct MemoryDurability;

impl Durability for MemoryDurability {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn load(&self, types: Arc<crate::common::type_registry::TypeRegistry>) -> Result<Snapshot> {
        Ok(Snapshot::new(types))
    }
    fn publish(&self, _commit: Commit<'_>) -> Result<()> {
        Ok(())
    }
    fn checkpoint(
        &self,
        _snapshot: &Snapshot,
        context: &crate::parallel::QueryContext,
    ) -> Result<()> {
        context.check()
    }
}

/// File publication and checkpoint representation are independently selected.
pub struct FileCheckpoint {
    file: Arc<dyn CheckpointStorage>,
    format: Arc<dyn SnapshotFormat>,
    recovery: Option<Arc<dyn Recovery>>,
}

impl FileCheckpoint {
    pub fn new(file: Arc<dyn CheckpointStorage>, format: Arc<dyn SnapshotFormat>) -> Self {
        Self {
            file,
            format,
            recovery: None,
        }
    }
    /// Select recovery independently of checkpoint representation and I/O.
    pub fn with_recovery(mut self, recovery: Arc<dyn Recovery>) -> Result<Self> {
        if recovery.format_id() != self.format.format_id() {
            return Err(Error::Unsupported(
                "recovery and checkpoint format families differ".into(),
            ));
        }
        self.recovery = Some(recovery);
        Ok(self)
    }
    pub fn open(
        path: impl AsRef<Path>,
        mode: OpenMode,
        format: Arc<dyn SnapshotFormat>,
    ) -> Result<Self> {
        let file = LocalCheckpointStorage::open(path.as_ref(), mode, || {
            format.encode(&Snapshot::default())
        })?;
        Ok(Self::new(Arc::new(file), format))
    }
    pub fn format(&self) -> &dyn SnapshotFormat {
        self.format.as_ref()
    }
    pub fn storage(&self) -> &dyn CheckpointStorage {
        self.file.as_ref()
    }
    pub fn recovery(&self) -> Option<&dyn Recovery> {
        self.recovery.as_deref()
    }
}

impl Durability for FileCheckpoint {
    fn checkpoint(
        &self,
        _snapshot: &Snapshot,
        context: &crate::parallel::QueryContext,
    ) -> Result<()> {
        context.check()?;
        if !self.writable() {
            return Err(Error::Unsupported("checkpoint on a read-only file".into()));
        }
        // Every successful commit already publishes a complete checkpoint.
        Ok(())
    }
    fn name(&self) -> &'static str {
        self.format.name()
    }
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let mut adapters = vec![
            ("durability", "file-checkpoint"),
            ("filesystem", self.file.name()),
        ];
        adapters.extend(self.format.adapters());
        if let Some(recovery) = &self.recovery {
            adapters.push(("recovery", recovery.name()));
        }
        adapters
    }
    fn writable(&self) -> bool {
        self.file.writable()
    }
    fn load(&self, types: Arc<crate::common::type_registry::TypeRegistry>) -> Result<Snapshot> {
        let checkpoint = self.file.read()?;
        let log = self.file.read_log()?;
        if log.is_empty() {
            return self.format.decode(checkpoint, types);
        }
        let recovery = self.recovery.as_ref().ok_or_else(|| {
            Error::Unsupported("no recovery adapter selected for nonempty log".into())
        })?;
        let input = RecoveryInput { checkpoint, log };
        let context = crate::parallel::QueryContext::background().with_types(types);
        if self.writable() {
            if !self.file.supports_recovery_publication() {
                return Err(Error::Unsupported(
                    "recovery publication on this storage".into(),
                ));
            }
            let prepared = recovery.prepare(input, self.format.as_ref(), &context)?;
            self.file
                .publish_recovery(&prepared.basis, &prepared.publication)?;
            Ok(prepared.snapshot)
        } else {
            recovery.recover(input, self.format.as_ref(), &context)
        }
    }
    fn publish(&self, commit: Commit<'_>) -> Result<()> {
        if !self.writable() {
            return Err(Error::Unsupported("writing a read-only checkpoint".into()));
        }
        self.file.replace(&self.format.encode(commit.snapshot)?)
    }
}
