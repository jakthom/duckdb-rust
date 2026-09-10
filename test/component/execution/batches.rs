use super::*;
use duckdb_rust::{
    catalog::{CatalogMut, ColumnDefinition, TableDefinition, TableName},
    common::{RowCollection, type_registry::TypeRegistry, vector::Vector},
    function::{AggregateFunction, AggregateState},
    storage::{TableStorage, TableStorageMut, scan::ScanBatch, table::Snapshot},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn materialized_row_collections_own_values_and_preserve_zero_width_rows() -> Result<()> {
    for width in 0..=4 {
        for count in [0, 1, 7] {
            let expected: Vec<Row> = (0..count)
                .map(|row| {
                    (0..width)
                        .map(|column| {
                            if (row + column) % 3 == 0 {
                                Value::Null
                            } else {
                                Value::Varchar(format!("{row}:{column}"))
                            }
                        })
                        .collect()
                })
                .collect();
            let chunk = DataChunk::from_rows(&vec![DataType::Varchar; width], &expected)?;
            let mut rows = RowCollection::new(width);
            let split = count / 2;
            rows.append(&chunk.slice(0, split)?)?;
            rows.append(&chunk.slice(split, count - split)?)?;
            drop(chunk);
            assert_eq!(rows.width(), width);
            assert_eq!(rows.len(), count);
            assert_eq!(rows, expected);
            assert_eq!(expected, rows);
            assert_eq!(rows.iter().len(), count);
            assert_eq!(
                rows.iter().rev().collect::<Vec<_>>(),
                expected.iter().rev().map(Vec::as_slice).collect::<Vec<_>>()
            );
            for (index, expected) in expected.iter().enumerate() {
                assert_eq!(&rows[index], expected);
            }
            assert!(rows.get(count).is_none());
            assert!(rows.get(usize::MAX).is_none());
            assert_eq!(
                serde_json::to_value(&rows).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            assert_eq!(rows.clone().into_rows(), expected);
            assert_eq!(RowCollection::from_rows(width, expected.clone())?, rows);
            let wrong_width = DataChunk::new(
                vec![Vector::constant(DataType::Null, Value::Null, 1)?; width + 1],
                1,
            )?;
            assert!(rows.append(&wrong_width).is_err());
            assert_eq!(rows, expected);
        }
    }
    assert!(RowCollection::from_rows(1, vec![vec![], ints(&[1])]).is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn contiguous_views_preserve_nested_selection_nulls_and_empty_cardinality() -> Result<()> {
    let flat = Vector::flat(
        DataType::BigInt,
        vec![Value::Null, Value::Integer(7), Value::Integer(9)],
    )?;
    let selected = Arc::new(flat.clone()).select(vec![2, 0, 1, 2])?;
    for column in [
        flat,
        selected,
        Vector::constant(DataType::BigInt, Value::Null, 4)?,
    ] {
        let expected: Vec<_> = column.values().skip(1).take(2).cloned().collect();
        let view = column.slice(1, 2)?;
        assert_eq!(view.values().cloned().collect::<Vec<_>>(), expected);
        assert_eq!(view.slice(1, 1)?.get(0), expected.get(1));
        assert!(view.slice(2, 0)?.is_empty());
        assert!(view.get(2).is_none());
        assert!(view.slice(2, 1).is_err());
        assert!(view.slice(usize::MAX, 0).is_err());
        let retained = Arc::new(view).select(vec![1, 0, 1])?;
        drop(column);
        assert_eq!(
            retained.values().cloned().collect::<Vec<_>>(),
            vec![
                expected[1].clone(),
                expected[0].clone(),
                expected[1].clone()
            ]
        );
        let expected = retained.values().cloned().collect::<Vec<_>>();
        let mut appended = vec![Value::Null];
        retained.append_to(&mut appended);
        assert_eq!(&appended[1..], expected);
        let nested = Arc::new(retained).select(vec![2, 0, 1])?.slice(1, 2)?;
        let mut copied = Vec::new();
        nested.append_to(&mut copied);
        assert_eq!(copied, nested.values().cloned().collect::<Vec<_>>());
    }
    let empty_width = DataChunk::new(vec![], 9)?;
    assert_eq!(
        empty_width.slice(3, 5)?.rows().collect::<Vec<_>>(),
        vec![vec![]; 5]
    );
    assert!(empty_width.slice(8, 2).is_err());
    let nullable = Vector::flat(DataType::BigInt, vec![Value::Null, Value::Integer(1)])?;
    assert!(!nullable.all_valid());
    assert!(!nullable.slice(1, 1)?.all_valid());
    assert!(nullable.slice(1, 0)?.all_valid());
    let valid = Vector::flat(DataType::BigInt, ints(&[1, 2, 3]))?;
    assert!(valid.all_valid());
    assert!(
        Arc::new(valid.slice(1, 2)?)
            .select(vec![1, 0, 1])?
            .all_valid()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_integer_columns_establish_physical_validity_and_stop_on_errors() -> Result<()> {
    let values = [Some(i64::MIN), None, Some(0), Some(i64::MAX)];
    let column = Vector::try_bigints(values.into_iter().map(Ok))?;
    assert_eq!(column.data_type(), &DataType::BigInt);
    assert!(!column.all_valid());
    assert_eq!(
        column.values().cloned().collect::<Vec<_>>(),
        vec![
            Value::Integer(i64::MIN as i128),
            Value::Null,
            Value::Integer(0),
            Value::Integer(i64::MAX as i128)
        ]
    );
    let valid = Vector::try_bigints([Ok(Some(i64::MIN)), Ok(Some(i64::MAX))])?;
    assert!(valid.all_valid());
    let mut calls = 0;
    let failed = Vector::try_bigints((0..10).map(|value| {
        calls += 1;
        if value == 2 {
            Err(Error::Interrupted)
        } else {
            Ok(Some(value))
        }
    }));
    assert!(matches!(failed, Err(Error::Interrupted)));
    assert_eq!(calls, 3);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn published_columns_preserve_holes_writes_restart_and_retained_batches() -> Result<()> {
    let context = QueryContext::background();
    let table = TableName::main("owned_columns");
    let mut snapshot = Snapshot::default();
    snapshot.create_table(
        TableDefinition {
            name: table.clone(),
            columns: vec![
                ColumnDefinition::new("i", DataType::BigInt),
                ColumnDefinition::new("v", DataType::Varchar),
            ],
            unique_keys: vec![],
        },
        false,
    )?;
    let row = |i: i128| {
        vec![
            Value::Integer(i),
            if i % 3 == 0 {
                Value::Null
            } else {
                Value::Varchar(format!("row {i}"))
            },
        ]
    };
    let expected: Vec<_> = (0..4097).map(|i| (i as u64, row(i))).collect();
    snapshot.insert(
        &table,
        expected.iter().map(|(_, row)| row.clone()).collect(),
        &context,
    )?;
    let before = snapshot.clone();
    let mut scan = before.open_scan(&table)?;
    let first = scan.next(1, &context)?.unwrap();
    let bulk = scan.next(3000, &context)?.unwrap();
    assert_eq!(first.rows().collect::<Vec<_>>(), expected[..1]);
    assert_eq!(bulk.rows().collect::<Vec<_>>(), expected[1..3001]);
    snapshot.delete(&table, &[0, 2048, 2048, 4096], &context)?;
    snapshot.update(&table, vec![(1, row(-1))], &context)?;
    snapshot.insert(&table, vec![row(5000)], &context)?;
    assert!(matches!(
        snapshot.update(
            &table,
            vec![(1, vec![Value::Varchar("invalid".into()), Value::Null])],
            &context
        ),
        Err(Error::Constraint(_))
    ));
    let remaining = scan.next(2048, &context)?.unwrap();
    assert_eq!(remaining.rows().collect::<Vec<_>>(), expected[3001..]);
    assert!(scan.next(1, &context)?.is_none());
    assert!(scan.next(2048, &context)?.is_none());
    drop(scan);
    drop(before);
    assert_eq!(bulk.rows().collect::<Vec<_>>(), expected[1..3001]);
    assert_eq!(
        snapshot.fetch(&table, &[0, 1, 4097, 1, 2048], &context)?,
        vec![None, Some(row(-1)), Some(row(5000)), Some(row(-1)), None]
    );
    let decoded: Snapshot =
        serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let actual = snapshot.scan(&table, &context)?;
    assert_eq!(decoded.scan(&table, &context)?, actual);
    assert_eq!(decoded.next_row_id(&table)?, 4098);
    for demand in [1, 3, 2048, 5000] {
        let mut cursor = decoded.open_scan(&table)?;
        let mut rows = Vec::new();
        while let Some(batch) = cursor.next(demand, &context)? {
            assert!(!batch.is_empty() && batch.len() <= demand);
            rows.extend(batch.rows());
        }
        assert_eq!(rows, actual);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
    let mut ids: Arc<[_]> = vec![11, 41, 99].into();
    assert!(ScanBatch::shared(ids.clone(), 3, columns.clone()).is_err());
    assert!(ScanBatch::shared(ids.clone(), usize::MAX, columns.clone()).is_err());
    let shared = ScanBatch::shared(ids.clone(), 1, columns.clone())?;
    assert_eq!(Arc::strong_count(&ids), 2);
    Arc::make_mut(&mut ids)[1] = 100;
    drop(ids);
    for batch in [
        ScanBatch::single(41, row.clone(), types.clone())?,
        ScanBatch::new(vec![41], columns)?,
        shared,
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
    for (initial, values) in [
        (i128::MAX, vec![1, -1]),
        (i128::MIN, vec![-1, 1]),
        (i128::MAX, vec![-1, 1]),
        (0, vec![i64::MAX as i128, i64::MIN as i128]),
    ] {
        let sum = functions.aggregate("sum").unwrap();
        let mut state = sum.create_state(&[DataType::HugeInt], context.types())?;
        state.update(&[Value::Integer(initial)], &context)?;
        let batch = DataChunk::new(
            vec![Vector::flat(DataType::BigInt, ints(&values))?],
            values.len(),
        )?;
        let actual = state
            .update_batch(&batch, &context)
            .and_then(|_| state.finish());
        let expected = values
            .iter()
            .try_fold(initial, |sum, value| sum.checked_add(*value));
        match expected {
            Some(value) => assert_eq!(actual?, Value::Integer(value)),
            None => assert!(matches!(actual, Err(Error::Execution(_)))),
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn integer_sum_batches_preserve_wide_prefixes_and_vector_views() -> Result<()> {
    let functions = FunctionRegistry::builtins();
    let sum = functions.aggregate("sum").unwrap();
    let query = QueryContext::background();
    for data_type in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
    ] {
        let bits = data_type.integer_bits().unwrap();
        let maximum = i128::MAX >> (128 - bits);
        let minimum = -maximum - 1;
        // Same-sign extrema overflow a machine-width partial sum. Alternating
        // signs and boundaries across 1024 rows also exercise carry/cancellation.
        let values: Vec<_> = (0..4099)
            .map(|i| {
                Value::Integer(match i % 7 {
                    0 | 1 => maximum,
                    2 | 3 => minimum,
                    _ => (i % 113) as i128 - 56,
                })
            })
            .collect();
        let flat = Vector::flat(data_type.clone(), values.clone())?;
        let mut nullable = values;
        for i in (0..nullable.len()).step_by(5) {
            nullable[i] = Value::Null;
        }
        let selected = Arc::new(flat.clone()).select(vec![4098, 0, 1, 2, 3, 3, 4, 9])?;
        for column in [
            flat.clone(),
            flat.slice(3, 4093)?,
            selected,
            Vector::flat(data_type.clone(), nullable)?,
            Vector::constant(data_type.clone(), Value::Integer(maximum), 1025)?,
            Vector::constant(data_type.clone(), Value::Null, 1025)?,
            flat.slice(0, 0)?,
        ] {
            for batch_size in [1, 7, 1024, 1025, 2048] {
                let mut scalar =
                    sum.create_state(std::slice::from_ref(&data_type), query.types())?;
                let mut batched =
                    sum.create_state(std::slice::from_ref(&data_type), query.types())?;
                let expected = column
                    .values()
                    .try_for_each(|value| scalar.update(std::slice::from_ref(value), &query))
                    .and_then(|_| scalar.finish());
                let actual = (0..column.len())
                    .step_by(batch_size)
                    .try_for_each(|offset| {
                        let count = batch_size.min(column.len() - offset);
                        batched.update_batch(
                            &DataChunk::new(vec![column.slice(offset, count)?], count)?,
                            &query,
                        )
                    })
                    .and_then(|_| batched.finish());
                assert_eq!(
                    format!("{actual:?}"),
                    format!("{expected:?}"),
                    "{data_type:?}, batch size {batch_size}"
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct RegisteredCount {
    batch: bool,
    calls: Arc<AtomicUsize>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
