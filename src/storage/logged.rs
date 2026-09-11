//! WAL durability composes checkpoint recovery, transaction encoding and I/O.
use super::{
    checkpoint::{
        Durability, FileCheckpoint, PublishOutcome,
        policy::{CheckpointPolicy, LogProgress, LogSizeCheckpoint},
    },
    log::{Commit, LogCheckpoint, LogSession, TransactionLog},
    recovery::{RecoveryInput, RecoveryPublication},
    table::Snapshot,
};
use crate::{
    common::{Error, Result, type_registry::TypeRegistry},
    parallel::QueryContext,
};
use std::sync::{Arc, Mutex};

pub struct FileWal {
    checkpoint: FileCheckpoint,
    encoder: Arc<dyn TransactionLog>,
    state: Mutex<State>,
    policy: Option<Arc<dyn CheckpointPolicy>>,
}
enum State {
    Unloaded,
    Ready(Ready),
    Uncertain,
    RecoveryRequired,
}
struct Ready {
    session: Box<dyn LogSession>,
    length: u64,
    header: Vec<u8>,
    context: QueryContext,
    commits: u64,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State {
    fn ready(&mut self) -> Result<&mut Ready> {
        match self {
            Self::Ready(ready) => Ok(ready),
            Self::Unloaded => Err(Error::Internal(
                "transaction log has not been loaded".into(),
            )),
            Self::Uncertain => Err(Error::CommitUnknown("previous log append failed".into())),
            Self::RecoveryRequired => Err(Error::RecoveryRequired(
                "previous storage maintenance failed".into(),
            )),
        }
    }
    fn failed(&mut self, error: &Error) {
        match error {
            Error::CommitUnknown(_) => *self = Self::Uncertain,
            Error::RecoveryRequired(_) => *self = Self::RecoveryRequired,
            _ => (),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FileWal {
    pub fn new(checkpoint: FileCheckpoint, encoder: Arc<dyn TransactionLog>) -> Result<Self> {
        if checkpoint.format().format_id() != encoder.format_id() {
            return Err(Error::Unsupported(
                "transaction log and checkpoint format families differ".into(),
            ));
        }
        if !checkpoint.writable() || !checkpoint.storage().supports_log_append() {
            return Err(Error::Unsupported(
                "transaction logging requires a writable log storage adapter".into(),
            ));
        }
        if !checkpoint.format().supports_successor()
            || !checkpoint.storage().supports_recovery_publication()
            || !checkpoint
                .recovery()
                .is_some_and(|recovery| recovery.supports_preparation())
        {
            return Err(Error::Unsupported(
                "transaction logging requires compatible writable checkpoint recovery".into(),
            ));
        }
        Ok(Self {
            checkpoint,
            encoder,
            state: Mutex::new(State::Unloaded),
            policy: Some(Arc::new(LogSizeCheckpoint::default())),
        })
    }
    /// Select scheduling separately from encoding/publication. None permits
    /// explicit checkpoints only. The default is a 16 MiB log-size policy.
    pub fn with_checkpoint_policy(mut self, policy: Option<Arc<dyn CheckpointPolicy>>) -> Self {
        self.policy = policy;
        self
    }
    fn checkpoint_ready(
        &self,
        ready: &mut Ready,
        snapshot: &Snapshot,
        context: &QueryContext,
    ) -> Result<()> {
        context.check()?;
        if ready.length == 0 {
            return Ok(());
        }
        let input = RecoveryInput {
            checkpoint: self.checkpoint.storage().read()?,
            log: self.checkpoint.storage().read_log()?,
        };
        if input.log.len() as u64 != ready.length {
            return Err(Error::Transaction(
                "log changed before checkpoint preparation".into(),
            ));
        }
        let recovery = self
            .checkpoint
            .recovery()
            .ok_or_else(|| Error::Internal("missing checkpoint recovery adapter".into()))?;
        let prepared = recovery.prepare(input, self.checkpoint.format(), context)?;
        let bytes = match &prepared.publication {
            RecoveryPublication::Replace { checkpoint, .. } => checkpoint.as_slice(),
            RecoveryPublication::RetireLog => prepared.basis.checkpoint.as_slice(),
        };
        let start = ready.session.rebase(
            LogCheckpoint {
                format: self.checkpoint.format(),
                logical: snapshot,
                physical: &prepared.snapshot,
                bytes,
                layout: &prepared.layout,
            },
            context,
        )?;
        context.check()?;
        // No incoming transaction has been appended. A maintenance failure can
        // require recovery without making that transaction's outcome unknown.
        self.checkpoint
            .storage()
            .publish_recovery(&prepared.basis, &prepared.publication)
            .map_err(|error| Error::RecoveryRequired(error.to_string()))?;
        ready.session = start.session;
        ready.header = start.header;
        ready.length = 0;
        ready.commits = 0;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Durability for FileWal {
    fn name(&self) -> &'static str {
        "file-wal"
    }
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let mut adapters = self.checkpoint.adapters();
        adapters.retain(|(kind, _)| *kind != "durability");
        adapters.push(("durability", self.name()));
        adapters.push(("transaction-log", self.encoder.name()));
        adapters.push((
            "checkpoint-policy",
            self.policy
                .as_ref()
                .map_or("manual", |policy| policy.name()),
        ));
        adapters
    }
    fn requires_journal(&self) -> bool {
        true
    }
    fn load(&self, types: Arc<TypeRegistry>) -> Result<Snapshot> {
        self.load_with_context(&QueryContext::background().with_types(types))
    }
    fn load_with_context(&self, context: &QueryContext) -> Result<Snapshot> {
        context.check()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("log session mutex poisoned".into()))?;
        if !matches!(*state, State::Unloaded) {
            return Err(Error::Transaction(
                "transaction logger already loaded; share its transaction manager".into(),
            ));
        }
        let snapshot = self.checkpoint.load_with_context(context)?;
        let start =
            self.encoder
                .start_at(&snapshot, self.checkpoint.storage_version()?, context)?;
        context.check()?;
        *state = State::Ready(Ready {
            session: start.session,
            length: 0,
            header: start.header,
            context: context.clone(),
            commits: 0,
        });
        Ok(snapshot)
    }
    fn checkpoint(&self, snapshot: &Snapshot, context: &QueryContext) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("log session mutex poisoned".into()))?;
        let result = self.checkpoint_ready(state.ready()?, snapshot, context);
        if let Err(error) = &result {
            state.failed(error);
        }
        result
    }
    fn publish(&self, commit: Commit<'_>) -> Result<PublishOutcome> {
        let changes = commit.changes.ok_or_else(|| {
            Error::Internal("transaction logger requires an ordered journal".into())
        })?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Internal("log session mutex poisoned".into()))?;
        let result = (|| {
            let ready = state.ready()?;
            if changes.is_empty() {
                return Ok(PublishOutcome::Published);
            }
            let mut append = ready.session.prepare(changes, &ready.context)?;
            let progress = LogProgress {
                committed_bytes: ready.length,
                committed_transactions: ready.commits,
                pending_bytes: append.bytes.len() as u64,
            };
            let checkpointed = ready.commits != 0
                && self
                    .policy
                    .as_ref()
                    .is_some_and(|policy| policy.should_checkpoint(progress));
            if checkpointed {
                self.checkpoint_ready(ready, commit.before, &ready.context.clone())?;
                append = ready.session.prepare(changes, &ready.context)?;
            }
            let commits = ready
                .commits
                .checked_add(1)
                .ok_or_else(|| Error::Resource("log commit count exhausted".into()))?;
            if ready.length == 0 {
                ready.length = self
                    .checkpoint
                    .storage()
                    .initialize_log(&ready.header)
                    .map_err(|error| match error {
                        Error::CommitUnknown(message) => Error::RecoveryRequired(message),
                        other => other,
                    })?;
            }
            ready.length = self
                .checkpoint
                .storage()
                .append_log(ready.length, &append.bytes)?;
            ready.session = append.next;
            ready.commits = commits;
            Ok(if checkpointed {
                PublishOutcome::CheckpointedBefore
            } else {
                PublishOutcome::Published
            })
        })();
        if let Err(error) = &result {
            state.failed(error);
        }
        result
    }
}
