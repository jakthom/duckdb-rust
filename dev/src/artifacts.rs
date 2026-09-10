//! One disposable trace workspace. OS locks protect active writers from cleanup.
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

const MAX_RETAINED: u64 = 256 * 1024 * 1024;

pub fn lease_path(root: &Path) -> PathBuf {
    root.join("target/.dev-traces.lock")
}

fn open_lease(path: &Path) -> io::Result<File> {
    fs::create_dir_all(path.parent().expect("lease directory"))?;
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn busy(error: fs::TryLockError) -> io::Error {
    io::Error::other(format!(
        "trace workspace is in use; finish the active command before replacing it: {error}"
    ))
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn bytes(path: &Path) -> io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            total += bytes(&entry.path())?;
        } else {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

pub fn clean(root: &Path) -> io::Result<()> {
    let path = root.join("target/dev-traces");
    if !path.exists() {
        return Ok(());
    }
    let lease = open_lease(&lease_path(root))?;
    lease.try_lock().map_err(busy)?;
    remove(&path)
}

/// Readers and recording processes hold shared leases until their files close.
pub fn read_lease(root: &Path) -> io::Result<File> {
    let lease = open_lease(&lease_path(root))?;
    lease.try_lock_shared().map_err(busy)?;
    Ok(lease)
}

pub(crate) fn recording_lease() -> io::Result<Option<File>> {
    let Some(path) = std::env::var_os("DUCKDB_DEV_LEASE") else {
        return Ok(None);
    };
    let lease = File::options().read(true).write(true).open(path)?;
    lease.try_lock_shared().map_err(busy)?;
    Ok(Some(lease))
}

pub struct Session {
    directory: PathBuf,
    lease: File,
    remove_on_drop: bool,
    _writer: File,
}

impl Session {
    pub fn begin(root: &Path) -> io::Result<Self> {
        // Serialize sessions across the exclusive-to-shared lease transition.
        // Children and readers can still share the data lease during recording.
        let writer = open_lease(&root.join("target/.dev-traces-writer.lock"))?;
        writer.try_lock().map_err(busy)?;
        let lease = open_lease(&lease_path(root))?;
        lease.try_lock().map_err(busy)?;
        let directory = root.join("target/dev-traces");
        remove(&directory)?;
        lease.unlock()?;
        lease.try_lock_shared().map_err(busy)?;
        Ok(Self {
            directory,
            lease,
            remove_on_drop: true,
            _writer: writer,
        })
    }

    fn remove(&self) -> io::Result<()> {
        self.lease.unlock()?;
        self.lease.try_lock().map_err(busy)?;
        remove(&self.directory)
    }

    pub fn finish(mut self, keep: bool) -> io::Result<()> {
        let oversized = keep && bytes(&self.directory)? > MAX_RETAINED;
        if !keep || oversized {
            self.remove()?;
            eprintln!("dev: temporary telemetry deleted");
        } else {
            eprintln!("dev: retained one trace run; remove after inspection with cargo dev clean");
        }
        self.remove_on_drop = false;
        if oversized {
            return Err(io::Error::other(
                "trace exceeded the 256 MiB retention limit and was deleted; narrow the reproduction",
            ));
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.remove_on_drop
            && let Err(error) = self.remove()
        {
            eprintln!("dev: temporary trace cleanup failed: {error}");
        }
    }
}
