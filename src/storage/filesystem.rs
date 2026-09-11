use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use super::recovery::{RecoveryInput, RecoveryPublication};
use crate::common::{Error, Result};

mod log;
mod publication;

/// Named I/O boundaries of local checkpoint publication. Injection runs before
/// the named operation, under the publication lock. It must not reenter this
/// storage. Errors follow the same definite/uncertain rules as real I/O errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationStep {
    CheckpointCreate,
    CheckpointWrite,
    CheckpointSync,
    RecoveryLogCreate,
    RecoveryLogWrite,
    RecoveryLogSync,
    RecoveryLogRename,
    RecoveryLogDirectorySync,
    CheckpointRename,
    CheckpointDirectorySync,
    CurrentCheckpointSync,
    LogRemove,
    LogRetirementDirectorySync,
    LogInitializeCreate,
    LogInitializeWrite,
    LogInitializeSync,
    LogInitializeRename,
    LogInitializeDirectorySync,
    LogAppendWrite,
    LogAppendSync,
    LogRollbackTruncate,
    LogRollbackSync,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait FileFaultInjector: Send + Sync {
    fn before(&self, step: PublicationStep) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    ReadOnly,
    ReadWrite,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// A leased, atomically replaceable object. Reads own their bytes. Publication
/// is serialized and durable before success; failures after visibility must be
/// CommitUnknown. Implementations retain exclusive writer ownership until drop.
pub trait CheckpointStorage: Send + Sync {
    fn name(&self) -> &'static str;
    fn writable(&self) -> bool;
    fn read(&self) -> Result<Vec<u8>>;
    /// Read the log under the checkpoint's retained lease. Adapters without a
    /// log return empty bytes. Unsupported multi-file recovery states must fail.
    fn read_log(&self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
    fn replace(&self, bytes: &[u8]) -> Result<()>;
    fn supports_recovery_publication(&self) -> bool {
        false
    }
    fn supports_log_append(&self) -> bool {
        false
    }
    /// Publish a complete, durable log header before any transaction append.
    /// A nonempty existing log is rejected. Header creation must be atomic so
    /// interruption cannot leave an incomplete version header at the log path.
    fn initialize_log(&self, _header: &[u8]) -> Result<u64> {
        Err(Error::Unsupported(
            "transaction logging on this storage".into(),
        ))
    }
    /// Append one complete encoded transaction to the expected log length.
    /// Serialize under the retained writer lease, sync before success, and
    /// return the new durable length. Failures either durably restore the old
    /// length or return CommitUnknown; callers must then stop writing.
    fn append_log(&self, _expected: u64, _bytes: &[u8]) -> Result<u64> {
        Err(Error::Unsupported(
            "transaction logging on this storage".into(),
        ))
    }
    /// Compare the prepared input under the publication lock, then execute the
    /// bridge protocol. A stale basis fails before mutation. Successful return
    /// makes the checkpoint durable and retires the log durably. Failures after
    /// checkpoint replacement or log removal are CommitUnknown, never retryable
    /// as an ordinary mutation. The lease remains held through the whole call.
    fn publish_recovery(
        &self,
        _basis: &RecoveryInput,
        _publication: &RecoveryPublication,
    ) -> Result<()> {
        Err(Error::Unsupported(
            "recovery publication on this storage".into(),
        ))
    }
}

static OPEN_FILES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Keeps process-associated fcntl locks from being dropped by another engine
/// instance opening and closing the same file. Connections share one instance.
struct Lease {
    keys: Vec<String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Lease {
    fn acquire(path: &Path) -> Result<Self> {
        let mut keys = vec![format!("path:{}", path.display())];
        #[cfg(unix)]
        if let Ok(meta) = std::fs::metadata(path) {
            use std::os::unix::fs::MetadataExt;
            keys.push(format!("inode:{}:{}", meta.dev(), meta.ino()));
        }
        let mut files = OPEN_FILES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| Error::Internal("file registry poisoned".into()))?;
        if keys.iter().any(|key| files.contains(key)) {
            return Err(Error::Transaction(
                "database is already open in this process; share its Database handle".into(),
            ));
        }
        files.extend(keys.clone());
        Ok(Self { keys })
    }
    fn add_identity(&mut self, file: &File) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = file.metadata()?;
            let key = format!("inode:{}:{}", meta.dev(), meta.ino());
            if !self.keys.contains(&key) {
                OPEN_FILES
                    .get_or_init(Default::default)
                    .lock()
                    .map_err(|_| Error::Internal("file registry poisoned".into()))?
                    .insert(key.clone());
                self.keys.push(key);
            }
        }
        #[cfg(not(unix))]
        let _ = file;
        Ok(())
    }
    fn retain_identity(&mut self, file: &File) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = file.metadata()?;
            let identity = format!("inode:{}:{}", meta.dev(), meta.ino());
            let mut files = OPEN_FILES
                .get_or_init(Default::default)
                .lock()
                .map_err(|_| Error::Internal("file registry poisoned".into()))?;
            self.keys.retain(|key| {
                if key.starts_with("inode:") && *key != identity {
                    files.remove(key);
                    false
                } else {
                    true
                }
            });
        }
        #[cfg(not(unix))]
        let _ = file;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut files) = OPEN_FILES.get_or_init(Default::default).lock() {
            for key in &self.keys {
                files.remove(key);
            }
        }
    }
}

pub struct LocalCheckpointStorage {
    file: Mutex<File>,
    lease: Mutex<Lease>,
    path: PathBuf,
    writable: bool,
    faults: Option<std::sync::Arc<dyn FileFaultInjector>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl LocalCheckpointStorage {
    pub fn open(
        path: &Path,
        mode: OpenMode,
        initial: impl FnOnce() -> Result<Vec<u8>>,
    ) -> Result<Self> {
        let writable = mode == OpenMode::ReadWrite;
        let path = if path.exists() {
            std::fs::canonicalize(path)?
        } else {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::canonicalize(parent)?.join(path.file_name().ok_or_else(|| {
                Error::Io(std::io::Error::other("database path needs a filename"))
            })?)
        };
        #[cfg(unix)]
        if writable && let Ok(meta) = std::fs::metadata(&path) {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() > 1 {
                return Err(Error::Unsupported(
                    "atomic checkpoint publication on a hard-linked file".into(),
                ));
            }
        }
        if !path.exists() && (log_present(&path, ".wal")? || checkpoint_transition(&path)?) {
            return Err(Error::Corrupt("WAL exists without its checkpoint".into()));
        }
        let mut lease = Lease::acquire(&path)?;
        let (mut file, created) = if writable {
            match create_file(&path) {
                Ok(file) => (file, true),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (
                    OpenOptions::new().read(true).write(true).open(&path)?,
                    false,
                ),
                Err(e) => return Err(e.into()),
            }
        } else {
            (File::open(&path)?, false)
        };
        lock(&file, writable)?;
        lease.add_identity(&file)?;
        if checkpoint_transition(&path)? {
            return Err(Error::Unsupported(
                "concurrent checkpoint WAL reconciliation".into(),
            ));
        }
        if created {
            let result = (|| {
                file.write_all(&initial()?)?;
                file.sync_all()?;
                sync_parent(&path)
            })();
            if let Err(error) = result {
                let _ = std::fs::remove_file(&path);
                return Err(error);
            }
        }
        Ok(Self {
            file: Mutex::new(file),
            lease: Mutex::new(lease),
            path,
            writable,
            faults: None,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn with_faults(mut self, faults: std::sync::Arc<dyn FileFaultInjector>) -> Self {
        self.faults = Some(faults);
        self
    }
    fn step(&self, step: PublicationStep) -> Result<()> {
        if let Some(faults) = &self.faults {
            faults.before(step)?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CheckpointStorage for LocalCheckpointStorage {
    fn name(&self) -> &'static str {
        "local-atomic-checkpoint"
    }
    fn writable(&self) -> bool {
        self.writable
    }
    fn read(&self) -> Result<Vec<u8>> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| Error::Internal("checkpoint mutex poisoned".into()))?;
        if file.metadata()?.len() > 512 * 1024 * 1024 {
            return Err(Error::Resource(
                "checkpoint reader limits input to 512 MiB".into(),
            ));
        }
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        (&mut *file)
            .take(512 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    fn read_log(&self) -> Result<Vec<u8>> {
        match File::open(sidecar(&self.path, ".wal")) {
            Ok(file) => {
                if file.metadata()?.len() > 512 * 1024 * 1024 {
                    return Err(Error::Resource("WAL reader limits input to 512 MiB".into()));
                }
                let mut bytes = Vec::new();
                file.take(512 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 512 * 1024 * 1024 {
                    return Err(Error::Resource("WAL reader limits input to 512 MiB".into()));
                }
                Ok(bytes)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }
    fn replace(&self, bytes: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(Error::Unsupported("writing a read-only checkpoint".into()));
        }
        let mut current = self
            .file
            .lock()
            .map_err(|_| Error::Internal("checkpoint mutex poisoned".into()))?;
        if checkpoint_transition(&self.path)? || log_present(&self.path, ".wal")? {
            return Err(Error::Unsupported(
                "recover the active log before checkpoint publication".into(),
            ));
        }
        let staged = self.stage_checkpoint(&current, bytes)?;
        self.install_checkpoint(&mut current, staged)
    }
    fn supports_recovery_publication(&self) -> bool {
        self.writable
    }
    fn supports_log_append(&self) -> bool {
        self.writable
    }
    fn initialize_log(&self, header: &[u8]) -> Result<u64> {
        self.initialize_transaction_log(header)
    }
    fn append_log(&self, expected: u64, bytes: &[u8]) -> Result<u64> {
        self.append_transaction_log(expected, bytes)
    }
    fn publish_recovery(
        &self,
        basis: &RecoveryInput,
        publication: &RecoveryPublication,
    ) -> Result<()> {
        self.publish_recovered(basis, publication)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn lock(file: &File, writable: bool) -> Result<()> {
    #[cfg(not(unix))]
    {
        let local = if writable {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        local.map_err(|e| Error::Transaction(format!("database file is locked: {e}")))?;
    }
    #[cfg(unix)]
    {
        use rustix::fs::{FlockOperation, fcntl_lock};
        fcntl_lock(
            file,
            if writable {
                FlockOperation::NonBlockingLockExclusive
            } else {
                FlockOperation::NonBlockingLockShared
            },
        )
        .map_err(|e| Error::Transaction(format!("database file is locked: {e}")))?;
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sync_parent(path: &Path) -> Result<()> {
    File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn create_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut result = path.as_os_str().to_os_string();
    result.push(suffix);
    result.into()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn log_present(path: &Path, suffix: &str) -> Result<bool> {
    match std::fs::metadata(sidecar(path, suffix)) {
        Ok(metadata) => Ok(metadata.len() > 0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn checkpoint_transition(path: &Path) -> Result<bool> {
    // Existence carries protocol state even before the first record is written.
    Ok(sidecar(path, ".wal.checkpoint").try_exists()?
        || sidecar(path, ".wal.recovery").try_exists()?)
}
