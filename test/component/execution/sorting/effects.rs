use super::*;
use std::sync::Mutex;

#[derive(Debug)]
struct Trace {
    values: Arc<Mutex<Vec<i128>>>,
    failure: Option<i128>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Trace {
    fn name(&self) -> &str {
        "sort_trace"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: false,
        }
    }
    fn return_type(
        &self,
        args: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if args == [DataType::BigInt] {
            Ok(DataType::BigInt)
        } else {
            Err(Error::Bind("sort_trace needs one BIGINT".into()))
        }
    }
    fn evaluate(&self, args: &[Value], q: &QueryContext) -> Result<Value> {
        q.check()?;
        let value = args[0].as_i128()?;
        self.values.lock().unwrap().push(value);
        if self.failure == Some(value) {
            return Err(Error::Execution("sort trace failure".into()));
        }
        Ok(args[0].clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sorting_and_projection_keep_row_effect_order_first_errors_and_owned_results() -> Result<()> {
    for algorithm in algorithms() {
        for expressions in [
            Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
            Arc::new(BatchedEvaluator),
        ] {
            for failure in [None, Some(1)] {
                let seen = Arc::new(Mutex::new(Vec::new()));
                let mut functions = FunctionRegistry::builtins();
                functions.register_scalar(Arc::new(Trace {
                    values: seen.clone(),
                    failure,
                }))?;
                let db = DatabaseBuilder::new()
                    .functions(functions)
                    .expressions(expressions.clone())
                    .physical_planner(Arc::new(
                        NativePhysicalPlanner::default().with_sorting(algorithm.clone()),
                    ))
                    .batch_size(3)
                    .build()?;
                let mut c = db.connect();
                c.execute("CREATE TABLE t(i BIGINT); INSERT INTO t VALUES (2),(0),(1)")?;
                let result = c.query("SELECT sort_trace(i),sort_trace(i+10) FROM t ORDER BY ALL");
                if failure.is_some() {
                    assert!(
                        matches!(result, Err(Error::Execution(message)) if message == "sort trace failure")
                    );
                    assert_eq!(*seen.lock().unwrap(), vec![2, 12, 0, 10, 1]);
                } else {
                    assert_eq!(
                        result?.rows,
                        vec![ints(&[0, 10]), ints(&[1, 11]), ints(&[2, 12])]
                    );
                    assert_eq!(*seen.lock().unwrap(), vec![2, 12, 0, 10, 1, 11]);
                }
                seen.lock().unwrap().clear();
                // Hidden sort expressions must be evaluated once, even when
                // their results are projected away from the delivered rows.
                let result = c.query("SELECT i FROM t ORDER BY sort_trace(i)");
                if failure.is_some() {
                    assert!(
                        matches!(result, Err(Error::Execution(message)) if message == "sort trace failure")
                    );
                } else {
                    let mut retained = Vec::new();
                    let expected = result?.rows;
                    seen.lock().unwrap().clear();
                    c.query_batches("SELECT i FROM t ORDER BY sort_trace(i)", |_, batch| {
                        retained.push(batch);
                        Ok(StreamControl::Continue)
                    })?;
                    c.execute("DELETE FROM t")?;
                    drop(c);
                    drop(db);
                    assert_eq!(
                        retained
                            .iter()
                            .flat_map(DataChunk::rows)
                            .collect::<Vec<_>>(),
                        expected
                    );
                }
                assert_eq!(*seen.lock().unwrap(), vec![2, 0, 1]);
            }
        }
    }
    Ok(())
}

struct WrongBatchLength;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for WrongBatchLength {
    fn name(&self) -> &'static str {
        "wrong-sort-batch-length"
    }
    fn evaluate(&self, e: &BoundExpr, row: &Row, c: &dyn EvaluationContext) -> Result<Value> {
        ScalarEvaluator.evaluate(e, row, c)
    }
    fn evaluate_batch(
        &self,
        e: &BoundExpr,
        input: &DataChunk,
        _: &dyn EvaluationContext,
    ) -> Result<Vector> {
        Vector::constant(e.data_type.clone(), Value::Integer(0), input.len() + 1)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn radix_sort_rejects_malformed_expression_batch_shapes() -> Result<()> {
    let input = DataChunk::from_rows(&[DataType::BigInt], &[ints(&[1])])?;
    assert!(
        matches!(sort(&RadixSort, &QueryContext::background(), &WrongBatchLength, &input, &[key(0,&DataType::BigInt,false,false)]), Err(Error::Internal(message)) if message.contains("cardinality"))
    );
    Ok(())
}
