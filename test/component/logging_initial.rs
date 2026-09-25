use super::*;
use duckdb_rust::storage::filesystem::CheckpointStorage;
use std::sync::Arc;

struct InitialFault(PublicationStep);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FileFaultInjector for InitialFault {
    fn before(&self, step: PublicationStep) -> Result<()> {
        if step == self.0 {
            return Err(std::io::Error::other(format!("injected {step:?}")).into());
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn atomic_initial_transaction_installs_one_durable_log() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("initial.duckdb");
    let storage = LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || Ok(vec![1]))?;
    assert_eq!(
        storage.initialize_log_transaction(b"header", b"transaction")?,
        Some(17)
    );
    assert_eq!(
        fs::read(path.with_extension("duckdb.wal"))?,
        b"headertransaction"
    );
    assert!(matches!(
        storage.initialize_log_transaction(b"header", b"next"),
        Err(Error::Transaction(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn atomic_initial_transaction_rejects_empty_parts_without_installing_a_log() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("initial.duckdb");
    let storage = LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || Ok(vec![1]))?;
    for (header, transaction) in [
        (b"".as_slice(), b"transaction".as_slice()),
        (b"header", b""),
    ] {
        assert!(matches!(
            storage.initialize_log_transaction(header, transaction),
            Err(Error::Internal(_))
        ));
        assert!(!path.with_extension("duckdb.wal").exists());
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn atomic_initial_transaction_faults_distinguish_unpublished_and_unknown() -> Result<()> {
    for step in [
        PublicationStep::LogInitializeCreate,
        PublicationStep::LogInitializeWrite,
        PublicationStep::LogInitializeSync,
        PublicationStep::LogInitializeRename,
        PublicationStep::LogInitializeDirectorySync,
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("initial.duckdb");
        let storage = LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || Ok(vec![1]))?
            .with_faults(Arc::new(InitialFault(step)));
        let result = storage.initialize_log_transaction(b"header", b"transaction");
        let log = path.with_extension("duckdb.wal");
        if step == PublicationStep::LogInitializeDirectorySync {
            assert!(matches!(result, Err(Error::CommitUnknown(_))));
            assert_eq!(fs::read(log)?, b"headertransaction");
        } else {
            assert!(!matches!(result, Err(Error::CommitUnknown(_))));
            assert!(!log.exists());
        }
    }
    Ok(())
}
