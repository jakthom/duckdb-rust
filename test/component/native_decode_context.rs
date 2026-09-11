//! Real native column decoding, including recursively encoded child streams.
use super::*;
use duckdb_rust::{
    catalog::{
        Catalog,
        expression::{StoredExpression, StoredExpressionEvaluator},
    },
    common::type_registry::TypeRegistry,
    main::settings::{SettingRegistry, SettingsSnapshot},
    parallel::InterruptHandle,
    storage::{
        compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
        duckdb::{
            DuckDbFormat,
            compression::{self, UncompressedDecoder},
        },
        format::SnapshotFormat,
    },
};

struct NoEvaluation;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl StoredExpressionEvaluator for NoEvaluation {
    fn evaluate(
        &self,
        _: &StoredExpression,
        _: &DataType,
        _: &dyn Catalog,
        _: &QueryContext,
    ) -> Result<Value> {
        panic!("column decoding must not evaluate catalog expressions")
    }
}

struct ContextDecoder {
    types: Arc<TypeRegistry>,
    calls: Arc<AtomicUsize>,
    interrupt: Option<InterruptHandle>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SegmentDecoder for ContextDecoder {
    fn id(&self) -> CodecId {
        CodecId(1)
    }
    fn name(&self) -> &'static str {
        "native-context-probe"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        UncompressedDecoder.supports(kind)
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        assert!(Arc::ptr_eq(&self.types, &context.query.type_registry()));
        assert_eq!(
            context
                .query
                .settings()
                .get("default_order", context.query)?,
            &Value::Varchar("DESC".into())
        );
        context.query.stored_expressions()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = UncompressedDecoder.decode(input, context)?;
        if let Some(interrupt) = &self.interrupt {
            interrupt.interrupt();
        }
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn query(interrupt: InterruptHandle, limit: usize) -> Result<QueryContext> {
    let query = QueryContext::new(interrupt, None, 127, limit)?
        .with_types(Arc::new(TypeRegistry::builtins()))
        .with_stored_expressions(Arc::new(NoEvaluation));
    let settings = SettingsSnapshot::new(
        Arc::new(SettingRegistry::builtins()),
        Arc::new([("default_order".into(), Value::Varchar("DESC".into()))].into()),
        Arc::default(),
        &query,
    )?;
    Ok(query.with_settings(settings))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn source(query: &QueryContext, sql: &str) -> Result<Snapshot> {
    let values = Database::memory()?.connect().query(sql)?;
    let mut snapshot = Snapshot::new(query.type_registry());
    let name = TableName::main("mixed");
    snapshot.create_table(
        TableDefinition {
            name: name.clone(),
            columns: values
                .columns
                .iter()
                .map(|column| ColumnDefinition::new(&column.name, column.data_type.clone()))
                .collect(),
            unique_keys: vec![],
        },
        false,
    )?;
    snapshot.insert(&name, values.rows.into_iter().collect(), query)?;
    Ok(snapshot)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn format(
    query: &QueryContext,
    calls: Arc<AtomicUsize>,
    interrupt: Option<InterruptHandle>,
) -> Result<DuckDbFormat> {
    let mut registry = compression::decoders();
    registry.replace(Arc::new(ContextDecoder {
        types: query.type_registry(),
        calls,
        interrupt,
    }))?;
    DuckDbFormat::new(registry).with_storage_version(69)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_descendant_decoders_keep_selected_context_without_evaluation() -> Result<()> {
    let query = query(InterruptHandle::default(), 100_000)?;
    let source = source(
        &query,
        "SELECT 7::UTINYINT AS u, 1.25::DECIMAL(12,2) AS d, TIMESTAMP '2024-02-29 12:34:56' AS t, {'d':1.25::DECIMAL(12,2),'ts':[TIMESTAMP '2024-02-29',NULL]} AS s, {'a':[1,NULL,3]}::VARIANT AS v",
    )?;
    let calls = Arc::new(AtomicUsize::new(0));
    let format = format(&query, calls.clone(), None)?;
    let bytes = format.encode(&source)?;
    let restored = format.decode_with_context(bytes, &query)?;
    source.validate_checkpoint_layout_for(
        &restored,
        &duckdb_rust::storage::layout::CheckpointLayout::identity(&source)?,
        &format,
        &query,
    )?;
    assert!(
        calls.load(Ordering::SeqCst) > 10,
        "nested children and validity must reach the selected decoder"
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_decode_cancellation_and_child_limits_reach_the_caller() -> Result<()> {
    let interrupt = InterruptHandle::default();
    let query = query(interrupt.clone(), 100_000)?;
    let source = source(&query, "SELECT [1,2,3] AS a FROM range(2)")?;
    let calls = Arc::new(AtomicUsize::new(0));
    let format = format(&query, calls.clone(), Some(interrupt.clone()))?;
    let bytes = format.encode(&source)?;
    assert!(matches!(
        format.decode_with_context(bytes.clone(), &query),
        Err(Error::Interrupted)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Pre-cancelled calls do not parse even malformed bytes or invoke adapters.
    assert!(matches!(
        format.decode_with_context(vec![], &query),
        Err(Error::Interrupted)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    interrupt.reset();
    for limit in [1, 2] {
        let limited = QueryContext::new(InterruptHandle::default(), None, 127, limit)?
            .with_types(query.type_registry());
        // Limit 1 rejects the row group; limit 2 reaches the six-element child
        // column. Neither is silently replaced with an unlimited background.
        assert!(matches!(
            DuckDbFormat::default().decode_with_context(bytes.clone(), &limited),
            Err(Error::Resource(_))
        ));
    }
    // The legacy API still explicitly supplies an unlimited maintenance context.
    assert_eq!(
        DuckDbFormat::default()
            .decode(bytes, query.type_registry())?
            .scan(&TableName::main("mixed"), &query)?
            .len(),
        2
    );
    Ok(())
}
