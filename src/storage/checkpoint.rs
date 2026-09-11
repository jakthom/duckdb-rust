use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use super::{
    filesystem::{CheckpointStorage, LocalCheckpointStorage, OpenMode},
    format::SnapshotFormat,
    log::Commit,
    recovery::{Recovery, RecoveryInput},
    table::Snapshot,
};
use crate::common::{Error, Result};
pub mod policy;

/// Durable work completed while publishing one transaction. A checkpointed
/// basis is the acknowledged `Commit::before` state, never the incoming
/// transaction itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PublishOutcome {
    #[default]
    Published,
    CheckpointedBefore,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
    /// Startup/recovery receives explicitly selected types, settings and closed
    /// expression services. Legacy adapters retain their own load capability.
    fn load_with_context(&self, context: &crate::parallel::QueryContext) -> Result<Snapshot> {
        context.check()?;
        let snapshot = self.load(context.type_registry())?;
        context.check()?;
        Ok(snapshot)
    }
    fn requires_journal(&self) -> bool {
        false
    }
    /// Return `CheckpointedBefore` only after the pre-commit snapshot was
    /// durably checkpointed. Adapters returning it must require a journal so
    /// the transaction manager can preserve incoming physical-slot changes.
    fn publish(&self, commit: Commit<'_>) -> Result<PublishOutcome>;
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Durability for MemoryDurability {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn load(&self, types: Arc<crate::common::type_registry::TypeRegistry>) -> Result<Snapshot> {
        Snapshot::try_new(types)
    }
    fn publish(&self, _commit: Commit<'_>) -> Result<PublishOutcome> {
        Ok(PublishOutcome::Published)
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
    publication: Mutex<PublicationState>,
}

enum PublicationState {
    Unloaded,
    Ready {
        encoder: Option<Box<dyn super::format::CheckpointEncoder>>,
        context: crate::parallel::QueryContext,
    },
    Uncertain,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FileCheckpoint {
    pub fn new(file: Arc<dyn CheckpointStorage>, format: Arc<dyn SnapshotFormat>) -> Self {
        Self {
            file,
            format,
            recovery: None,
            publication: Mutex::new(PublicationState::Unloaded),
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
    /// Hand only format-owned compatibility metadata to the selected logger.
    /// The bound encoder was installed by load; no file is reread or cached here.
    pub(super) fn storage_version(&self) -> Result<Option<super::format::StorageVersion>> {
        let publication = self
            .publication
            .lock()
            .map_err(|_| Error::Internal("checkpoint publication mutex poisoned".into()))?;
        let version = match &*publication {
            PublicationState::Ready {
                encoder: Some(encoder),
                ..
            } => encoder.storage_version(),
            PublicationState::Ready { encoder: None, .. } => None,
            PublicationState::Unloaded => {
                return Err(Error::Internal("checkpoint has not been loaded".into()));
            }
            PublicationState::Uncertain => {
                return Err(Error::CommitUnknown(
                    "previous checkpoint publication failed".into(),
                ));
            }
        };
        if version.is_some_and(|version| version.format != self.format.format_id()) {
            return Err(Error::Internal(
                "checkpoint compatibility format differs from its encoder".into(),
            ));
        }
        Ok(version)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
        self.load_with_context(&crate::parallel::QueryContext::background().with_types(types))
    }
    fn load_with_context(&self, context: &crate::parallel::QueryContext) -> Result<Snapshot> {
        context.check()?;
        let mut publication = self
            .publication
            .lock()
            .map_err(|_| Error::Internal("checkpoint publication mutex poisoned".into()))?;
        if !matches!(*publication, PublicationState::Unloaded) {
            return Err(Error::Transaction(
                "checkpoint already loaded; share its transaction manager".into(),
            ));
        }
        let checkpoint = self.file.read()?;
        let log = self.file.read_log()?;
        if log.is_empty() {
            let encoder = self.format.checkpoint_encoder(&checkpoint)?;
            let snapshot = self.format.decode_with_context(checkpoint, context)?;
            context.check()?;
            *publication = PublicationState::Ready {
                encoder,
                context: context.clone(),
            };
            return Ok(snapshot);
        }
        let recovery = self.recovery.as_ref().ok_or_else(|| {
            Error::Unsupported("no recovery adapter selected for nonempty log".into())
        })?;
        let input = RecoveryInput { checkpoint, log };
        if self.writable() {
            if !self.file.supports_recovery_publication() {
                return Err(Error::Unsupported(
                    "recovery publication on this storage".into(),
                ));
            }
            let prepared = recovery.prepare(input, self.format.as_ref(), context)?;
            let bytes = match &prepared.publication {
                super::recovery::RecoveryPublication::Replace { checkpoint, .. } => checkpoint,
                super::recovery::RecoveryPublication::RetireLog => &prepared.basis.checkpoint,
            };
            let encoder = self.format.checkpoint_encoder(bytes)?;
            context.check()?;
            if let Err(error) = self
                .file
                .publish_recovery(&prepared.basis, &prepared.publication)
            {
                if matches!(error, Error::CommitUnknown(_)) {
                    *publication = PublicationState::Uncertain;
                }
                return Err(error);
            }
            *publication = PublicationState::Ready {
                encoder,
                context: context.clone(),
            };
            Ok(prepared.snapshot)
        } else {
            let snapshot = recovery.recover(input, self.format.as_ref(), context)?;
            context.check()?;
            *publication = PublicationState::Ready {
                encoder: None,
                context: context.clone(),
            };
            Ok(snapshot)
        }
    }
    fn publish(&self, commit: Commit<'_>) -> Result<PublishOutcome> {
        if !self.writable() {
            return Err(Error::Unsupported("writing a read-only checkpoint".into()));
        }
        let mut publication = self
            .publication
            .lock()
            .map_err(|_| Error::Internal("checkpoint publication mutex poisoned".into()))?;
        let (encoder, context) = match &*publication {
            PublicationState::Ready { encoder, context } => (encoder, context),
            PublicationState::Unloaded => {
                return Err(Error::Internal("checkpoint has not been loaded".into()));
            }
            PublicationState::Uncertain => {
                return Err(Error::CommitUnknown(
                    "previous checkpoint publication failed".into(),
                ));
            }
        };
        let bytes = match encoder {
            Some(encoder) => encoder.encode_with_context(commit.snapshot, context)?,
            None => self.format.encode_with_context(commit.snapshot, context)?,
        };
        let next = self.format.checkpoint_encoder(&bytes)?;
        context.check()?;
        let context = context.clone();
        if let Err(error) = self.file.replace(&bytes) {
            if matches!(error, Error::CommitUnknown(_)) {
                *publication = PublicationState::Uncertain;
            }
            return Err(error);
        }
        *publication = PublicationState::Ready {
            encoder: next,
            context,
        };
        Ok(PublishOutcome::Published)
    }
}
