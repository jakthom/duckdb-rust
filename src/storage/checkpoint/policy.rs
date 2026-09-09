//! Replaceable checkpoint scheduling decisions, without I/O or clocks.
use std::num::NonZeroU64;

#[derive(Clone, Copy, Debug)]
pub struct LogProgress {
    pub committed_bytes: u64,
    /// Successful nonempty transaction appends since the last checkpoint.
    pub committed_transactions: u64,
    pub pending_bytes: u64,
}

/// Pure, concurrently callable decision over durable progress and a prepared
/// incoming transaction. Policies request checkpointing only acknowledged work;
/// they cannot change transaction contents, visibility or publication ordering.
pub trait CheckpointPolicy: Send + Sync {
    fn name(&self) -> &'static str;
    fn should_checkpoint(&self, progress: LogProgress) -> bool;
}

pub struct LogSizeCheckpoint(pub NonZeroU64);
impl Default for LogSizeCheckpoint {
    fn default() -> Self {
        Self(NonZeroU64::new(16 * 1024 * 1024).unwrap())
    }
}
impl CheckpointPolicy for LogSizeCheckpoint {
    fn name(&self) -> &'static str {
        "checkpoint-by-log-size"
    }
    fn should_checkpoint(&self, progress: LogProgress) -> bool {
        progress
            .committed_bytes
            .saturating_add(progress.pending_bytes)
            > self.0.get()
    }
}

pub struct CommitCountCheckpoint(pub NonZeroU64);
impl CheckpointPolicy for CommitCountCheckpoint {
    fn name(&self) -> &'static str {
        "checkpoint-by-commit-count"
    }
    fn should_checkpoint(&self, progress: LogProgress) -> bool {
        progress.committed_transactions >= self.0.get()
    }
}
