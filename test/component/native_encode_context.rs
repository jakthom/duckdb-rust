use super::*;
use duckdb_rust::storage::{format::CheckpointEncoder, log::Commit};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_fresh_bound_and_recovery_encoders_use_the_request_limits() -> Result<()> {
    let query = query(InterruptHandle::default(), 100_000)?;
    let source = source(
        &query,
        "SELECT i, {'a':[i,NULL]}::VARIANT AS v FROM range(2) t(i)",
    )?;
    let format = DuckDbFormat::default().with_storage_version(69)?;
    let initial = format.encode_with_context(&source, &query)?;
    let bound = format.checkpoint_encoder(&initial)?.unwrap();
    for bytes in [
        bound.encode_with_context(&source, &query)?,
        format
            .encode_successor_with_context(&source, &initial, &query)?
            .bytes,
    ] {
        let restored = format.decode_with_context(bytes, &query)?;
        source.validate_checkpoint_layout_for(
            &restored,
            &duckdb_rust::storage::layout::CheckpointLayout::identity(&source)?,
            &format,
            &query,
        )?;
    }
    let interrupt = InterruptHandle::default();
    let limited = QueryContext::new(interrupt.clone(), None, 1, 1)?;
    for cancelled in [false, true] {
        if cancelled {
            interrupt.interrupt();
        }
        for result in [
            format.encode_with_context(&source, &limited),
            bound.encode_with_context(&source, &limited),
            format
                .encode_successor_with_context(&source, &initial, &limited)
                .map(|image| image.bytes),
        ] {
            assert!(if cancelled {
                matches!(result, Err(Error::Interrupted))
            } else {
                matches!(result, Err(Error::Resource(_)))
            });
        }
    }
    Ok(())
}

struct ContextEncoder {
    calls: Arc<AtomicUsize>,
    interrupt: Option<InterruptHandle>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ContextEncoder {
    fn contextual(&self, snapshot: &Snapshot, query: &QueryContext) -> Result<Vec<u8>> {
        query.check()?;
        assert_eq!(
            query.settings().get("default_order", query)?,
            &Value::Varchar("DESC".into())
        );
        query.stored_expressions()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let bytes = JsonSnapshotFormat.encode(snapshot)?;
        if let Some(interrupt) = &self.interrupt {
            interrupt.interrupt();
        }
        // Deliberately omit a trailing check: publication must independently
        // observe a callback that cancels just before returning prepared bytes.
        Ok(bytes)
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CheckpointEncoder for ContextEncoder {
    fn encode(&self, _: &Snapshot) -> Result<Vec<u8>> {
        Err(Error::Unsupported(
            "legacy bound encoder must not be called".into(),
        ))
    }
    fn encode_with_context(&self, snapshot: &Snapshot, query: &QueryContext) -> Result<Vec<u8>> {
        self.contextual(snapshot, query)
    }
}

struct ContextFormat {
    calls: Arc<AtomicUsize>,
    interrupt: Option<InterruptHandle>,
    stateful: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotFormat for ContextFormat {
    fn name(&self) -> &'static str {
        "selected-encoding-context"
    }
    fn format_id(&self) -> duckdb_rust::storage::format::FormatId {
        JsonSnapshotFormat.format_id()
    }
    fn decode(&self, bytes: Vec<u8>, types: Arc<TypeRegistry>) -> Result<Snapshot> {
        JsonSnapshotFormat.decode(bytes, types)
    }
    fn encode(&self, _: &Snapshot) -> Result<Vec<u8>> {
        Err(Error::Unsupported(
            "legacy stateless encoder must not be called".into(),
        ))
    }
    fn encode_with_context(&self, snapshot: &Snapshot, query: &QueryContext) -> Result<Vec<u8>> {
        assert!(!self.stateful);
        ContextEncoder {
            calls: self.calls.clone(),
            interrupt: self.interrupt.clone(),
        }
        .contextual(snapshot, query)
    }
    fn checkpoint_encoder(&self, _: &[u8]) -> Result<Option<Box<dyn CheckpointEncoder>>> {
        Ok(self.stateful.then(|| {
            Box::new(ContextEncoder {
                calls: self.calls.clone(),
                interrupt: self.interrupt.clone(),
            }) as Box<dyn CheckpointEncoder>
        }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn file_publication_retains_startup_context_and_checks_after_selected_encoding() -> Result<()> {
    for stateful in [false, true] {
        for cancel in [false, true] {
            let interrupt = InterruptHandle::default();
            let query = query(interrupt.clone(), 100_000)?;
            let source = source(&query, "SELECT 1 AS i")?;
            let empty = Snapshot::new(query.type_registry());
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("context.ddb");
            let initial = JsonSnapshotFormat.encode(&empty)?;
            std::fs::write(&path, &initial)?;
            let calls = Arc::new(AtomicUsize::new(0));
            let file = FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(ContextFormat {
                    calls: calls.clone(),
                    interrupt: cancel.then_some(interrupt.clone()),
                    stateful,
                }),
            )?;
            file.load_with_context(&query)?;
            let result = file.publish(Commit {
                before: &empty,
                snapshot: &source,
                changes: None,
            });
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            if cancel {
                assert!(matches!(result, Err(Error::Interrupted)));
                assert_eq!(std::fs::read(&path)?, initial);
                interrupt.reset();
                // Preparation failure leaves the previous encoder usable, not
                // Uncertain; this deliberately cancelling adapter is called again.
                assert!(matches!(
                    file.publish(Commit {
                        before: &empty,
                        snapshot: &source,
                        changes: None
                    }),
                    Err(Error::Interrupted)
                ));
                assert_eq!(calls.load(Ordering::SeqCst), 2);
                assert_eq!(std::fs::read(&path)?, initial);
            } else {
                result?;
                let restored =
                    JsonSnapshotFormat.decode(std::fs::read(&path)?, query.type_registry())?;
                assert_eq!(
                    restored.scan(&TableName::main("mixed"), &query)?[0].1,
                    vec![Value::Integer(1)]
                );
            }
        }
    }
    Ok(())
}

struct LegacyEncoder(Arc<AtomicUsize>, InterruptHandle);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CheckpointEncoder for LegacyEncoder {
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        self.1.interrupt();
        JsonSnapshotFormat.encode(snapshot)
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotFormat for LegacyEncoder {
    fn name(&self) -> &'static str {
        "legacy-encoder"
    }
    fn format_id(&self) -> duckdb_rust::storage::format::FormatId {
        JsonSnapshotFormat.format_id()
    }
    fn decode(&self, bytes: Vec<u8>, types: Arc<TypeRegistry>) -> Result<Snapshot> {
        JsonSnapshotFormat.decode(bytes, types)
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        CheckpointEncoder::encode(self, snapshot)
    }
    fn encode_successor(
        &self,
        snapshot: &Snapshot,
        _: &[u8],
    ) -> Result<duckdb_rust::storage::layout::CheckpointImage> {
        Ok(duckdb_rust::storage::layout::CheckpointImage {
            bytes: CheckpointEncoder::encode(self, snapshot)?,
            layout: duckdb_rust::storage::layout::CheckpointLayout::identity(snapshot)?,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn default_contextual_encoding_preserves_selected_legacy_callbacks_and_cancellation() -> Result<()>
{
    let interrupt = InterruptHandle::default();
    let query = query(interrupt.clone(), 100_000)?;
    let source = Snapshot::new(query.type_registry());
    let calls = Arc::new(AtomicUsize::new(0));
    let legacy = LegacyEncoder(calls.clone(), interrupt.clone());
    for pass in 0..3 {
        interrupt.reset();
        let result = match pass {
            0 => CheckpointEncoder::encode_with_context(&legacy, &source, &query),
            1 => SnapshotFormat::encode_with_context(&legacy, &source, &query),
            _ => legacy
                .encode_successor_with_context(&source, &[], &query)
                .map(|image| image.bytes),
        };
        assert!(matches!(result, Err(Error::Interrupted)));
        assert_eq!(calls.load(Ordering::SeqCst), pass + 1);
    }
    assert!(matches!(
        CheckpointEncoder::encode_with_context(&legacy, &source, &query),
        Err(Error::Interrupted)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    Ok(())
}
