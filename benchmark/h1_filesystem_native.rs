//! One-operation H1 worker. Fixture creation and validation stay outside the
//! timed child; this binary reports one actual local-I/O operation.
use duckdb_rust::{
    Error, Result, common, parallel,
    storage::filesystem::{
        CheckpointStorage, FileFaultInjector, LocalCheckpointStorage, OpenMode, PublicationStep,
    },
};
use std::{io::Write, path::PathBuf, sync::Arc, time::Instant};
#[path = "../src/storage/filesystem/io.rs"]
mod local_io;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err(Error::Parse(
            "expected operation, file path, and byte count".into(),
        ));
    }
    let operation = args[0].to_string_lossy().into_owned();
    let path = PathBuf::from(&args[1]);
    let size: usize = args[2]
        .to_string_lossy()
        .parse()
        .map_err(|_| Error::Parse("H1 byte count must be an unsigned integer".into()))?;
    if size == 0
        || size > 512 * 1024 * 1024
        || !matches!(
            operation.as_str(),
            "sequential-read" | "positioned-read" | "publication" | "publication-cleanup"
        )
    {
        return Err(Error::Resource("invalid H1 operation or byte count".into()));
    }
    // This fixture transfer is outside elapsed_ns. It is also performed before
    // the comparable C++ operation; parent-side setup/checks are never timed.
    let source = if matches!(operation.as_str(), "publication" | "publication-cleanup") {
        let b = std::fs::read(path.with_extension("seed"))?;
        if b.len() != size {
            return Err(Error::Corrupt(
                "H1 fixture length differs from request".into(),
            ));
        }
        Some(b)
    } else {
        None
    };
    let context = parallel::QueryContext::background();
    let started = Instant::now();
    match operation.as_str() {
        "sequential-read" => {
            let mut file = std::fs::File::open(&path)?;
            let bytes = local_io::read_to_end(&mut file, size, &context)?;
            if bytes.len() != size {
                return Err(Error::Io(std::io::Error::from(
                    std::io::ErrorKind::UnexpectedEof,
                )));
            }
            let elapsed = started.elapsed().as_nanos();
            println!(
                "{{\"engine\":\"rust\",\"operation\":\"{operation}\",\"bytes\":{size},\"elapsed_ns\":{elapsed},\"checksum\":{}}}",
                checksum(&bytes)
            );
            std::io::stdout().flush()?;
            return Ok(());
        }
        "positioned-read" => {
            let mut file = std::fs::File::open(&path)?;
            let bytes = local_io::LocalFileReader::new(&mut file, &context).read_all(size)?;
            if bytes.len() != size {
                return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
            }
            let elapsed = started.elapsed().as_nanos();
            println!(
                "{{\"engine\":\"rust\",\"operation\":\"{operation}\",\"bytes\":{size},\"elapsed_ns\":{elapsed},\"checksum\":{}}}",
                checksum(&bytes)
            );
            std::io::stdout().flush()?;
            return Ok(());
        }
        "publication" => {
            LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || unreachable!())?
                .replace_with_context(source.as_deref().expect("source"), &context)?
        }
        "publication-cleanup" => {
            let storage =
                LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || unreachable!())?
                    .with_faults(Arc::new(CleanupFault));
            if !matches!(
                storage.replace_with_context(source.as_deref().expect("source"), &context),
                Err(Error::Execution(_))
            ) {
                return Err(Error::Execution(
                    "H1 cleanup injector did not reject checkpoint sync".into(),
                ));
            }
        }
        _ => unreachable!(),
    }
    println!(
        "{{\"engine\":\"rust\",\"operation\":\"{operation}\",\"bytes\":{size},\"elapsed_ns\":{}}}",
        started.elapsed().as_nanos()
    );
    std::io::stdout().flush()?;
    Ok(())
}
struct CleanupFault;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FileFaultInjector for CleanupFault {
    fn before(&self, step: PublicationStep) -> Result<()> {
        if step == PublicationStep::CheckpointSync {
            return Err(Error::Execution("H1 staged cleanup injection".into()));
        }
        Ok(())
    }
}
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0_u64, |sum, byte| {
        sum.wrapping_mul(257).wrapping_add(u64::from(*byte))
    })
}
