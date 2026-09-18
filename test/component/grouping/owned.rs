use super::*;

#[derive(Debug)]
struct OwnedProbe {
    strategy: AggregateModifierStrategy,
    owned_calls: Arc<AtomicUsize>,
    state_calls: Arc<AtomicUsize>,
    invalid_result: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for OwnedProbe {
    fn name(&self) -> &str {
        "owned_probe"
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if arguments != [DataType::Varchar] {
            return Err(Error::Bind("owned probe needs VARCHAR".into()));
        }
        Ok(DataType::Varchar)
    }
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        self.return_type(arguments, types)?;
        Ok(Box::new(ProbeState {
            value: String::new(),
            calls: self.state_calls.clone(),
        }))
    }
    fn modifier_strategy(&self, _: &[DataType]) -> AggregateModifierStrategy {
        self.strategy
    }
    fn finish_owned(
        &self,
        arguments: &[DataType],
        columns: Vec<Vec<Value>>,
        permutation: Vec<usize>,
        query: &QueryContext,
    ) -> Result<Value> {
        self.return_type(arguments, query.types())?;
        self.owned_calls.fetch_add(1, Ordering::SeqCst);
        if self.invalid_result {
            return Ok(Value::Integer(42));
        }
        let [column]: [Vec<Value>; 1] = columns
            .try_into()
            .map_err(|_| Error::Internal("owned probe width".into()))?;
        assert_eq!(column.len(), permutation.len());
        let mut seen = vec![false; column.len()];
        let mut value = String::new();
        for index in permutation {
            query.check()?;
            assert!(!std::mem::replace(&mut seen[index], true));
            match &column[index] {
                Value::Varchar(input) => value.push_str(input),
                Value::Null => value.push('_'),
                _ => return Err(Error::Internal("owned probe type".into())),
            }
        }
        Ok(Value::Varchar(value))
    }
}

struct ProbeState {
    value: String,
    calls: Arc<AtomicUsize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateState for ProbeState {
    fn update(&mut self, arguments: &[Value], query: &QueryContext) -> Result<()> {
        query.check()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        match arguments {
            [Value::Varchar(input)] => self.value.push_str(input),
            [Value::Null] => self.value.push('_'),
            _ => return Err(Error::Internal("probe state type".into())),
        }
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(Value::Varchar(self.value))
    }
}

#[derive(Debug)]
struct MissingOwned;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for MissingOwned {
    fn name(&self) -> &str {
        "missing_owned"
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Varchar)
    }
    fn create_state(&self, _: &[DataType], _: &TypeRegistry) -> Result<Box<dyn AggregateState>> {
        Err(Error::Internal("unexpected borrowed fallback".into()))
    }
    fn modifier_strategy(&self, _: &[DataType]) -> AggregateModifierStrategy {
        AggregateModifierStrategy::BufferedOwnedTotal
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn owned_completion_is_selected_explicitly_and_preserves_order_nulls_and_fallback() -> Result<()> {
    for strategy in [
        AggregateModifierStrategy::Generic,
        AggregateModifierStrategy::BufferedTotal,
        AggregateModifierStrategy::BufferedOwnedTotal,
    ] {
        for algorithm in algorithms() {
            // The capability permits, but does not mandate, the optimization.
            // OrderedAggregation intentionally retains its row/state driver.
            let owns_completion = strategy == AggregateModifierStrategy::BufferedOwnedTotal
                && algorithm.name() == HashAggregation.name();
            let owned_calls = Arc::new(AtomicUsize::new(0));
            let state_calls = Arc::new(AtomicUsize::new(0));
            let mut functions = FunctionRegistry::builtins();
            functions.register_aggregate(Arc::new(OwnedProbe {
                strategy,
                owned_calls: owned_calls.clone(),
                state_calls: state_calls.clone(),
                invalid_result: false,
            }))?;
            let db = DatabaseBuilder::new()
                .functions(functions)
                .batch_size(3)
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_aggregation(algorithm),
                ))
                .build()?;
            assert_eq!(db.connect().query("SELECT owned_probe(x ORDER BY k) FROM (VALUES ('b',2),('a',1),(NULL,3),('c',2)) t(x,k)")?.rows, vec![vec![Value::Varchar("abc_".into())]]);
            if owns_completion {
                assert_eq!(owned_calls.load(Ordering::SeqCst), 1);
                assert_eq!(state_calls.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(owned_calls.load(Ordering::SeqCst), 0);
                assert_eq!(state_calls.load(Ordering::SeqCst), 4);
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn owned_capability_without_implementation_and_wrong_result_fail_without_fallback() -> Result<()> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_aggregate(Arc::new(MissingOwned))?;
    let db = DatabaseBuilder::new().functions(functions).build()?;
    assert!(
        matches!(db.connect().query("SELECT missing_owned(x ORDER BY x) FROM (VALUES ('b'),('a')) t(x)"), Err(Error::Internal(message)) if message.contains("no implementation"))
    );
    let state_calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::builtins();
    functions.register_aggregate(Arc::new(OwnedProbe {
        strategy: AggregateModifierStrategy::BufferedOwnedTotal,
        owned_calls: Arc::new(AtomicUsize::new(0)),
        state_calls: state_calls.clone(),
        invalid_result: true,
    }))?;
    let db = DatabaseBuilder::new().functions(functions).build()?;
    assert!(
        db.connect()
            .query("SELECT owned_probe(x ORDER BY x) FROM (VALUES ('b'),('a')) t(x)")
            .is_err()
    );
    assert_eq!(state_calls.load(Ordering::SeqCst), 0);
    Ok(())
}
