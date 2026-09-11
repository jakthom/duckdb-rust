use super::*;

/// Drop removes only a temporary file this call successfully created. Once
/// renamed, its former pathname is disarmed before any subsequent fallible work.
pub(super) struct StagedFile {
    pub(super) path: Option<PathBuf>,
    pub(super) file: File,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl LocalCheckpointStorage {
    pub(super) fn stage(
        &self,
        kind: &str,
        permissions: std::fs::Permissions,
        bytes: &[u8],
        steps: [PublicationStep; 3],
        retain_lock: bool,
    ) -> Result<StagedFile> {
        if bytes.len() > 512 * 1024 * 1024 {
            return Err(Error::Resource(
                "checkpoint publication limits each file to 512 MiB".into(),
            ));
        }
        let path = sidecar(
            &self.path,
            &format!(
                ".{kind}-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ),
        );
        self.step(steps[0])?;
        let mut staged = StagedFile {
            file: create_file(&path)?,
            path: Some(path),
        };
        staged.file.set_permissions(permissions)?;
        if retain_lock {
            lock(&staged.file, true)?;
        }
        self.step(steps[1])?;
        staged.file.write_all(bytes)?;
        self.step(steps[2])?;
        staged.file.sync_all()?;
        Ok(staged)
    }

    pub(super) fn stage_checkpoint(&self, current: &File, bytes: &[u8]) -> Result<StagedFile> {
        self.stage(
            "checkpoint",
            current.metadata()?.permissions(),
            bytes,
            [
                PublicationStep::CheckpointCreate,
                PublicationStep::CheckpointWrite,
                PublicationStep::CheckpointSync,
            ],
            true,
        )
    }

    pub(super) fn install_checkpoint(
        &self,
        current: &mut File,
        mut staged: StagedFile,
    ) -> Result<()> {
        self.step(PublicationStep::CheckpointRename)?;
        let path = staged
            .path
            .as_ref()
            .ok_or_else(|| Error::Internal("staged checkpoint already published".into()))?;
        self.lease
            .lock()
            .map_err(|_| Error::Internal("file lease poisoned".into()))?
            .add_identity(&staged.file)?;
        if let Err(error) = std::fs::rename(path, &self.path) {
            self.lease
                .lock()
                .map_err(|_| Error::Internal("file lease poisoned".into()))?
                .retain_identity(current)?;
            return Err(error.into());
        }
        staged.path = None;
        // Swap retains the new inode lock before dropping the old handle.
        std::mem::swap(current, &mut staged.file);
        self.lease
            .lock()
            .map_err(|_| Error::CommitUnknown("file lease poisoned".into()))?
            .retain_identity(current)
            .map_err(uncertain)?;
        self.step(PublicationStep::CheckpointDirectorySync)
            .map_err(uncertain)?;
        sync_parent(&self.path).map_err(uncertain)
    }

    pub(super) fn publish_recovered(
        &self,
        basis: &RecoveryInput,
        publication: &RecoveryPublication,
    ) -> Result<()> {
        if !self.writable {
            return Err(Error::Unsupported(
                "recovery publication on a read-only checkpoint".into(),
            ));
        }
        let mut current = self
            .file
            .lock()
            .map_err(|_| Error::Internal("checkpoint mutex poisoned".into()))?;
        if checkpoint_transition(&self.path)? {
            return Err(Error::Unsupported(
                "concurrent checkpoint WAL reconciliation".into(),
            ));
        }
        let log_path = sidecar(&self.path, ".wal");
        let log_matches = match File::open(&log_path) {
            Ok(mut file) => matches(&mut file, &basis.log)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => basis.log.is_empty(),
            Err(e) => return Err(e.into()),
        };
        if !matches(&mut current, &basis.checkpoint)? || !log_matches {
            return Err(Error::Transaction(
                "recovery input changed after preparation".into(),
            ));
        }
        match publication {
            RecoveryPublication::Replace {
                checkpoint,
                bridge_log,
            } => {
                if checkpoint.is_empty() || bridge_log.is_empty() {
                    return Err(Error::Internal(
                        "empty recovery checkpoint or bridge log".into(),
                    ));
                }
                // Both replacements are completely written and synced before
                // exposing a bridge or changing the database pathname.
                let staged = self.stage_checkpoint(&current, checkpoint)?;
                let permissions = match std::fs::metadata(&log_path) {
                    Ok(metadata) => metadata.permissions(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        current.metadata()?.permissions()
                    }
                    Err(e) => return Err(e.into()),
                };
                let mut log = self.stage(
                    "recovery-log",
                    permissions,
                    bridge_log,
                    [
                        PublicationStep::RecoveryLogCreate,
                        PublicationStep::RecoveryLogWrite,
                        PublicationStep::RecoveryLogSync,
                    ],
                    false,
                )?;
                self.step(PublicationStep::RecoveryLogRename)?;
                std::fs::rename(
                    log.path
                        .as_ref()
                        .ok_or_else(|| Error::Internal("missing staged log".into()))?,
                    &log_path,
                )?;
                log.path = None;
                self.step(PublicationStep::RecoveryLogDirectorySync)?;
                sync_parent(&self.path)?;
                self.install_checkpoint(&mut current, staged)?;
                self.retire_log(&log_path).map_err(uncertain)
            }
            RecoveryPublication::RetireLog => {
                // A previous attempt may have renamed this checkpoint without
                // syncing its directory. Establish that durability now before
                // removing the only recovery evidence for that publication.
                self.step(PublicationStep::CurrentCheckpointSync)?;
                current.sync_all()?;
                self.step(PublicationStep::CheckpointDirectorySync)?;
                sync_parent(&self.path)?;
                self.retire_log(&log_path)
            }
        }
    }

    fn retire_log(&self, path: &Path) -> Result<()> {
        self.step(PublicationStep::LogRemove)?;
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.step(PublicationStep::LogRetirementDirectorySync)
            .map_err(uncertain)?;
        sync_parent(&self.path).map_err(uncertain)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn uncertain(error: Error) -> Error {
    match error {
        Error::CommitUnknown(_) => error,
        other => Error::CommitUnknown(other.to_string()),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn matches(file: &mut File, expected: &[u8]) -> Result<bool> {
    if file.metadata()?.len() != expected.len() as u64 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut buffer = [0; 65536];
    for expected in expected.chunks(buffer.len()) {
        let bytes = &mut buffer[..expected.len()];
        file.read_exact(bytes)?;
        if bytes != expected {
            return Ok(false);
        }
    }
    Ok(true)
}
