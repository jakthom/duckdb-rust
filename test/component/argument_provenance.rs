use super::*;
use duckdb_rust::{
    common::Row,
    execution::expression_executor::{
        BatchedEvaluator, EvaluatedValue, EvaluationContext, ExpressionEvaluator, ScalarEvaluator,
    },
    function::{ArgumentEvaluation, ArgumentProvenance, FunctionEffects},
    optimizer::{Optimizer, PipelineOptimizer},
    planner::{BoundExpr, ExprKind},
};

#[derive(Debug)]
struct OrdinaryFunction(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for OrdinaryFunction {
    fn name(&self) -> &str {
        "ordinary_provenance"
    }
    fn return_type(
        &self,
        _: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        Ok(DataType::BigInt)
    }
    fn evaluate(&self, arguments: &[Value], _: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(arguments[0].clone())
    }
}

struct OrdinaryEvaluator(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for OrdinaryEvaluator {
    fn name(&self) -> &'static str {
        "ordinary-provenance"
    }
    fn evaluate(&self, _: &BoundExpr, _: &Row, _: &dyn EvaluationContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Integer(19))
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn provenance_defaults_retain_selected_callbacks_without_guessing_constant() -> Result<()> {
    let query = QueryContext::background();
    let calls = Arc::new(AtomicUsize::new(0));
    let function = OrdinaryFunction(calls.clone());
    for provenance in [ArgumentProvenance::Unknown, ArgumentProvenance::Constant] {
        assert_eq!(
            function.evaluate_with_provenance(&[Value::Integer(7)], &[provenance], &query)?,
            Value::Integer(7)
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        function.evaluate_with_provenance(&[Value::Integer(7)], &[], &query),
        Err(Error::Internal(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let evaluator = OrdinaryEvaluator(calls.clone());
    let expression = BoundExpr::literal(Value::Integer(42));
    let result = evaluator.evaluate_with_provenance(&expression, &vec![], &query)?;
    assert_eq!(result.value, Value::Integer(19));
    assert_eq!(result.provenance, ArgumentProvenance::Unknown);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    Ok(())
}

#[derive(Debug)]
struct ProvenanceProbe;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ProvenanceProbe {
    fn name(&self) -> &str {
        "argument_is_constant"
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments.len() != 1 {
            return Err(Error::Bind("provenance probe needs one argument".into()));
        }
        Ok(DataType::Boolean)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Ok(Value::Boolean(false))
    }
    fn evaluate_with_provenance(
        &self,
        arguments: &[Value],
        provenance: &[ArgumentProvenance],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        assert_eq!(arguments.len(), 1);
        assert_eq!(provenance.len(), 1);
        Ok(Value::Boolean(
            provenance[0] == ArgumentProvenance::Constant,
        ))
    }
}

#[derive(Debug)]
struct VolatileValue(Arc<AtomicUsize>, bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for VolatileValue {
    fn name(&self) -> &str {
        if self.1 {
            "provenance_external"
        } else {
            "provenance_volatile"
        }
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: !self.1,
            external_access: self.1,
        }
    }
    fn return_type(
        &self,
        _: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Varchar("bad".into()))
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn execution_provenance_keeps_values_projections_subquery_cache_effects_and_preparation_distinct()
-> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for size in [1, 2, 5] {
                let calls = Arc::new(AtomicUsize::new(0));
                let mut functions = FunctionRegistry::builtins();
                functions.register_scalar(Arc::new(ProvenanceProbe))?;
                functions.register_scalar(Arc::new(VolatileValue(calls.clone(), false)))?;
                functions.register_scalar(Arc::new(VolatileValue(calls.clone(), true)))?;
                let mut c = DatabaseBuilder::new()
                    .functions(functions)
                    .expressions(evaluator.clone())
                    .optimizer(optimizer.clone())
                    .batch_size(size)
                    .build()?
                    .connect();
                for (sql, expected, count) in [
                    (
                        "SELECT argument_is_constant(p) FROM (SELECT 'bad' p)",
                        true,
                        1,
                    ),
                    (
                        "SELECT argument_is_constant(p) FROM (VALUES ('bad')) t(p)",
                        false,
                        1,
                    ),
                    (
                        "SELECT argument_is_constant(p) FROM (VALUES ('bad'),('bad')) t(p)",
                        false,
                        2,
                    ),
                    (
                        "SELECT argument_is_constant(p) FROM (SELECT 'bad' p FROM range(3))",
                        true,
                        3,
                    ),
                    (
                        "SELECT argument_is_constant(p) FROM (SELECT 'bad' p,concat('x','y') x FROM range(3))",
                        true,
                        3,
                    ),
                    (
                        "SELECT argument_is_constant((SELECT 'bad')) FROM range(3)",
                        true,
                        3,
                    ),
                    (
                        "SELECT argument_is_constant((SELECT p FROM (VALUES ('bad')) t(p)))",
                        true,
                        1,
                    ),
                    (
                        "SELECT argument_is_constant((SELECT i)) FROM range(3) t(i)",
                        false,
                        3,
                    ),
                    (
                        "SELECT argument_is_constant(concat('b','ad')) FROM range(3)",
                        true,
                        3,
                    ),
                ] {
                    assert_eq!(
                        c.query(sql)?.rows,
                        vec![vec![Value::Boolean(expected)]; count],
                        "{sql}; batch={size}; evaluator={}",
                        evaluator.name()
                    );
                }
                let prepared = c.prepare("SELECT argument_is_constant($1),argument_is_constant(p) FROM (SELECT $2 p FROM range(3))")?;
                for values in [
                    vec![Value::Varchar("bad".into()), Value::Varchar("day".into())],
                    vec![Value::Integer(7), Value::Null],
                ] {
                    assert_eq!(
                        c.execute_prepared(&prepared, &values)?.rows,
                        vec![vec![Value::Boolean(true); 2]; 3]
                    );
                }
                assert!(
                    c.query("SELECT argument_is_constant(provenance_volatile()) FROM range(0)")?
                        .rows
                        .is_empty()
                );
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert_eq!(c.query("SELECT CASE WHEN false THEN argument_is_constant(provenance_external()) ELSE true END FROM range(3)")?.rows, vec![vec![Value::Boolean(true)];3]);
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert_eq!(c.query("SELECT argument_is_constant(provenance_volatile()),argument_is_constant(provenance_external()) FROM range(3)")?.rows, vec![vec![Value::Boolean(false);2];3]);
                assert_eq!(calls.load(Ordering::SeqCst), 6);
            }
        }
    }
    Ok(())
}

struct LaterInvalidConstant(Arc<AtomicUsize>, u8);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for LaterInvalidConstant {
    fn name(&self) -> &'static str {
        "later-invalid-constant"
    }
    fn evaluate(&self, _: &BoundExpr, _: &Row, _: &dyn EvaluationContext) -> Result<Value> {
        panic!("selected encoded callback must be retained")
    }
    fn evaluate_with_provenance(
        &self,
        expression: &BoundExpr,
        _: &Row,
        _: &dyn EvaluationContext,
    ) -> Result<EvaluatedValue> {
        let second = self.0.fetch_add(1, Ordering::SeqCst) == 1;
        let value = if self.1 == 3 {
            Value::extension(
                expression.data_type.clone(),
                if second { vec![255] } else { vec![b'x'] },
            )
        } else if second {
            match self.1 {
                0 => Value::Varchar("invalid later BIGINT".into()),
                1 => return Err(Error::Resource("later selected resource".into())),
                _ => return Err(Error::Internal("later selected internal".into())),
            }
        } else {
            Value::Integer(7)
        };
        Ok(EvaluatedValue {
            value,
            provenance: ArgumentProvenance::Constant,
        })
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn constant_result_metadata_cannot_discard_later_invalid_values_or_errors() -> Result<()> {
    let query = QueryContext::background();
    let input = DataChunk::new(vec![Vector::flat(DataType::BigInt, integers(&[1, 2]))?], 2)?;
    let expression = BoundExpr {
        kind: ExprKind::Column(0),
        data_type: DataType::BigInt,
    };
    for failure in 0..3 {
        let calls = Arc::new(AtomicUsize::new(0));
        let evaluator = LaterInvalidConstant(calls.clone(), failure);
        let error = evaluator
            .evaluate_batch(&expression, &input, &query)
            .unwrap_err();
        assert!(match failure {
            1 => matches!(error, Error::Resource(_)),
            _ => matches!(error, Error::Internal(_)),
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    use duckdb_rust::common::type_registry::{TypeRegistry, ascii};
    let mut types = TypeRegistry::builtins();
    types.register(ascii::FAMILY, Arc::new(ascii::MaterializedAscii))?;
    let query = query.with_types(Arc::new(types));
    let expression = BoundExpr {
        kind: ExprKind::Column(0),
        data_type: ascii::data_type(8)?,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let evaluator = LaterInvalidConstant(calls.clone(), 3);
    assert!(
        matches!(evaluator.evaluate_batch(&expression,&input,&query),Err(Error::Internal(message)) if message.contains("invalid logical constant"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn physical_selection_preserves_constant_encoding_without_promoting_equal_dictionary_rows()
-> Result<()> {
    let constant = Arc::new(Vector::constant(DataType::BigInt, Value::Integer(7), 3)?);
    let flat = Arc::new(Vector::flat(DataType::BigInt, integers(&[7, 7, 7]))?);
    for selection in [vec![], vec![0], vec![2, 0, 2]] {
        let selected = constant.select(selection.clone())?;
        assert_eq!(selected.constant_value(), Some(&Value::Integer(7)));
        assert_eq!(selected.len(), selection.len());
        let selected = flat.select(selection)?;
        assert_eq!(selected.constant_value(), None);
        assert!(selected.dictionary().is_some());
    }
    Ok(())
}

struct DelegatingEvaluator(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for DelegatingEvaluator {
    fn name(&self) -> &'static str {
        "delegating-provenance"
    }
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        ScalarEvaluator.evaluate(expression, row, context)
    }
    fn evaluate_with_provenance(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<EvaluatedValue> {
        if self.0 {
            ScalarEvaluator.evaluate_with_provenance(expression, row, context)
        } else {
            self.evaluate(expression, row, context)
                .map(|value| EvaluatedValue {
                    value,
                    provenance: ArgumentProvenance::Unknown,
                })
        }
    }
}

struct ValueOnlyDelegating;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for ValueOnlyDelegating {
    fn name(&self) -> &'static str {
        "value-only-delegating"
    }
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Row,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        ScalarEvaluator.evaluate(expression, row, context)
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn replacement_evaluators_own_their_provenance_and_cancellation_is_checked_for_empty_input()
-> Result<()> {
    for aware in [false, true] {
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(ProvenanceProbe))?;
        let mut c = DatabaseBuilder::new()
            .functions(functions)
            .expressions(if aware {
                Arc::new(DelegatingEvaluator(true))
            } else {
                Arc::new(ValueOnlyDelegating)
            })
            .optimizer(Arc::new(IdentityOptimizer))
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(
            c.query("SELECT argument_is_constant(p) FROM (SELECT 'bad' p FROM range(3))")?
                .rows,
            vec![vec![Value::Boolean(aware)]; 3]
        );
    }
    let interrupt = duckdb_rust::parallel::InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2, 100)?;
    interrupt.interrupt();
    let expression = BoundExpr::literal(Value::Integer(7));
    let input = DataChunk::new(vec![], 0)?;
    assert!(matches!(
        ScalarEvaluator.evaluate_batch(&expression, &input, &query),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct NullPolicyProbe(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for NullPolicyProbe {
    fn name(&self) -> &str {
        "stop_on_constant_null"
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::NullOnConstant
    }
    fn return_type(
        &self,
        _: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        Ok(DataType::BigInt)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Integer(7))
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn constant_null_policy_is_selected_ordered_and_keeps_executed_type_validation_fatal() -> Result<()>
{
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let effects = Arc::new(AtomicUsize::new(0));
        let callbacks = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(NullPolicyProbe(callbacks.clone())))?;
        functions.register_scalar(Arc::new(VolatileValue(effects.clone(), false)))?;
        let mut c = DatabaseBuilder::new()
            .functions(functions)
            .expressions(evaluator)
            .optimizer(Arc::new(IdentityOptimizer))
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(c.query("SELECT stop_on_constant_null(p,provenance_volatile(),'bad'::DATE) FROM (SELECT NULL p FROM range(3))")?.rows,vec![vec![Value::Null];3]);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(callbacks.load(Ordering::SeqCst), 0);
        assert_eq!(c.query("SELECT stop_on_constant_null(provenance_volatile(),p,provenance_volatile()) FROM (SELECT NULL p FROM range(3))")?.rows,vec![vec![Value::Null];3]);
        assert_eq!(effects.load(Ordering::SeqCst), 3);
        assert_eq!(callbacks.load(Ordering::SeqCst), 0);
        assert_eq!(c.query("SELECT stop_on_constant_null(p,provenance_volatile()) FROM (VALUES (NULL),(NULL)) t(p)")?.rows,vec![vec![Value::Integer(7)];2]);
        assert_eq!(effects.load(Ordering::SeqCst), 5);
        assert_eq!(callbacks.load(Ordering::SeqCst), 2);
    }
    let query = QueryContext::background();
    for bad_result in [false, true] {
        let unknown = DataType::extension("unregistered_null_metadata", vec![]);
        let argument = BoundExpr {
            kind: ExprKind::Literal(Value::Null),
            data_type: if bad_result {
                DataType::Null
            } else {
                unknown.clone()
            },
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let expression = BoundExpr {
            kind: ExprKind::Scalar(Arc::new(NullPolicyProbe(calls.clone())), vec![argument]),
            data_type: if bad_result {
                unknown
            } else {
                DataType::BigInt
            },
        };
        assert!(
            matches!(ScalarEvaluator.evaluate(&expression,&vec![],&query),Err(Error::Unsupported(message)) if message.contains("unregistered_null_metadata"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    Ok(())
}
