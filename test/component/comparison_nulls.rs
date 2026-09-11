use super::*;
use duckdb_rust::{
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec},
        type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry},
    },
    execution::expression_executor::{
        BatchedEvaluator, EvaluationContext, ExpressionEvaluator, ScalarEvaluator,
    },
    function::{FunctionEffects, ScalarFunction},
    optimizer::{Optimizer, PipelineOptimizer},
    planner::{BoundExpr, ExprKind, expression::BinaryOp},
};

#[derive(Debug)]
struct Effect(Arc<AtomicUsize>);

struct OrdinaryEvaluator;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for OrdinaryEvaluator {
    fn name(&self) -> &'static str {
        "ordinary-comparison-evaluator"
    }
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Vec<Value>,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        ScalarEvaluator.evaluate(expression, row, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn comparison_projection_does_not_inherit_an_ordinary_replacement_encoding_claim() -> Result<()> {
    let mut c = DatabaseBuilder::new()
        .expressions(Arc::new(OrdinaryEvaluator))
        .optimizer(Arc::new(IdentityOptimizer))
        .build()?
        .connect();
    assert!(matches!(
        c.query("SELECT a=CAST('bad' AS INTEGER) FROM (SELECT NULL::INTEGER a FROM range(3))t"),
        Err(Error::Conversion(_))
    ));
    Ok(())
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Effect {
    fn name(&self) -> &str {
        "comparison_effect"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: true,
        }
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Integer)
    }
    fn evaluate(&self, _: &[Value], q: &QueryContext) -> Result<Value> {
        q.check()?;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Integer(7))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn comparisons_skip_only_after_an_executed_physical_constant_null() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for size in [1, 2, 5] {
                let effects = Arc::new(AtomicUsize::new(0));
                let mut functions = FunctionRegistry::builtins();
                functions.register_scalar(Arc::new(Effect(effects.clone())))?;
                let mut c = DatabaseBuilder::new()
                    .functions(functions)
                    .expressions(evaluator.clone())
                    .optimizer(optimizer.clone())
                    .batch_size(size)
                    .build()?
                    .connect();
                for op in ["=", "<>", "<", "<=", ">", ">="] {
                    assert_eq!(
                        c.query(&format!("SELECT NULL::INTEGER {op} CAST('bad' AS INTEGER)"))?
                            .rows,
                        vec![vec![Value::Null]]
                    );
                    assert_eq!(c.query(&format!("SELECT a {op} CAST('bad' AS INTEGER) FROM (SELECT NULL::INTEGER a FROM range(3))t"))?.rows,vec![vec![Value::Null];3]);
                    assert_eq!(c.query(&format!("SELECT i FROM (SELECT NULL::INTEGER a,i FROM range(3)t(i))t WHERE a {op} CAST('bad' AS INTEGER)"))?.rows,Vec::<Vec<Value>>::new());
                    assert!(matches!(c.query(&format!("SELECT a {op} CAST('bad' AS INTEGER) FROM (VALUES (NULL::INTEGER),(NULL))t(a)")),Err(Error::Conversion(_))));
                    assert!(matches!(
                        c.query(&format!("SELECT CAST('bad' AS INTEGER) {op} NULL::INTEGER")),
                        Err(Error::Conversion(_))
                    ));
                }
                assert_eq!(c.query("SELECT nullif(NULL::INTEGER,CAST('bad' AS INTEGER)),(SELECT NULL::INTEGER)=CAST('bad' AS INTEGER)")?.rows,vec![vec![Value::Null,Value::Null]]);
                assert_eq!(
                    c.query("SELECT nullif(NULL::INTEGER,comparison_effect())")?
                        .rows,
                    vec![vec![Value::Null]]
                );
                assert_eq!(effects.load(Ordering::SeqCst), 0);
                assert_eq!(
                    c.query("SELECT comparison_effect()=NULL::INTEGER FROM range(3)")?
                        .rows,
                    vec![vec![Value::Null]; 3]
                );
                assert_eq!(effects.load(Ordering::SeqCst), 3);
                assert_eq!(
                    c.query(
                        "SELECT a=comparison_effect() FROM (VALUES(NULL::INTEGER),(NULL))t(a)"
                    )?
                    .rows,
                    vec![vec![Value::Null]; 2]
                );
                assert_eq!(effects.load(Ordering::SeqCst), 5);
                let p = c.prepare("SELECT $1::INTEGER=CAST('bad' AS INTEGER)")?;
                assert_eq!(
                    c.execute_prepared(&p, &[Value::Null])?.rows,
                    vec![vec![Value::Null]]
                );
                assert!(matches!(
                    c.execute_prepared(&p, &[Value::Integer(1)]),
                    Err(Error::Conversion(_))
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ResourceCast(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for ResourceCast {
    fn name(&self) -> &'static str {
        "total-but-resource-fallible-comparison-cast"
    }
    fn supports(&self, s: &CastSpec) -> bool {
        s.source == DataType::Integer && s.target == DataType::BigInt
    }
    fn is_total(&self, _: &CastSpec) -> bool {
        true
    }
    fn cast(&self, _: &Value, _: &CastSpec, q: &QueryContext) -> Result<Value> {
        q.check()?;
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(Error::Resource("required comparison child".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn binary(
    op: BinaryOp,
    left: BoundExpr,
    right: BoundExpr,
    query: &QueryContext,
) -> Result<BoundExpr> {
    Ok(BoundExpr {
        data_type: DataType::Boolean,
        kind: ExprKind::Binary(
            op,
            Box::new(left),
            Box::new(right),
            query.types().bind(&DataType::BigInt)?.into(),
        ),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn constant_null_comparison_batches_predicates_and_dictionary_cache_preserve_provenance()
-> Result<()> {
    let query = QueryContext::background();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut casts = CastRegistry::builtins();
    let spec = CastSpec {
        source: DataType::Integer,
        target: DataType::BigInt,
        mode: CastMode::Explicit,
    };
    casts.replace(spec, Arc::new(ResourceCast(calls.clone())))?;
    let right = BoundExpr::column(1, DataType::Integer).cast(
        DataType::BigInt,
        CastMode::Explicit,
        &casts,
        query.types(),
    )?;
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for op in [
            BinaryOp::Equal,
            BinaryOp::NotEqual,
            BinaryOp::Less,
            BinaryOp::LessEqual,
            BinaryOp::Greater,
            BinaryOp::GreaterEqual,
        ] {
            let expression = binary(
                op,
                BoundExpr::column(0, DataType::BigInt),
                right.clone(),
                &query,
            )?;
            assert!(expression.is_pure_and_total());
            let input = DataChunk::new(
                vec![
                    Vector::constant(DataType::BigInt, Value::Null, 3)?,
                    Vector::flat(DataType::Integer, vec![Value::Integer(1); 3])?,
                ],
                3,
            )?;
            let before = calls.load(Ordering::SeqCst);
            let result = evaluator.evaluate_batch(&expression, &input, &query)?;
            assert_eq!(result.constant_value(), Some(&Value::Null));
            assert_eq!(
                evaluator.select_batch(&expression, &input, &query)?,
                Vec::<usize>::new()
            );
            assert_eq!(calls.load(Ordering::SeqCst), before);
            let flat = DataChunk::new(
                vec![
                    Vector::flat(DataType::BigInt, vec![Value::Null; 3])?,
                    input.columns()[1].clone(),
                ],
                3,
            )?;
            assert!(matches!(
                evaluator.evaluate_batch(&expression, &flat, &query),
                Err(Error::Resource(_))
            ));
            assert!(matches!(
                evaluator.select_batch(&expression, &flat, &query),
                Err(Error::Resource(_))
            ));
            let reversed = binary(
                op,
                right.clone(),
                BoundExpr::column(0, DataType::BigInt),
                &query,
            )?;
            assert!(matches!(
                evaluator.evaluate_batch(&reversed, &input, &query),
                Err(Error::Resource(_))
            ));
        }
    }
    let mut null = BoundExpr::literal(Value::Null);
    null.data_type = DataType::BigInt;
    let right = BoundExpr::column(0, DataType::Varchar).cast(
        DataType::BigInt,
        CastMode::Explicit,
        &CastRegistry::builtins(),
        query.types(),
    )?;
    let expression = binary(BinaryOp::Equal, null, right, &query)?;
    let dictionary = Arc::new(Vector::flat(
        DataType::Varchar,
        vec![
            Value::Varchar("bad".into()),
            Value::Varchar("also bad".into()),
        ],
    )?)
    .select((0..32).map(|i| i % 2).collect())?;
    let input = DataChunk::new(vec![dictionary], 32)?;
    let result = BatchedEvaluator.evaluate_batch(&expression, &input, &query)?;
    assert_eq!(result.constant_value(), Some(&Value::Null));
    assert_eq!(result.len(), 32);
    Ok(())
}

#[derive(Debug)]
struct RejectMetadata {
    resource: bool,
    metadata: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for RejectMetadata {
    fn name(&self) -> &'static str {
        "selected-comparison-null-validator"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        if !self.metadata {
            return PrimitiveTypes.validate_type(ty);
        }
        Err(if self.resource {
            Error::Resource("selected comparison result metadata".into())
        } else {
            Error::Conversion("selected comparison result metadata".into())
        })
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        if self.metadata {
            return PrimitiveTypes.validate_value(ty, value, query);
        }
        Err(if self.resource {
            Error::Resource("required earlier operand validation".into())
        } else {
            Error::Conversion("required earlier operand validation".into())
        })
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(
        &self,
        ty: &DataType,
        a: &Value,
        b: &Value,
        q: &QueryContext,
    ) -> Result<std::cmp::Ordering> {
        PrimitiveTypes.compare(ty, a, b, q)
    }
    fn write_key(
        &self,
        ty: &DataType,
        v: &Value,
        w: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(ty, v, w, q)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn constant_null_comparisons_keep_retained_operand_and_result_validation_fatal() -> Result<()> {
    for family in ["builtin.boolean", "builtin.bigint"] {
        for resource in [false, true] {
            let mut types = TypeRegistry::builtins();
            let metadata = family == "builtin.boolean";
            types.replace(family, Arc::new(RejectMetadata { resource, metadata }))?;
            let query = QueryContext::background().with_types(Arc::new(types));
            let expression = binary(
                BinaryOp::Equal,
                BoundExpr::column(0, DataType::BigInt),
                BoundExpr::column(1, DataType::BigInt),
                &query,
            )?;
            let input = DataChunk::new(
                vec![
                    if metadata {
                        Vector::constant(DataType::BigInt, Value::Null, 2)?
                    } else {
                        Vector::flat(DataType::BigInt, vec![Value::Integer(1); 2])?
                    },
                    Vector::constant(DataType::BigInt, Value::Null, 2)?,
                ],
                2,
            )?;
            for evaluator in [
                Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                Arc::new(BatchedEvaluator),
            ] {
                let result = evaluator.evaluate_batch(&expression, &input, &query);
                if resource {
                    assert!(matches!(result, Err(Error::Resource(_))));
                } else {
                    assert!(
                        matches!(result, Err(Error::Internal(_))),
                        "{family}/{}: {result:?}",
                        evaluator.name()
                    );
                }
            }
        }
    }
    // NULL payload validity is universal in BoundType, but the retained operand
    // and declared child/result metadata must still agree at the skip boundary.
    let query = QueryContext::background();
    for (child, result) in [
        (DataType::Integer, DataType::Boolean),
        (DataType::BigInt, DataType::BigInt),
    ] {
        let mut expression = binary(
            BinaryOp::Equal,
            BoundExpr::column(0, child.clone()),
            BoundExpr::column(1, DataType::BigInt),
            &query,
        )?;
        expression.data_type = result;
        let input = DataChunk::new(
            vec![
                Vector::constant(child, Value::Null, 2)?,
                Vector::flat(DataType::BigInt, vec![Value::Integer(1); 2])?,
            ],
            2,
        )?;
        for evaluator in [
            Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
            Arc::new(BatchedEvaluator),
        ] {
            assert!(matches!(
                evaluator.evaluate_batch(&expression, &input, &query),
                Err(Error::Internal(_))
            ));
        }
    }
    Ok(())
}
