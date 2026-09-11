use super::*;
use crate::{
    catalog::{CatalogMut, ColumnDefinition, TableDefinition, TableName},
    common::{
        NestedPayload, NestedValue, Value,
        type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry},
    },
    parallel::{InterruptHandle, QueryContext},
    storage::{
        TableStorageMut,
        log::{TransactionChange, TransactionLog},
    },
};
use std::{
    cmp::Ordering,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn write_versions_separate_raw_headers_recursive_table_gates_and_log_capabilities() -> Result<()> {
    for version in 64..=69 {
        let format = DuckDbFormat::default().with_storage_version(version)?;
        let bytes = format.encode(&Snapshot::default())?;
        let identity = CheckpointIdentity::read(&bytes)?;
        assert_eq!(identity.storage_version(), version);
        assert_eq!(
            format
                .checkpoint_encoder(&bytes)?
                .unwrap()
                .storage_version(),
            Some(crate::storage::format::StorageVersion {
                format: crate::storage::format::DUCKDB_FORMAT,
                version,
            })
        );
        assert_eq!(
            (identity.main_version, identity.database_version),
            new_headers(version)?
        );
        for (ty, required) in [
            (NestedType::Variant.data_type(), 68),
            (NestedType::Tuple(vec![]).data_type(), 69),
            (NestedType::Tuple(vec![DataType::Integer]).data_type(), 69),
            (NestedType::Struct(vec![]).data_type(), 69),
        ] {
            for ty in [
                ty.clone(),
                NestedType::List(ty.clone()).data_type(),
                NestedType::Struct(vec![("child".into(), ty)]).data_type(),
            ] {
                assert_eq!(checkpoint_type(&ty, version).is_ok(), version >= required);
                assert!(matches!(wal_type(&ty), Err(Error::Unsupported(_))));
                assert_eq!(
                    wal_type_at(&ty, Some(version)).is_ok(),
                    required == 69 && version == 69
                );
                let name = TableName::main("t");
                let mut snapshot = Snapshot::default();
                snapshot.create_table(
                    TableDefinition {
                        name,
                        columns: vec![ColumnDefinition::new("v", ty.clone())],
                        unique_keys: vec![],
                    },
                    false,
                )?;
                let encoded = format.encode(&snapshot);
                assert_eq!(encoded.is_ok(), version >= required, "{version}: {ty}");
                if let Ok(encoded) = encoded {
                    let restored = format.decode(encoded, snapshot.type_registry())?;
                    assert_eq!(
                        crate::catalog::Catalog::tables(&restored)?[0].columns[0].data_type,
                        ty
                    );
                    if required == 68 {
                        assert!(matches!(
                            format.encode_successor(&snapshot, &bytes),
                            Err(Error::Unsupported(_))
                        ));
                    }
                }
                assert!(matches!(
                    wal::writer::DuckDbTransactionLog.start(&snapshot, &QueryContext::background()),
                    Err(Error::Unsupported(_))
                ));
            }
        }
    }
    for version in [0, 63, 70, 999, u64::MAX] {
        assert!(matches!(
            DuckDbFormat::default().with_storage_version(version),
            Err(Error::Unsupported(_))
        ));
    }
    // Even a successful metadata-only session must reject unsupported ALTER/CREATE.
    let session = wal::writer::DuckDbTransactionLog
        .start(&Snapshot::default(), &QueryContext::background())?
        .session;
    assert!(matches!(
        session.prepare(
            &[TransactionChange::CreateTable(TableDefinition {
                name: TableName::main("empty"),
                columns: vec![ColumnDefinition::new(
                    "v",
                    NestedType::List(NestedType::Variant.data_type()).data_type()
                )],
                unique_keys: vec![]
            })],
            &QueryContext::background()
        ),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn log_storage_compatibility_is_validated_and_cannot_change_during_rebase() -> Result<()> {
    use crate::storage::{
        format::{DUCKDB_FORMAT, JSON_FORMAT, StorageVersion},
        layout::CheckpointLayout,
        log::LogCheckpoint,
    };
    let log = wal::writer::DuckDbTransactionLog;
    let snapshot = Snapshot::default();
    let query = QueryContext::background();
    for (format, version) in [(JSON_FORMAT, 69), (DUCKDB_FORMAT, 999), (DUCKDB_FORMAT, 0)] {
        assert!(matches!(
            log.start_at(&snapshot, Some(StorageVersion { format, version }), &query),
            Err(Error::Unsupported(_))
        ));
    }
    let session = log
        .start_at(
            &snapshot,
            Some(StorageVersion {
                format: DUCKDB_FORMAT,
                version: 68,
            }),
            &query,
        )?
        .session;
    let format = DuckDbFormat::default().with_storage_version(69)?;
    let bytes = format.encode(&snapshot)?;
    let layout = CheckpointLayout::identity(&snapshot)?;
    assert!(matches!(
        session.rebase(
            LogCheckpoint {
                format: &format,
                logical: &snapshot,
                physical: &snapshot,
                bytes: &bytes,
                layout: &layout,
            },
            &query
        ),
        Err(Error::Internal(_))
    ));
    let correct = DuckDbFormat::default()
        .with_storage_version(68)?
        .encode(&snapshot)?;
    session.rebase(
        LogCheckpoint {
            format: &format,
            logical: &snapshot,
            physical: &snapshot,
            bytes: &correct,
            layout: &layout,
        },
        &query,
    )?;
    Ok(())
}

#[derive(Debug)]
struct SelectedInteger {
    calls: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for SelectedInteger {
    fn name(&self) -> &'static str {
        "native-writer-selected-integer"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        self.calls.fetch_add(1, AtomicOrdering::Relaxed);
        if self.fail.load(AtomicOrdering::Relaxed) {
            return Err(Error::Resource("selected native child failed".into()));
        }
        PrimitiveTypes.validate_value(ty, value, query)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(
        &self,
        ty: &DataType,
        a: &Value,
        b: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        PrimitiveTypes.compare(ty, a, b, query)
    }
    fn write_key(
        &self,
        ty: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(ty, value, output, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_writer_retains_snapshot_services_through_variant_statistics_and_children() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let mut types = TypeRegistry::builtins();
    types.replace(
        DataType::Integer.family(),
        Arc::new(SelectedInteger {
            calls: calls.clone(),
            fail: fail.clone(),
        }),
    )?;
    let types = Arc::new(types);
    let mut snapshot = Snapshot::new(types.clone());
    assert!(Arc::ptr_eq(&types, &snapshot.type_registry()));
    let ty = NestedType::Variant.data_type();
    let value = NestedValue::value(
        ty.clone(),
        NestedPayload::Variant {
            data_type: DataType::Integer,
            value: Value::Integer(42),
        },
    )?;
    let name = TableName::main("t");
    snapshot.create_table(
        TableDefinition {
            name: name.clone(),
            columns: vec![ColumnDefinition::new("v", ty)],
            unique_keys: vec![],
        },
        false,
    )?;
    snapshot.insert(&name, vec![vec![value]], &QueryContext::background())?;
    calls.store(0, AtomicOrdering::Relaxed);
    // Enter the actual writer below SnapshotFormat's whole-snapshot validation.
    // A builtin-context fallback would miss this retained logical validator.
    writer::encode_version(&snapshot, 68)?;
    assert!(calls.load(AtomicOrdering::Relaxed) >= 2);
    fail.store(true, AtomicOrdering::Relaxed);
    assert!(matches!(
        writer::encode_version(&snapshot, 68),
        Err(Error::Resource(_))
    ));
    fail.store(false, AtomicOrdering::Relaxed);
    writer::encode_version(&snapshot, 68)?;

    let interrupt = InterruptHandle::default();
    let context = QueryContext::new(interrupt.clone(), None, 2, 10)?.with_types(types);
    interrupt.interrupt();
    let mut output = binary::Encoder::default();
    assert!(matches!(
        writer::statistics(
            &mut output,
            Some(&NestedType::Variant.data_type()),
            &[],
            &context
        ),
        Err(Error::Interrupted)
    ));
    assert!(output.0.is_empty());
    Ok(())
}
