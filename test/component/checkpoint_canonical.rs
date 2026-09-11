//! A codec's canonical form may differ; SQL equality is still insufficient.
use super::*;
use duckdb_rust::{
    catalog::Catalog,
    common::type_registry::{
        BoundType, KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry, builtin_types,
    },
    storage::{TableStorage, format::FormatId},
};
use std::sync::atomic::AtomicUsize;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn snapshot(
    value: &Value,
    rows: bool,
    default: bool,
    types: Arc<TypeRegistry>,
) -> Result<Snapshot> {
    let name = TableName::main("canonical");
    let mut column = ColumnDefinition::new("v", value.data_type());
    if default {
        column.default = Some(duckdb_rust::catalog::expression::StoredExpression::literal(
            value.data_type(),
            value.clone(),
        ));
    }
    let mut result = Snapshot::new(types.clone());
    result.create_table(
        TableDefinition {
            name: name.clone(),
            columns: vec![column],
            unique_keys: vec![],
        },
        false,
    )?;
    if rows {
        result.insert(
            &name,
            vec![vec![value.clone()], vec![Value::Null]],
            &QueryContext::background().with_types(types),
        )?;
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_canonical_values_preserve_declared_containers_rows_and_empty_table_defaults() -> Result<()>
{
    let query = QueryContext::background();
    let format = DuckDbFormat::default().with_storage_version(69)?;
    let value = Database::memory()?.connect().query("SELECT {'z':1.25::DECIMAL(12,2),'n':'2024-02-29 12:34:56.123456789'::TIMESTAMP_NS,'a':[1,NULL]::INTEGER[2]}::VARIANT")?.rows[0][0].clone();
    for shape in [0, 1, 2, 3, 5, 6, 7, 8] {
        let value = wrapped(value.clone(), shape)?;
        let source = snapshot(&value, true, false, builtin_types())?;
        let decoded = format.decode(format.encode(&source)?, builtin_types())?;
        let layout = CheckpointLayout::identity(&source)?;
        assert!(matches!(
            source.validate_checkpoint_layout(&decoded, &layout, &query),
            Err(Error::Internal(_))
        ));
        source.validate_checkpoint_layout_for(&decoded, &layout, &format, &query)?;
        assert!(matches!(
            source.validate_checkpoint_layout_for(&decoded, &layout, &JsonSnapshotFormat, &query),
            Err(Error::Internal(_))
        ));
        let canonical = decoded.scan(&TableName::main("canonical"), &query)?[0].1[0].clone();
        // Native non-NULL nested DEFAULT encoding remains a separate capability.
        // The shared identity validator must cover their catalog values already,
        // even when no row exists to expose a canonicalization difference.
        for rows in [false, true] {
            let source = snapshot(&value, rows, true, builtin_types())?;
            let decoded = snapshot(&canonical, rows, true, builtin_types())?;
            let layout = CheckpointLayout::identity(&source)?;
            source.validate_checkpoint_layout_for(&decoded, &layout, &format, &query)?;
            assert!(matches!(
                source.validate_checkpoint_layout(&decoded, &layout, &query),
                Err(Error::Internal(_))
            ));
        }
        for fault in 0..4 {
            let mut bad = layout.clone();
            let mapping = bad.tables.get_mut(&TableName::main("canonical")).unwrap();
            match fault {
                0 => {
                    mapping.rows.remove(&0);
                }
                1 => {
                    mapping.rows.insert(1, 0);
                }
                2 => {
                    mapping.rows.insert(0, 99);
                }
                _ => mapping.next_row_id += 1,
            }
            assert!(matches!(
                source.validate_checkpoint_layout_for(&decoded, &bad, &format, &query),
                Err(Error::Internal(_))
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn canonical_layout_rejects_equal_sql_values_with_different_native_content() -> Result<()> {
    let query = QueryContext::background();
    let format = DuckDbFormat::default();
    let mut connection = Database::memory()?.connect();
    let mut pairs = Vec::new();
    for (left, right) in [
        ("1::TINYINT", "1::INTEGER"),
        ("1::DECIMAL(4,0)", "1::DECIMAL(9,0)"),
        ("'-0'::DOUBLE", "'0'::DOUBLE"),
        ("INTERVAL '1 day'", "INTERVAL '24 hours'"),
        ("{'z':1,'a':NULL}", "{'a':NULL,'z':1}"),
        ("{'a':NULL}", "{'A':NULL}"),
        ("{'a':NULL}", "struct_pack()"),
    ] {
        let values = connection.query(&format!("SELECT ({left})::VARIANT,({right})::VARIANT"))?;
        pairs.push((values.rows[0][0].clone(), values.rows[0][1].clone()));
    }
    pairs.push((
        wrapped(Value::Float(f32::from_bits(0x7fc0_1234)), 8)?,
        wrapped(Value::Float(f32::from_bits(0x7fc0_1235)), 8)?,
    ));
    for (left, right) in pairs {
        for default in [false, true] {
            let source = snapshot(&left, !default, default, builtin_types())?;
            let decoded = snapshot(&right, !default, default, builtin_types())?;
            assert!(matches!(
                source.validate_checkpoint_layout_for(
                    &decoded,
                    &CheckpointLayout::identity(&source)?,
                    &format,
                    &query,
                ),
                Err(Error::Internal(_))
            ));
        }
    }
    // Format delegation must not change declared metadata outside VARIANT.
    let value = wrapped(Value::Integer(1), 8)?;
    let source = snapshot(&value, false, true, builtin_types())?;
    for fault in 0..3 {
        let mut definition = source.table(&TableName::main("canonical"))?;
        match fault {
            0 => definition.columns[0].name = "renamed".into(),
            1 => definition.columns[0].nullable = false,
            _ => {
                definition.columns[0].data_type = DataType::Integer;
                definition.columns[0].default =
                    Some(duckdb_rust::catalog::expression::StoredExpression::literal(
                        DataType::Integer,
                        Value::Integer(1),
                    ));
            }
        }
        let mut decoded = Snapshot::default();
        decoded.create_table(definition, false)?;
        assert!(matches!(
            source.validate_checkpoint_layout_for(
                &decoded,
                &CheckpointLayout::identity(&source)?,
                &format,
                &query,
            ),
            Err(Error::Internal(_))
        ));
    }
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
        "checkpoint-exact-selected-integer"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail.load(Ordering::Relaxed) {
            return Err(Error::Resource("selected exact integer failure".into()));
        }
        PrimitiveTypes.validate_value(ty, value, query)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(
        &self,
        _: &DataType,
        _: &Value,
        _: &Value,
        _: &QueryContext,
    ) -> Result<std::cmp::Ordering> {
        panic!("checkpoint equality must not use SQL comparisons")
    }
    fn write_key(
        &self,
        _: &DataType,
        _: &Value,
        _: &mut KeyWriter<'_>,
        _: &QueryContext,
    ) -> Result<()> {
        panic!("checkpoint equality must not use SQL keys")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn canonical_layout_retains_source_services_and_selected_validation_failures() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let mut types = TypeRegistry::builtins();
    types.replace(
        "builtin.integer",
        Arc::new(SelectedInteger {
            calls: calls.clone(),
            fail: fail.clone(),
        }),
    )?;
    let value = wrapped(wrapped(Value::Integer(1), 8)?, 0)?;
    let source = snapshot(&value, true, true, Arc::new(types.clone()))?;
    types.replace(
        "builtin.integer",
        Arc::new(SelectedInteger {
            calls: Arc::new(AtomicUsize::new(0)),
            fail: Arc::new(AtomicBool::new(true)),
        }),
    )?;
    let decoded = snapshot(&value, true, true, builtin_types())?;
    let layout = CheckpointLayout::identity(&source)?;
    let query = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    calls.store(0, Ordering::Relaxed);
    source.validate_checkpoint_layout_for(&decoded, &layout, &DuckDbFormat::default(), &query)?;
    assert!(calls.load(Ordering::Relaxed) >= 4);
    fail.store(true, Ordering::Relaxed);
    assert!(matches!(
        source.validate_checkpoint_layout_for(&decoded, &layout, &DuckDbFormat::default(), &query),
        Err(Error::Resource(message)) if message == "selected exact integer failure"
    ));
    let handle = InterruptHandle::default();
    let interrupted = QueryContext::new(handle.clone(), None, 2, 10)?;
    handle.interrupt();
    assert!(matches!(
        source.validate_checkpoint_layout_for(
            &decoded,
            &layout,
            &DuckDbFormat::default(),
            &interrupted
        ),
        Err(Error::Interrupted)
    ));
    Ok(())
}

struct RejectExact {
    calls: Arc<AtomicUsize>,
    reject_at: Arc<AtomicUsize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotFormat for RejectExact {
    fn name(&self) -> &'static str {
        "reject-selected-exact-layout"
    }
    fn format_id(&self) -> FormatId {
        DuckDbFormat::default().format_id()
    }
    fn decode(&self, bytes: Vec<u8>, types: Arc<TypeRegistry>) -> Result<Snapshot> {
        DuckDbFormat::default().decode(bytes, types)
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        DuckDbFormat::default().encode(snapshot)
    }
    fn supports_successor(&self) -> bool {
        true
    }
    fn encode_successor(
        &self,
        snapshot: &Snapshot,
        previous: &[u8],
    ) -> Result<duckdb_rust::storage::layout::CheckpointImage> {
        DuckDbFormat::default().encode_successor(snapshot, previous)
    }
    fn checkpoint_value_equivalent(
        &self,
        _: &BoundType,
        _: &Value,
        _: &Value,
        _: &QueryContext,
    ) -> Result<Option<bool>> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        if call == self.reject_at.load(Ordering::Relaxed) {
            Err(Error::Resource(
                "selected exact checkpoint rejection".into(),
            ))
        } else {
            Ok(None)
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_exact_rejection_precedes_publication_and_preserves_live_log_session() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("exact_rejected.duckdb");
    seed(&path)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let reject_at = Arc::new(AtomicUsize::new(0));
    let storage = Arc::new(LocalCheckpointStorage::open(
        &path,
        OpenMode::ReadWrite,
        || unreachable!(),
    )?);
    let checkpoint = FileCheckpoint::new(
        storage,
        Arc::new(RejectExact {
            calls: calls.clone(),
            reject_at: reject_at.clone(),
        }),
    )
    .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let wal =
        FileWal::new(checkpoint, Arc::new(DuckDbTransactionLog))?.with_checkpoint_policy(None);
    let transactions = Arc::new(SnapshotTransactions::new(Arc::new(wal))?);
    let database = DatabaseBuilder::new()
        .transactions(transactions.clone())
        .build()?;
    let mut c = database.connect();
    c.execute("INSERT INTO t VALUES(2,'logged'); DELETE FROM t WHERE i=1")?;
    let before = fs::read(&path)?;
    let log = fs::read(path.with_extension("duckdb.wal"))?;
    // Recovery preparation and the subsequent live-logical rebase each compare
    // two cells. Absent defaults are metadata and do not call value equivalence.
    for call in [1, 4] {
        calls.store(0, Ordering::Relaxed);
        reject_at.store(call, Ordering::Relaxed);
        let result = c.checkpoint();
        assert!(
            matches!(&result, Err(Error::Resource(message)) if message == "selected exact checkpoint rejection"),
            "call={call}, observed={}, result={result:?}",
            calls.load(Ordering::Relaxed),
        );
        assert_eq!(calls.load(Ordering::Relaxed), call);
        assert!(fs::read(&path)? == before);
        assert!(fs::read(path.with_extension("duckdb.wal"))? == log);
    }
    assert_eq!(
        c.query("SELECT i FROM t")?.rows,
        vec![vec![Value::Integer(2)]]
    );
    reject_at.store(0, Ordering::Relaxed);
    c.execute("UPDATE t SET i=12 WHERE i=2; CHECKPOINT; UPDATE t SET i=22 WHERE i=12")?;
    drop(c);
    drop(database);
    drop(transactions);
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT i FROM t")?
            .rows,
        vec![vec![Value::Integer(22)]]
    );
    Ok(())
}
