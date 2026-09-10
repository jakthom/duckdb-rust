//! A separate format proves successor-state dispatch without native type tests.
use super::*;
use duckdb_rust::{
    common::type_registry::TypeRegistry,
    storage::{
        checkpoint::Durability,
        filesystem::CheckpointStorage,
        format::{CheckpointEncoder, FormatId, JsonSnapshotFormat},
        log::Commit,
        table::Snapshot,
    },
};
use std::sync::{Mutex, atomic::AtomicUsize};

struct TestStorage {
    bytes: Mutex<Vec<u8>>,
    reads: AtomicUsize,
    writes: AtomicUsize,
    failure: AtomicUsize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CheckpointStorage for TestStorage {
    fn name(&self) -> &'static str {
        "publication-state-memory-test"
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self) -> Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.bytes.lock().unwrap().clone())
    }
    fn replace(&self, bytes: &[u8]) -> Result<()> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        match self.failure.swap(0, Ordering::Relaxed) {
            1 => Err(Error::Execution("definite publication failure".into())),
            2 => {
                *self.bytes.lock().unwrap() = bytes.to_vec();
                Err(Error::CommitUnknown("uncertain publication failure".into()))
            }
            _ => {
                *self.bytes.lock().unwrap() = bytes.to_vec();
                Ok(())
            }
        }
    }
}

struct SelectedFormat {
    reject_binding: Arc<AtomicBool>,
    encoded: Arc<AtomicUsize>,
}
struct SelectedEncoder {
    generation: u64,
    encoded: Arc<AtomicUsize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn generation(bytes: &[u8]) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(..8)
            .ok_or_else(|| Error::Corrupt("missing test generation".into()))?
            .try_into()
            .unwrap(),
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encode_generation(snapshot: &Snapshot, generation: u64) -> Result<Vec<u8>> {
    let mut bytes = generation.to_le_bytes().to_vec();
    bytes.extend(JsonSnapshotFormat.encode(snapshot)?);
    Ok(bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotFormat for SelectedFormat {
    fn name(&self) -> &'static str {
        "selected-generation-test"
    }
    fn format_id(&self) -> FormatId {
        FormatId("selected-generation-test")
    }
    fn decode(&self, bytes: Vec<u8>, types: Arc<TypeRegistry>) -> Result<Snapshot> {
        generation(&bytes)?;
        JsonSnapshotFormat.decode(bytes[8..].to_vec(), types)
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        encode_generation(snapshot, 0)
    }
    fn checkpoint_encoder(&self, bytes: &[u8]) -> Result<Option<Box<dyn CheckpointEncoder>>> {
        if self.reject_binding.load(Ordering::Relaxed) {
            return Err(Error::Unsupported(
                "selected checkpoint binding rejected".into(),
            ));
        }
        Ok(Some(Box::new(SelectedEncoder {
            generation: generation(bytes)?,
            encoded: self.encoded.clone(),
        })))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CheckpointEncoder for SelectedEncoder {
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        self.encoded.fetch_add(1, Ordering::Relaxed);
        encode_generation(
            snapshot,
            self.generation
                .checked_add(1)
                .ok_or_else(|| Error::Resource("test generation exhausted".into()))?,
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_checkpoint_state_preserves_publication_failures_without_rereading_tables() -> Result<()>
{
    let snapshot = Snapshot::default();
    let reject_binding = Arc::new(AtomicBool::new(false));
    let encoded = Arc::new(AtomicUsize::new(0));
    let format = Arc::new(SelectedFormat {
        reject_binding: reject_binding.clone(),
        encoded: encoded.clone(),
    });
    let storage = Arc::new(TestStorage {
        bytes: Mutex::new(encode_generation(&snapshot, 41)?),
        reads: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
        failure: AtomicUsize::new(0),
    });
    let durability = FileCheckpoint::new(storage.clone(), format.clone());
    let publish = || {
        durability.publish(Commit {
            before: &snapshot,
            snapshot: &snapshot,
            changes: None,
        })
    };
    assert!(matches!(publish(), Err(Error::Internal(_))));
    assert_eq!(storage.writes.load(Ordering::Relaxed), 0);
    durability.load(Arc::new(TypeRegistry::builtins()))?;
    assert!(matches!(
        durability.load(Arc::new(TypeRegistry::builtins())),
        Err(Error::Transaction(_))
    ));
    publish()?;
    assert_eq!(generation(&storage.bytes.lock().unwrap())?, 42);

    // Binding the next image can fail, but no bytes or encoder state advance.
    reject_binding.store(true, Ordering::Relaxed);
    assert!(matches!(publish(), Err(Error::Unsupported(_))));
    assert_eq!(storage.writes.load(Ordering::Relaxed), 1);
    assert_eq!(generation(&storage.bytes.lock().unwrap())?, 42);
    reject_binding.store(false, Ordering::Relaxed);
    storage.failure.store(1, Ordering::Relaxed);
    assert!(matches!(publish(), Err(Error::Execution(_))));
    assert_eq!(generation(&storage.bytes.lock().unwrap())?, 42);
    publish()?;
    assert_eq!(generation(&storage.bytes.lock().unwrap())?, 43);
    assert_eq!(storage.reads.load(Ordering::Relaxed), 1);

    // An uncertain outcome must not reuse the stale encoder on another call.
    storage.failure.store(2, Ordering::Relaxed);
    assert!(matches!(publish(), Err(Error::CommitUnknown(_))));
    let calls = encoded.load(Ordering::Relaxed);
    let writes = storage.writes.load(Ordering::Relaxed);
    assert!(matches!(publish(), Err(Error::CommitUnknown(_))));
    assert_eq!(encoded.load(Ordering::Relaxed), calls);
    assert_eq!(storage.writes.load(Ordering::Relaxed), writes);
    let reopened = FileCheckpoint::new(storage.clone(), format);
    reopened.load(Arc::new(TypeRegistry::builtins()))?;
    reopened.publish(Commit {
        before: &snapshot,
        snapshot: &snapshot,
        changes: None,
    })?;
    assert_eq!(generation(&storage.bytes.lock().unwrap())?, 45);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_checkpoint_generation_survives_typed_sql_and_reopen() -> Result<()> {
    let snapshot = Snapshot::default();
    let format = Arc::new(SelectedFormat {
        reject_binding: Arc::new(AtomicBool::new(false)),
        encoded: Arc::new(AtomicUsize::new(0)),
    });
    let storage = Arc::new(TestStorage {
        bytes: Mutex::new(format.encode(&snapshot)?),
        reads: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
        failure: AtomicUsize::new(0),
    });
    let database = DatabaseBuilder::new()
        .durability(Arc::new(FileCheckpoint::new(
            storage.clone(),
            format.clone(),
        )))
        .build()?;
    let mut c = database.connect();
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,s STRUCT(d DECIMAL(8,2),xs TIMESTAMP_NS[])); INSERT INTO t VALUES(1,{'d':1.25,'xs':[TIMESTAMP_NS '2000-01-01 00:00:00.123456789',NULL]})")?;
    let before = generation(&storage.bytes.lock().unwrap())?;
    c.execute("BEGIN; UPDATE t SET s=NULL; ROLLBACK")?;
    assert_eq!(generation(&storage.bytes.lock().unwrap())?, before);
    let parameter = c.prepare(
        "UPDATE t SET s={'d':$1,'xs':[TIMESTAMP_NS '2001-01-01 00:00:00.987654321']} WHERE id=1",
    )?;
    c.execute_prepared(
        &parameter,
        &[Value::Decimal {
            value: 250,
            width: 8,
            scale: 2,
        }],
    )?;
    let expected = c.query("SELECT id,s.d,s.xs[1] FROM t")?.rows;
    assert_eq!(generation(&storage.bytes.lock().unwrap())?, before + 1);
    assert_eq!(storage.reads.load(Ordering::Relaxed), 1);
    drop(c);
    drop(database);
    let reopened = DatabaseBuilder::new()
        .durability(Arc::new(FileCheckpoint::new(storage, format)))
        .build()?;
    assert_eq!(
        reopened
            .connect()
            .query("SELECT id,s.d,s.xs[1] FROM t")?
            .rows,
        expected
    );
    Ok(())
}
