use super::*;
use duckdb_rust::{
    common::{
        type_registry::TypeRegistry,
        vector::{DataChunk, Vector},
    },
    function::{
        AggregateFunction, AggregateState,
        grouped::{GroupSelection, GroupedAggregateState},
    },
};

#[test]
fn grouped_integer_states_match_scalar_states_for_encodings_widths_and_empty_groups() -> Result<()>
{
    let query = QueryContext::background();
    let functions = FunctionRegistry::builtins();
    for data_type in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
    ] {
        let bits = data_type.integer_bits().unwrap();
        let maximum = i128::MAX >> (128 - bits);
        let minimum = -maximum - 1;
        let values = (0..4099)
            .map(|i| {
                Value::Integer(match i % 7 {
                    0 | 1 => maximum,
                    2 | 3 => minimum,
                    _ => i % 113 - 56,
                })
            })
            .collect::<Vec<_>>();
        let flat = Vector::flat(data_type.clone(), values.clone())?;
        let nullable = Vector::flat(
            data_type.clone(),
            values
                .into_iter()
                .enumerate()
                .map(|(i, value)| if i % 5 == 0 { Value::Null } else { value })
                .collect(),
        )?;
        let columns = [
            flat.clone(),
            flat.slice(3, 4093)?,
            nullable,
            Arc::new(flat).select(vec![4098, 0, 1, 2, 3, 3, 4, 9])?,
            Vector::constant(data_type.clone(), Value::Integer(maximum), 4099)?,
            Vector::constant(data_type.clone(), Value::Null, 4099)?,
            Vector::flat(data_type.clone(), vec![])?,
        ];
        for column in columns {
            for name in ["sum", "count"] {
                let function = functions.aggregate(name).unwrap();
                for batch_size in [1, 7, 1024, 2048] {
                    let mut grouped = function
                        .create_grouped_state(std::slice::from_ref(&data_type), query.types())?
                        .unwrap();
                    grouped.resize(7, &query)?;
                    let mut scalar = (0..9)
                        .map(|_| {
                            function.create_state(std::slice::from_ref(&data_type), query.types())
                        })
                        .collect::<Result<Vec<_>>>()?;
                    for (i, value) in column.values().enumerate() {
                        scalar[i % 5].update(std::slice::from_ref(value), &query)?;
                    }
                    for offset in (0..column.len()).step_by(batch_size) {
                        let len = batch_size.min(column.len() - offset);
                        let groups = (offset..offset + len).map(|i| i % 5).collect::<Vec<_>>();
                        let selection = GroupSelection::new(&groups, 7, &query)?;
                        let input = DataChunk::new(vec![column.slice(offset, len)?], len)?;
                        grouped.update_batch(&selection, &input, &query)?;
                    }
                    grouped.resize(9, &query)?;
                    let expected = scalar
                        .into_iter()
                        .map(|s| s.finish())
                        .collect::<Result<Vec<_>>>()?;
                    assert_eq!(
                        grouped.finish(&query)?,
                        expected,
                        "{name} {data_type:?} batch={batch_size}"
                    );
                }
            }
        }
    }
    for data_type in [DataType::HugeInt, DataType::Double] {
        assert!(
            functions
                .aggregate("sum")
                .unwrap()
                .create_grouped_state(&[data_type], query.types())?
                .is_none()
        );
    }
    let mut count = functions
        .aggregate("count")
        .unwrap()
        .create_grouped_state(&[], query.types())?
        .unwrap();
    count.resize(3, &query)?;
    let groups = [1, 1, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0];
    count.update_batch(
        &GroupSelection::new(&groups, 3, &query)?,
        &DataChunk::new(vec![], groups.len())?,
        &query,
    )?;
    assert_eq!(
        count.finish(&query)?,
        vec![Value::Integer(8), Value::Integer(4), Value::Integer(0)]
    );
    Ok(())
}

#[test]
fn grouped_count_handles_nullable_strings_and_cancelled_empty_states() -> Result<()> {
    let query = QueryContext::background();
    let function = FunctionRegistry::builtins().aggregate("count").unwrap();
    let values = (0..2051)
        .map(|i| {
            if i % 3 == 0 {
                Value::Null
            } else {
                Value::Varchar(i.to_string())
            }
        })
        .collect::<Vec<_>>();
    let flat = Vector::flat(DataType::Varchar, values)?;
    for column in [
        flat.clone(),
        Arc::new(flat).select(vec![0, 1, 2, 0, 2048, 2048, 2050])?,
        Vector::constant(DataType::Varchar, Value::Null, 2051)?,
        Vector::constant(DataType::Varchar, Value::Varchar("x".into()), 2051)?,
        Vector::flat(DataType::Varchar, vec![])?,
    ] {
        for destinations in [1, 3] {
            for batch_size in [1, 7, 1024] {
                let mut state = function
                    .create_grouped_state(&[DataType::Varchar], query.types())?
                    .unwrap();
                state.resize(4, &query)?;
                let mut expected = [0_i128; 4];
                for (row, value) in column.values().enumerate() {
                    expected[row % destinations] += i128::from(!value.is_null());
                }
                for offset in (0..column.len()).step_by(batch_size) {
                    let len = batch_size.min(column.len() - offset);
                    let indices = (offset..offset + len)
                        .map(|i| i % destinations)
                        .collect::<Vec<_>>();
                    state.update_batch(
                        &GroupSelection::new(&indices, 4, &query)?,
                        &DataChunk::new(vec![column.slice(offset, len)?], len)?,
                        &query,
                    )?;
                }
                assert_eq!(state.finish(&query)?, expected.map(Value::Integer));
            }
        }
    }
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 10)?;
    let state = function.create_grouped_state(&[], query.types())?.unwrap();
    interrupt.interrupt();
    assert!(matches!(state.finish(&cancelled), Err(Error::Interrupted)));
    Ok(())
}

#[test]
fn group_destinations_validate_shape_counts_cancellation_and_resources() -> Result<()> {
    let query = QueryContext::background();
    assert!(matches!(
        GroupSelection::new(&[0, 2], 2, &query),
        Err(Error::Internal(_))
    ));
    let indices = (0..4111).map(|i| (i * 7) % 5).collect::<Vec<_>>();
    let groups = GroupSelection::new(&indices, 7, &query)?;
    let counts = groups.counts().unwrap();
    for (group, &count) in counts.iter().enumerate() {
        assert_eq!(count, indices.iter().filter(|&&g| g == group).count());
    }
    assert_eq!(counts.len(), 7);
    assert_eq!(counts.iter().sum::<usize>(), indices.len());
    assert_eq!(groups.indices(), indices);
    let constant = GroupSelection::new(&[6; 19], 7, &query)?;
    assert_eq!(constant.constant_group(), Some(6));
    assert!(constant.counts().is_none());
    let sparse = GroupSelection::new(&[6, 0, 6], 7, &query)?;
    assert_eq!(sparse.indices(), &[6, 0, 6]);
    assert!(sparse.counts().is_none());
    let functions = FunctionRegistry::builtins();
    let mut state = functions
        .aggregate("count")
        .unwrap()
        .create_grouped_state(&[], query.types())?
        .unwrap();
    state.resize(6, &query)?;
    assert!(
        state
            .update_batch(&groups, &DataChunk::new(vec![], indices.len())?, &query)
            .is_err()
    );
    state.resize(7, &query)?;
    assert!(state.resize(6, &query).is_err());
    assert!(
        state
            .update_batch(&groups, &DataChunk::new(vec![], indices.len() - 1)?, &query)
            .is_err()
    );
    let limited = QueryContext::new(InterruptHandle::default(), None, 2, 3)?;
    assert!(matches!(
        GroupSelection::new(&indices, 7, &limited),
        Err(Error::Resource(_))
    ));
    let bounded = QueryContext::new(InterruptHandle::default(), None, 2, 7)?;
    assert_eq!(
        GroupSelection::new(&indices, 7, &bounded)?.counts(),
        Some(counts)
    );
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 2, 100)?;
    interrupt.interrupt();
    assert!(matches!(
        GroupSelection::new(&[], 0, &cancelled),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        state.resize(7, &cancelled),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct RegisteredSum {
    scalar: Arc<dyn AggregateFunction>,
    mode: usize,
    batches: Arc<AtomicUsize>,
}
impl AggregateFunction for RegisteredSum {
    fn name(&self) -> &str {
        "registered_sum"
    }
    fn return_type(&self, arguments: &[DataType], types: &TypeRegistry) -> Result<DataType> {
        self.scalar.return_type(arguments, types)
    }
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        self.scalar.create_state(arguments, types)
    }
    fn create_grouped_state(
        &self,
        arguments: &[DataType],
        _: &TypeRegistry,
    ) -> Result<Option<Box<dyn GroupedAggregateState>>> {
        if arguments != [DataType::BigInt] {
            return Ok(None);
        }
        Ok(Some(Box::new(ScalarGroups {
            function: self.scalar.clone(),
            states: vec![],
            counts: vec![],
            mode: self.mode,
            batches: self.batches.clone(),
        })))
    }
}
struct ScalarGroups {
    function: Arc<dyn AggregateFunction>,
    states: Vec<Box<dyn AggregateState>>,
    counts: Vec<usize>,
    mode: usize,
    batches: Arc<AtomicUsize>,
}
impl GroupedAggregateState for ScalarGroups {
    fn group_count(&self) -> usize {
        self.states.len()
    }
    fn resize(&mut self, groups: usize, query: &QueryContext) -> Result<()> {
        query.check_rows(groups)?;
        if groups < self.states.len() {
            return Err(Error::Internal(
                "cannot shrink test aggregate states".into(),
            ));
        }
        if self.mode == 1 {
            return Ok(());
        }
        while self.states.len() < groups {
            self.states.push(
                self.function
                    .create_state(&[DataType::BigInt], query.types())?,
            );
            self.counts.push(0);
        }
        Ok(())
    }
    fn update_batch(
        &mut self,
        groups: &GroupSelection<'_>,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<()> {
        groups.validate(arguments, self.group_count())?;
        self.batches.fetch_add(1, Ordering::Relaxed);
        for (&group, row) in groups.indices().iter().zip(arguments.rows()) {
            self.counts[group] = self.counts[group]
                .checked_add(1)
                .ok_or_else(|| Error::Resource("test update count exceeded".into()))?;
            self.states[group].update(&row, query)?;
        }
        Ok(())
    }
    fn finish(self: Box<Self>, _: &QueryContext) -> Result<Vec<Value>> {
        let mut values = self
            .states
            .into_iter()
            .map(|s| s.finish())
            .collect::<Result<Vec<_>>>()?;
        if self.mode == 2 {
            values.pop();
        }
        if self.mode == 3 {
            values[0] = Value::Varchar("invalid".into());
        }
        Ok(values)
    }
}

#[test]
fn grouped_functions_replace_storage_without_changing_sql_and_reject_bad_adapters() -> Result<()> {
    for mode in 0..4 {
        let batches = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_aggregate(Arc::new(RegisteredSum {
            scalar: functions.aggregate("sum").unwrap(),
            mode,
            batches: batches.clone(),
        }))?;
        let db = DatabaseBuilder::new()
            .functions(functions)
            .batch_size(16)
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t AS SELECT i%3 AS a,i AS v FROM range(100)t(i); CREATE TABLE totals(s HUGEINT)")?;
        let sql =
            "SELECT registered_sum(v),grouping(a) FROM t GROUP BY ROLLUP(a) ORDER BY grouping(a),a";
        if mode == 0 {
            assert_eq!(
                c.query(sql)?.rows,
                c.query(
                    "SELECT sum(v),grouping(a) FROM t GROUP BY ROLLUP(a) ORDER BY grouping(a),a"
                )?
                .rows
            );
            assert!(batches.load(Ordering::Relaxed) > 0);
        } else {
            assert!(c.query(sql).is_err());
            assert!(
                c.execute("INSERT INTO totals SELECT registered_sum(v) FROM t GROUP BY ROLLUP(a)")
                    .is_err()
            );
            assert!(c.query("SELECT * FROM totals")?.rows.is_empty());
        }
    }
    Ok(())
}
