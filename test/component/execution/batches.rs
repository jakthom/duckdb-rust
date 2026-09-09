use super::*;
use duckdb_rust::{
    common::{type_registry::TypeRegistry, vector::Vector},
    function::{AggregateFunction, AggregateState},
    storage::scan::ScanBatch,
};

#[test]
fn scan_representations_preserve_identities_selection_and_owned_values() -> Result<()> {
    let types: Arc<[DataType]> = vec![DataType::BigInt, DataType::Varchar].into();
    let row = vec![Value::Integer(7), Value::Varchar("retained".into())];
    let context = QueryContext::background();
    let bound = types
        .iter()
        .map(|t| context.types().bind(t))
        .collect::<Result<Vec<_>>>()?;
    let columns = DataChunk::from_rows(&types, std::slice::from_ref(&row))?;
    assert!(ScanBatch::new(vec![], columns.clone()).is_err());
    assert!(ScanBatch::single(41, vec![Value::Null], types.clone()).is_err());
    for batch in [
        ScanBatch::single(41, row.clone(), types.clone())?,
        ScanBatch::new(vec![41], columns)?,
    ] {
        batch.validate(&bound, &context)?;
        assert_eq!(batch.rows().collect::<Vec<_>>(), vec![(41, row.clone())]);
        let mut buffer = vec![Value::Null, Value::Null, Value::Null];
        assert_eq!(batch.read_row(0, &mut buffer)?, &row);
        let prior = buffer.clone();
        assert!(batch.read_row(1, &mut buffer).is_err());
        assert_eq!(buffer, prior);
        let selected = batch.select(&[0, 0])?;
        assert_eq!(
            selected.rows().collect::<Vec<_>>(),
            vec![row.clone(), row.clone()]
        );
        let mut buffer = vec![];
        selected.read_row(1, &mut buffer)?;
        assert_eq!(buffer, row);
    }
    Ok(())
}

#[test]
fn aggregate_batches_match_scalar_updates_for_nulls_encodings_empty_input_and_overflow()
-> Result<()> {
    let functions = FunctionRegistry::builtins();
    let context = QueryContext::background();
    for name in ["count", "sum"] {
        for values in [
            vec![],
            vec![Value::Null; 3],
            ints(&[1, -1, 7]),
            ints(&[i128::MAX, 1]),
        ] {
            let flat = Vector::flat(DataType::HugeInt, values.clone())?;
            let dictionary = Arc::new(flat.clone()).select((0..values.len()).rev().collect())?;
            for column in [
                flat,
                dictionary,
                Vector::constant(DataType::HugeInt, Value::Null, values.len())?,
            ] {
                let batch = DataChunk::new(vec![column], values.len())?;
                let function = functions.aggregate(name).unwrap();
                let mut scalar = function.create_state(&[DataType::HugeInt], context.types())?;
                let mut vector = function.create_state(&[DataType::HugeInt], context.types())?;
                let scalar_result = batch
                    .rows()
                    .try_for_each(|row| scalar.update(&row, &context))
                    .and_then(|_| scalar.finish());
                let batch_result = vector
                    .update_batch(&batch, &context)
                    .and_then(|_| vector.finish());
                assert_eq!(
                    format!("{scalar_result:?}"),
                    format!("{batch_result:?}"),
                    "{name}: {values:?}"
                );
            }
        }
    }
    let count = functions.aggregate("count").unwrap();
    let mut state = count.create_state(&[], context.types())?;
    state.update_batch(&DataChunk::new(vec![], 7)?, &context)?;
    assert_eq!(state.finish()?, Value::Integer(7));
    let handle = InterruptHandle::default();
    let cancelled = QueryContext::new(handle.clone(), None, 3, 20)?;
    handle.interrupt();
    let mut state = count.create_state(&[], context.types())?;
    assert!(matches!(
        state.update_batch(&DataChunk::new(vec![], 7)?, &cancelled),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct RegisteredCount {
    batch: bool,
    calls: Arc<AtomicUsize>,
}
impl AggregateFunction for RegisteredCount {
    fn name(&self) -> &str {
        "registered_count"
    }
    fn return_type(&self, args: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if args.len() != 1 {
            return Err(Error::Bind("one count argument required".into()));
        }
        Ok(DataType::BigInt)
    }
    fn create_state(&self, _: &[DataType], _: &TypeRegistry) -> Result<Box<dyn AggregateState>> {
        let state = RowCount {
            count: 0,
            calls: self.calls.clone(),
        };
        Ok(if self.batch {
            Box::new(ColumnCount(state))
        } else {
            Box::new(state)
        })
    }
}
struct RowCount {
    count: i128,
    calls: Arc<AtomicUsize>,
}
impl AggregateState for RowCount {
    fn update(&mut self, args: &[Value], context: &QueryContext) -> Result<()> {
        context.check()?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.count += i128::from(!args[0].is_null());
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(Value::Integer(self.count))
    }
}
struct ColumnCount(RowCount);
impl AggregateState for ColumnCount {
    fn update(&mut self, _: &[Value], _: &QueryContext) -> Result<()> {
        Err(Error::Internal(
            "column aggregate unexpectedly received a scalar update".into(),
        ))
    }
    fn update_batch(&mut self, args: &DataChunk, context: &QueryContext) -> Result<()> {
        context.check()?;
        self.0.calls.fetch_add(1, Ordering::Relaxed);
        self.0.count += args.columns()[0].values().filter(|v| !v.is_null()).count() as i128;
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(Value::Integer(self.0.count))
    }
}

#[test]
fn registered_aggregate_adapters_receive_the_same_batch_contract() -> Result<()> {
    for batch in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_aggregate(Arc::new(RegisteredCount {
            batch,
            calls: calls.clone(),
        }))?;
        let database = DatabaseBuilder::new()
            .functions(functions)
            .batch_size(2)
            .build()?;
        let mut connection = database.connect();
        connection.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES(1),(NULL),(2),(3)")?;
        assert_eq!(
            connection.query("SELECT registered_count(i) FROM t")?.rows,
            vec![ints(&[3])]
        );
        assert_eq!(calls.load(Ordering::Relaxed), if batch { 2 } else { 4 });
    }
    Ok(())
}
