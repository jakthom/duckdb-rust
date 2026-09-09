use super::*;
use duckdb_rust::{
    common::{type_registry::TypeRegistry, vector::Vector},
    execution::expression_executor::{BatchedEvaluator, EvaluationContext, ExpressionEvaluator},
    function::operator::{
        NumericArithmetic, Operator, OperatorFunction, OperatorRegistry, OperatorSignature,
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[test]
fn integer_batch_kernels_match_scalar_width_null_selection_and_overflow_semantics() -> Result<()> {
    let registry = OperatorRegistry::builtins();
    let query = QueryContext::background();
    for data_type in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
    ] {
        let bits = data_type.integer_bits().unwrap();
        let min = if bits == 128 {
            i128::MIN
        } else {
            -(1_i128 << (bits - 1))
        };
        let max = if bits == 128 {
            i128::MAX
        } else {
            (1_i128 << (bits - 1)) - 1
        };
        let flat = Vector::flat(
            data_type.clone(),
            vec![
                Value::Null,
                Value::Integer(min),
                Value::Integer(-7),
                Value::Integer(0),
                Value::Integer(9),
                Value::Integer(max),
            ],
        )?;
        let dictionary = Arc::new(flat.clone())
            .select(vec![5, 3, 2, 0, 1])?
            .slice(1, 4)?;
        let constant = Vector::constant(data_type.clone(), Value::Integer(min), 3)?;
        for operator in [Operator::Modulo, Operator::IntegerDivide] {
            let bound = registry.bind(
                operator,
                &[data_type.clone(), data_type.clone()],
                query.types(),
            )?;
            for input in [&flat, &dictionary, &constant] {
                for divisor in [
                    Value::Null,
                    Value::Integer(-1),
                    Value::Integer(0),
                    Value::Integer(1),
                    Value::Integer(2),
                    Value::Integer(3),
                    Value::Integer(-2),
                    Value::Integer(64),
                    Value::Integer(-64),
                    Value::Integer(min),
                ] {
                    let right = Vector::constant(data_type.clone(), divisor.clone(), input.len())?;
                    let batch = DataChunk::new(vec![input.clone(), right], input.len())?;
                    let scalar = input
                        .values()
                        .map(|value| bound.apply(&[value.clone(), divisor.clone()], &query))
                        .collect::<Result<Vec<_>>>();
                    let columns = bound
                        .apply_batch(&batch, &query)
                        .map(|column| column.values().cloned().collect::<Vec<_>>());
                    assert_eq!(
                        format!("{scalar:?}"),
                        format!("{columns:?}"),
                        "{data_type} {operator:?} {divisor}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn expression_adapters_preserve_lazy_branches_first_errors_and_effect_counts() -> Result<()> {
    let optimizers: [Arc<dyn Optimizer>; 2] = [
        Arc::new(IdentityOptimizer),
        Arc::new(PipelineOptimizer::default()),
    ];
    for optimizer in optimizers {
        for batch_size in [1, 3, 2048] {
            let mut observations = Vec::new();
            for evaluator in [
                Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                Arc::new(BatchedEvaluator),
            ] {
                let calls = Arc::new(AtomicUsize::new(0));
                let mut functions = FunctionRegistry::builtins();
                functions.register_scalar(Arc::new(CountCalls(calls.clone())))?;
                let mut connection = DatabaseBuilder::new()
                    .functions(functions)
                    .expressions(evaluator)
                    .optimizer(optimizer.clone())
                    .batch_size(batch_size)
                    .build()?
                    .connect();
                connection.execute("CREATE TABLE t(i BIGINT, v VARCHAR); INSERT INTO t VALUES (9,'bad'),(NULL,'2'),(-7,'3'),(-9223372036854775808,'4'),(0,'5'),(9223372036854775807,'6')")?;
                let mut results = Vec::new();
                for sql in [
                    "SELECT i FROM t WHERE i%2=0",
                    "SELECT i FROM t WHERE i%0 IS NULL",
                    "SELECT i FROM t WHERE NOT(i%64=1)",
                    "SELECT i FROM t WHERE count_calls(i)%2=0",
                    "SELECT i FROM t WHERE i%(-1)=0",
                    "SELECT i FROM t WHERE i%(-1)=CAST(v AS BIGINT)",
                    "SELECT i FROM t WHERE false AND i%(-1)=0",
                    "SELECT i FROM t WHERE true OR i%(-1)=0",
                    "SELECT i FROM t WHERE CASE WHEN i<0 THEN true ELSE i%2=0 END",
                    "SELECT i FROM t WHERE i%2=0 LIMIT 1",
                ] {
                    let result = connection.query(sql).map(|result| result.rows);
                    results.push((format!("{result:?}"), calls.swap(0, Ordering::Relaxed)));
                }
                observations.push(results);
            }
            assert_eq!(
                observations[0],
                observations[1],
                "batch={batch_size}, optimizer={}",
                optimizer.name()
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ModuloProbe {
    total: bool,
    batch_calls: Arc<AtomicUsize>,
    invalid: bool,
}
impl OperatorFunction for ModuloProbe {
    fn name(&self) -> &'static str {
        "independent-modulo-probe"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        NumericArithmetic.supports(signature)
    }
    fn is_total(&self, signature: &OperatorSignature, constants: &[Option<&Value>]) -> bool {
        self.total && NumericArithmetic.is_total(signature, constants)
    }
    fn evaluate(
        &self,
        signature: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        NumericArithmetic.evaluate(signature, arguments, query)
    }
    fn evaluate_batch(
        &self,
        signature: &OperatorSignature,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Vector> {
        self.batch_calls.fetch_add(1, Ordering::Relaxed);
        if self.invalid {
            Vector::constant(signature.result.clone(), Value::Integer(7), arguments.len())
        } else {
            duckdb_rust::function::operator::evaluate_operator_rows(
                self, signature, arguments, query,
            )
        }
    }
}

#[test]
fn operator_replacement_controls_batch_proofs_and_cannot_bypass_null_validation() -> Result<()> {
    for (total, invalid) in [(false, false), (true, false), (true, true)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = OperatorRegistry::builtins();
        registry.replace(
            OperatorSignature {
                operator: Operator::Modulo,
                arguments: vec![DataType::BigInt; 2],
                result: DataType::BigInt,
                nullable: true,
            },
            Arc::new(ModuloProbe {
                total,
                invalid,
                batch_calls: calls.clone(),
            }),
        )?;
        let mut connection = DatabaseBuilder::new()
            .operators(registry)
            .build()?
            .connect();
        connection.execute("CREATE TABLE t(i BIGINT); INSERT INTO t VALUES (1),(2),(NULL),(4)")?;
        let result = connection.query("SELECT i FROM t WHERE i%2=0");
        if invalid {
            assert!(matches!(result, Err(Error::Internal(_))));
        } else {
            assert_eq!(result?.rows, vec![ints(&[2]), ints(&[4])]);
        }
        assert_eq!(calls.load(Ordering::Relaxed), usize::from(total));
    }
    Ok(())
}

struct InvalidBatchExpression {
    wrong_type: bool,
}
impl ExpressionEvaluator for InvalidBatchExpression {
    fn name(&self) -> &'static str {
        "invalid-batch-expression"
    }
    fn evaluate(
        &self,
        expression: &duckdb_rust::planner::BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        ScalarEvaluator.evaluate(expression, row, context)
    }
    fn evaluate_batch(
        &self,
        _: &duckdb_rust::planner::BoundExpr,
        input: &DataChunk,
        _: &dyn EvaluationContext,
    ) -> Result<Vector> {
        if self.wrong_type {
            Vector::constant(DataType::BigInt, Value::Integer(0), input.len())
        } else {
            Vector::constant(DataType::Boolean, Value::Boolean(false), input.len() - 1)
        }
    }
}

#[test]
fn rejected_filter_rows_do_not_hide_invalid_expression_batches() -> Result<()> {
    for wrong_type in [false, true] {
        let mut connection = DatabaseBuilder::new()
            .expressions(Arc::new(InvalidBatchExpression { wrong_type }))
            .build()?
            .connect();
        connection.execute("CREATE TABLE t(i BIGINT); INSERT INTO t VALUES(1),(2)")?;
        assert!(matches!(
            connection.query("SELECT i FROM t WHERE i=3"),
            Err(Error::Internal(_))
        ));
    }
    Ok(())
}

struct InvalidSelection(Vec<usize>);
impl ExpressionEvaluator for InvalidSelection {
    fn name(&self) -> &'static str {
        "invalid-predicate-selection"
    }
    fn evaluate(
        &self,
        expression: &duckdb_rust::planner::BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        ScalarEvaluator.evaluate(expression, row, context)
    }
    fn select_batch(
        &self,
        _: &duckdb_rust::planner::BoundExpr,
        _: &DataChunk,
        _: &dyn EvaluationContext,
    ) -> Result<Vec<usize>> {
        Ok(self.0.clone())
    }
}

#[test]
fn predicate_selection_rejects_duplicate_reordered_and_out_of_bounds_rows() -> Result<()> {
    for positions in [vec![0, 0], vec![1, 0], vec![2], vec![usize::MAX]] {
        let mut connection = DatabaseBuilder::new()
            .expressions(Arc::new(InvalidSelection(positions)))
            .build()?
            .connect();
        connection.execute("CREATE TABLE t(i BIGINT); INSERT INTO t VALUES(1),(2)")?;
        assert!(matches!(
            connection.query("SELECT i FROM t WHERE i=1"),
            Err(Error::Internal(_))
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct InvalidBatchType;
impl duckdb_rust::common::type_registry::TypeAdapter for InvalidBatchType {
    fn name(&self) -> &'static str {
        "invalid-batch-type"
    }
    fn validate_type(&self, _: &DataType) -> Result<()> {
        Ok(())
    }
    fn validate_value(&self, _: &DataType, _: &Value, _: &QueryContext) -> Result<()> {
        Ok(())
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        Ok(DataType::common(left, right).ok())
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        _: &QueryContext,
    ) -> Result<std::cmp::Ordering> {
        left.compare(right)
    }
    fn write_key(
        &self,
        data_type: &DataType,
        value: &Value,
        output: &mut duckdb_rust::common::type_registry::KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        duckdb_rust::common::type_registry::PrimitiveTypes
            .write_key(data_type, value, output, query)
    }
    fn compare_batch(
        &self,
        _: &DataType,
        left: &Vector,
        _: &Vector,
        _: &QueryContext,
    ) -> Result<Vec<Option<std::cmp::Ordering>>> {
        Ok(vec![Some(std::cmp::Ordering::Equal); left.len()])
    }
}

#[test]
fn type_batch_boundaries_reject_invalid_null_results_and_cancel() -> Result<()> {
    let mut types = TypeRegistry::builtins();
    types.replace(DataType::BigInt.family(), Arc::new(InvalidBatchType))?;
    let bound = types.bind(&DataType::BigInt)?;
    let left = Vector::flat(DataType::BigInt, vec![Value::Integer(1), Value::Null])?;
    let right = Vector::constant(DataType::BigInt, Value::Integer(1), 2)?;
    assert!(matches!(
        bound.compare_batch(&left, &right, &QueryContext::background()),
        Err(Error::Internal(_))
    ));
    let handle = InterruptHandle::default();
    let query = QueryContext::new(handle.clone(), None, 2, 20)?;
    handle.interrupt();
    assert!(matches!(
        bound.compare_batch(&left, &right, &query),
        Err(Error::Interrupted)
    ));
    Ok(())
}
