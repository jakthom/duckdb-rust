use super::*;
use duckdb_rust::{
    common::{type_registry::TypeRegistry, vector::Vector},
    execution::expression_executor::{BatchedEvaluator, EvaluationContext, ExpressionEvaluator},
    function::operator::{
        NumericArithmetic, Operator, OperatorFunction, OperatorRegistry, OperatorSignature,
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    planner::{BoundExpr, ExprKind},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
                        .map(|column| column.values().collect::<Vec<_>>());
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn builtin_projection_chain_hook_proves_bounds_encodings_order_and_cancellation() -> Result<()> {
    let registry = OperatorRegistry::builtins();
    let query = QueryContext::background();
    let stage = |query: &QueryContext, literal| -> Result<BoundExpr> {
        Ok(BoundExpr {
            kind: ExprKind::Operator(
                Arc::new(registry.bind(
                    Operator::Add,
                    &[DataType::BigInt, DataType::BigInt],
                    query.types(),
                )?),
                vec![
                    BoundExpr {
                        kind: ExprKind::Column(0),
                        data_type: DataType::BigInt,
                    },
                    BoundExpr {
                        kind: ExprKind::Literal(Value::Integer(literal)),
                        data_type: DataType::BigInt,
                    },
                ],
            ),
            data_type: DataType::BigInt,
        })
    };
    let stages = vec![stage(&query, 1)?, stage(&query, 2)?];
    let input = Vector::try_bigints([Ok(Some(-2)), Ok(Some(0)), Ok(Some(4))])?;
    let batch = DataChunk::new(vec![input.clone()], input.len())?;
    let output = BatchedEvaluator
        .evaluate_projection_chain_batch(&stages, &batch, &query)?
        .expect("native BIGINT literal chain");
    assert_eq!(
        output.values().collect::<Vec<_>>(),
        vec![Value::Integer(1), Value::Integer(3), Value::Integer(7)]
    );
    assert!(output.numeric_ascending());

    for declined in [
        Vector::constant(DataType::BigInt, Value::Integer(1), 3)?,
        Vector::try_bigints([Ok(Some(1)), Ok(None), Ok(Some(3))])?,
        Arc::new(input.clone()).select(vec![2, 0, 1])?,
    ] {
        let batch = DataChunk::new(vec![declined], 3)?;
        assert!(
            BatchedEvaluator
                .evaluate_projection_chain_batch(&stages, &batch, &query)?
                .is_none()
        );
    }
    let out_of_range = vec![stage(&query, i128::from(i64::MAX) + 1)?, stage(&query, 1)?];
    assert!(
        BatchedEvaluator
            .evaluate_projection_chain_batch(&out_of_range, &batch, &query)?
            .is_none()
    );
    let overflow = DataChunk::new(
        vec![Vector::try_bigints([Ok(Some(i64::MAX - 1)), Ok(Some(0))])?],
        2,
    )?;
    assert!(
        BatchedEvaluator
            .evaluate_projection_chain_batch(&stages, &overflow, &query)?
            .is_none()
    );

    let mut types = TypeRegistry::builtins();
    types.replace(DataType::BigInt.family(), Arc::new(InvalidBatchType))?;
    let logical = QueryContext::background().with_types(Arc::new(types));
    assert!(
        BatchedEvaluator
            .evaluate_projection_chain_batch(&stages, &batch, &logical)?
            .is_none()
    );
    let logical_stages = vec![stage(&logical, 1)?, stage(&logical, 2)?];
    assert!(
        BatchedEvaluator
            .evaluate_projection_chain_batch(&logical_stages, &batch, &logical)?
            .is_none()
    );

    let handle = InterruptHandle::default();
    let cancelled = QueryContext::new(handle.clone(), None, 2, 20)?;
    handle.interrupt();
    assert!(matches!(
        BatchedEvaluator.evaluate_projection_chain_batch(&stages, &batch, &cancelled),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct CountingProjectionEvaluator(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for CountingProjectionEvaluator {
    fn name(&self) -> &'static str {
        "counting-projection"
    }
    fn evaluate(
        &self,
        expression: &duckdb_rust::planner::BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        ScalarEvaluator.evaluate(expression, row, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn projection_chain_keeps_custom_evaluator_stage_callbacks_and_native_fallbacks() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut connection = DatabaseBuilder::new()
        .expressions(Arc::new(CountingProjectionEvaluator(calls.clone())))
        .batch_size(8)
        .build()?
        .connect();
    connection.execute(
        "CREATE TABLE base(i BIGINT); INSERT INTO base VALUES (1),(2),(3); \
         CREATE VIEW first AS SELECT i + 1 AS i FROM base; \
         CREATE VIEW second AS SELECT i + 1 AS i FROM first",
    )?;
    calls.store(0, Ordering::SeqCst);
    assert_eq!(
        connection.query("SELECT sum(i) FROM second")?.rows,
        vec![ints(&[12])]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 6);

    let mut native = DatabaseBuilder::new().batch_size(8).build()?.connect();
    native.execute(
        "CREATE TABLE ends(i BIGINT); INSERT INTO ends VALUES (9223372036854775806); \
         CREATE VIEW first_end AS SELECT i + 1 AS i FROM ends; \
         CREATE VIEW second_end AS SELECT i + 1 AS i FROM first_end",
    )?;
    assert!(matches!(
        native.query("SELECT sum(i) FROM second_end"),
        Err(Error::Execution(message)) if message.contains("integer overflow")
    ));
    Ok(())
}

#[derive(Debug)]
struct InvalidProjectionChainEvaluator;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for InvalidProjectionChainEvaluator {
    fn name(&self) -> &'static str {
        "invalid-projection-chain"
    }
    fn evaluate(
        &self,
        expression: &duckdb_rust::planner::BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        ScalarEvaluator.evaluate(expression, row, context)
    }
    fn evaluate_projection_chain_batch(
        &self,
        _: &[duckdb_rust::planner::BoundExpr],
        input: &DataChunk,
        _: &dyn EvaluationContext,
    ) -> Result<Option<Vector>> {
        Ok(Some(Vector::constant(
            DataType::Varchar,
            Value::Varchar("wrong".into()),
            input.len(),
        )?))
    }
}

#[derive(Debug)]
struct InvalidIntermediateProjectionEvaluator;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for InvalidIntermediateProjectionEvaluator {
    fn name(&self) -> &'static str {
        "invalid-intermediate-projection"
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
        Vector::constant(
            DataType::Varchar,
            Value::Varchar("wrong intermediate".into()),
            input.len(),
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn projection_chain_validates_opt_in_evaluator_output() -> Result<()> {
    let mut connection = DatabaseBuilder::new()
        .expressions(Arc::new(InvalidProjectionChainEvaluator))
        .batch_size(8)
        .build()?
        .connect();
    connection.execute(
        "CREATE TABLE base(i BIGINT); INSERT INTO base VALUES (1); \
         CREATE VIEW first AS SELECT i + 1 AS i FROM base; \
         CREATE VIEW second AS SELECT i + 1 AS i FROM first",
    )?;
    assert!(matches!(
        connection.query("SELECT sum(i) FROM second"),
        Err(Error::Internal(_))
    ));

    #[derive(Debug)]
    struct WrongCardinality;
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl ExpressionEvaluator for WrongCardinality {
        fn name(&self) -> &'static str {
            "wrong-projection-chain-cardinality"
        }
        fn evaluate(
            &self,
            expression: &duckdb_rust::planner::BoundExpr,
            row: &Row,
            context: &dyn EvaluationContext,
        ) -> Result<Value> {
            ScalarEvaluator.evaluate(expression, row, context)
        }
        fn evaluate_projection_chain_batch(
            &self,
            _: &[duckdb_rust::planner::BoundExpr],
            _: &DataChunk,
            _: &dyn EvaluationContext,
        ) -> Result<Option<Vector>> {
            Ok(Some(Vector::constant(
                DataType::BigInt,
                Value::Integer(0),
                0,
            )?))
        }
    }
    let mut cardinality = DatabaseBuilder::new()
        .expressions(Arc::new(WrongCardinality))
        .batch_size(8)
        .build()?
        .connect();
    cardinality.execute(
        "CREATE TABLE base(i BIGINT); INSERT INTO base VALUES (1); \
         CREATE VIEW first AS SELECT i + 1 AS i FROM base; \
         CREATE VIEW second AS SELECT i + 1 AS i FROM first",
    )?;
    assert!(matches!(
        cardinality.query("SELECT sum(i) FROM second"),
        Err(Error::Internal(message)) if message.contains("cardinality")
    ));

    let mut intermediate = DatabaseBuilder::new()
        .expressions(Arc::new(InvalidIntermediateProjectionEvaluator))
        .batch_size(8)
        .build()?
        .connect();
    intermediate.execute(
        "CREATE TABLE base(i BIGINT); INSERT INTO base VALUES (1); \
         CREATE VIEW first AS SELECT i + 1 AS i FROM base; \
         CREATE VIEW second AS SELECT i + 1 AS i FROM first",
    )?;
    assert!(matches!(
        intermediate.query("SELECT sum(i) FROM second"),
        Err(Error::Internal(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bigint_power_of_two_remainder_filter_preserves_signed_remainder_semantics() -> Result<()> {
    let mut connection = DatabaseBuilder::new()
        .expressions(Arc::new(BatchedEvaluator))
        .batch_size(2048)
        .build()?
        .connect();
    connection.execute(
        "CREATE TABLE t(i BIGINT); \
         INSERT INTO t VALUES \
         (-9223372036854775808),(-9),(-8),(-3),(-2),(-1),(0),(1),(2),(3),(8),(9)",
    )?;
    assert_eq!(
        connection.query("SELECT i FROM t WHERE i % 2 = 0")?.rows,
        vec![
            ints(&[-9223372036854775808]),
            ints(&[-8]),
            ints(&[-2]),
            ints(&[0]),
            ints(&[2]),
            ints(&[8]),
        ],
    );
    assert_eq!(
        connection.query("SELECT i FROM t WHERE i % -8 = 0")?.rows,
        vec![
            ints(&[-9223372036854775808]),
            ints(&[-8]),
            ints(&[0]),
            ints(&[8]),
        ],
    );
    assert_eq!(
        connection
            .query("SELECT i FROM t WHERE i % -9223372036854775808 = 0")?
            .rows,
        vec![ints(&[-9223372036854775808]), ints(&[0])],
    );
    assert_eq!(
        connection
            .query(
                "SELECT i FROM (VALUES (NULL::BIGINT),(2::BIGINT),(3::BIGINT)) t(i) \
                 WHERE i % 2 = 0",
            )?
            .rows,
        vec![ints(&[2])],
    );
    assert_eq!(
        connection.query("SELECT i FROM t WHERE i % 3 = 0")?.rows,
        vec![ints(&[-9]), ints(&[-3]), ints(&[0]), ints(&[3]), ints(&[9])],
    );
    assert!(
        connection
            .query("SELECT i FROM t WHERE i % -1 = 0")
            .is_err()
    );
    assert!(
        connection
            .query("SELECT i FROM t WHERE i % 0 = 0")?
            .rows
            .is_empty()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn speculative_multi_column_projection_restores_row_major_first_error() -> Result<()> {
    let mut connection = DatabaseBuilder::new()
        .expressions(Arc::new(BatchedEvaluator))
        .batch_size(2048)
        .build()?
        .connect();
    let error = connection
        .query(
            "SELECT CAST(a AS INTEGER), CAST(b AS INTEGER) \
             FROM (VALUES ('1','bad-earlier-row'),('bad-later-row','2')) t(a,b)",
        )
        .unwrap_err();
    assert!(
        matches!(error, Error::Conversion(message) if message.contains("bad-earlier-row") && !message.contains("bad-later-row"))
    );
    Ok(())
}

#[derive(Debug)]
struct ModuloProbe {
    total: bool,
    batch_calls: Arc<AtomicUsize>,
    invalid: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
