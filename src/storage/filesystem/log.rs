use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl LocalCheckpointStorage {
    pub(super) fn initialize_transaction_log_transaction(
        &self,
        header: &[u8],
        transaction: &[u8],
    ) -> Result<u64> {
        if !self.writable {
            return Err(Error::Unsupported("logging on read-only storage".into()));
        }
        if header.is_empty() || transaction.is_empty() {
            return Err(Error::Internal(
                "empty transaction log header or first transaction".into(),
            ));
        }
        let length = header
            .len()
            .checked_add(transaction.len())
            .filter(|length| *length <= 512 * 1024 * 1024)
            .ok_or_else(|| Error::Resource("WAL exceeds 512 MiB".into()))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| Error::Resource("WAL allocation failed".into()))?;
        bytes.extend_from_slice(header);
        bytes.extend_from_slice(transaction);
        // The established initializer treats its nonempty bytes as opaque and
        // already stages, syncs, renames, and parent-syncs with the required
        // pre-publication versus CommitUnknown failure distinction.
        self.initialize_transaction_log(&bytes)
    }

    pub(super) fn initialize_transaction_log(&self, header: &[u8]) -> Result<u64> {
        if !self.writable {
            return Err(Error::Unsupported("logging on read-only storage".into()));
        }
        if header.is_empty() {
            return Err(Error::Internal("empty transaction log header".into()));
        }
        let current = self
            .file
            .lock()
            .map_err(|_| Error::Internal("checkpoint mutex poisoned".into()))?;
        if checkpoint_transition(&self.path)? || log_present(&self.path, ".wal")? {
            return Err(Error::Transaction(
                "transaction log must be recovered before initialization".into(),
            ));
        }
        let mut staged = self.stage(
            "log-header",
            current.metadata()?.permissions(),
            header,
            [
                PublicationStep::LogInitializeCreate,
                PublicationStep::LogInitializeWrite,
                PublicationStep::LogInitializeSync,
            ],
            false,
        )?;
        self.step(PublicationStep::LogInitializeRename)?;
        std::fs::rename(
            staged
                .path
                .as_ref()
                .ok_or_else(|| Error::Internal("missing staged log header".into()))?,
            sidecar(&self.path, ".wal"),
        )?;
        staged.path = None;
        self.step(PublicationStep::LogInitializeDirectorySync)
            .and_then(|_| sync_parent(&self.path))
            .map_err(|e| Error::CommitUnknown(e.to_string()))?;
        Ok(header.len() as u64)
    }

    pub(super) fn append_transaction_log(&self, expected: u64, bytes: &[u8]) -> Result<u64> {
        if !self.writable {
            return Err(Error::Unsupported("logging on read-only storage".into()));
        }
        if expected == 0 || bytes.is_empty() {
            return Err(Error::Internal(
                "uninitialized or empty transaction log append".into(),
            ));
        }
        let length = expected
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= 512 * 1024 * 1024)
            .ok_or_else(|| Error::Resource("WAL exceeds 512 MiB".into()))?;
        let _current = self
            .file
            .lock()
            .map_err(|_| Error::Internal("checkpoint mutex poisoned".into()))?;
        if checkpoint_transition(&self.path)? {
            return Err(Error::Unsupported(
                "concurrent checkpoint WAL reconciliation".into(),
            ));
        }
        let mut log = OpenOptions::new()
            .read(true)
            .write(true)
            .open(sidecar(&self.path, ".wal"))?;
        if log.metadata()?.len() != expected {
            return Err(Error::Transaction("transaction log length changed".into()));
        }
        log.seek(SeekFrom::Start(expected))?;
        self.step(PublicationStep::LogAppendWrite)?;
        let result = (|| {
            log.write_all(bytes)?;
            self.step(PublicationStep::LogAppendSync)?;
            log.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            let rollback: Result<()> = (|| {
                self.step(PublicationStep::LogRollbackTruncate)?;
                log.set_len(expected)?;
                self.step(PublicationStep::LogRollbackSync)?;
                log.sync_all()?;
                Ok(())
            })();
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback) => Err(Error::CommitUnknown(format!(
                    "log append failed: {error}; rollback failed: {rollback}"
                ))),
            };
        }
        Ok(length)
    }
}
