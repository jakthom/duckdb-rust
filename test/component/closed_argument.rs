use super::*;
use duckdb_rust::{
    common::type_registry::TypeRegistry,
    execution::expression_executor::{
        BatchedEvaluator, EvaluationContext, ExpressionEvaluator, ScalarEvaluator,
    },
    function::{ArgumentEvaluation, FunctionEffects, ScalarBindArguments},
    optimizer::{Optimizer, PipelineOptimizer},
    parallel::InterruptHandle,
    planner::{BoundExpr, ExprKind},
};

type InterruptSlot = Arc<std::sync::Mutex<Option<InterruptHandle>>>;

#[derive(Clone)]
struct ClosedProbe {
    index: usize,
    value: Option<bool>,
    interrupt: Option<InterruptSlot>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for ClosedProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClosedProbe")
            .field("index", &self.index)
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ClosedProbe {
    fn name(&self) -> &str {
        "closed_argument"
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        if let Some(interrupt) = &self.interrupt {
            interrupt.lock().unwrap().as_ref().unwrap().interrupt();
        }
        Ok(Some(Arc::new(Self {
            value: Some(arguments.is_closed(self.index)?),
            ..self.clone()
        })))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::TypeOnly
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Boolean)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        assert!(arguments.is_empty());
        self.value
            .map(Value::Boolean)
            .ok_or_else(|| Error::Internal("unbound closed metadata probe".into()))
    }
}

#[derive(Debug)]
struct EffectfulChild(bool);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for EffectfulChild {
    fn name(&self) -> &str {
        if self.0 {
            "closed_volatile"
        } else {
            "closed_external"
        }
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: self.0,
            external_access: !self.0,
        }
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Integer)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        panic!("metadata classification must not execute an effectful child")
    }
}

struct RejectCastEvaluator(bool);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for RejectCastEvaluator {
    fn name(&self) -> &'static str {
        "closed-metadata-no-evaluation"
    }
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Vec<Value>,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        if matches!(expression.kind, ExprKind::Cast(..)) {
            return Err(if self.0 {
                Error::Resource("closed metadata child resource".into())
            } else {
                Error::Internal("closed metadata child internal".into())
            });
        }
        ScalarEvaluator.evaluate(expression, row, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn registry(index: usize, interrupt: Option<InterruptSlot>) -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(ClosedProbe {
        index,
        value: None,
        interrupt,
    }))?;
    functions.register_scalar(Arc::new(EffectfulChild(true)))?;
    functions.register_scalar(Arc::new(EffectfulChild(false)))?;
    Ok(functions)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn closed_metadata_retains_dependencies_parameters_and_lazy_failing_children() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
        Arc::new(RejectCastEvaluator(false)),
        Arc::new(RejectCastEvaluator(true)),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .functions(registry(0, None)?)
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            let result = c.query("SELECT closed_argument('bad'::INTEGER),closed_argument(i),closed_argument(1+2),closed_argument(NULL),closed_argument(closed_volatile()),closed_argument(closed_external()),closed_argument(CASE WHEN false THEN 'bad'::INTEGER ELSE 1 END) FROM range(3) t(i)")?;
            assert_eq!(
                result.rows,
                vec![
                    vec![
                        Value::Boolean(true),
                        Value::Boolean(false),
                        Value::Boolean(true),
                        Value::Boolean(true),
                        Value::Boolean(false),
                        Value::Boolean(false),
                        Value::Boolean(true)
                    ];
                    3
                ]
            );
            assert_eq!(
                c.query("SELECT closed_argument((SELECT 1))")?.rows,
                vec![vec![Value::Boolean(false)]]
            );
            let prepared =
                c.prepare("SELECT closed_argument($1),closed_argument(CAST($1 AS INTEGER))")?;
            for value in [Value::Varchar("bad".into()), Value::Integer(7), Value::Null] {
                assert_eq!(
                    c.execute_prepared(&prepared, &[value])?.rows,
                    vec![vec![Value::Boolean(true), Value::Boolean(true)]]
                );
            }
            assert!(matches!(
                c.query("SELECT closed_argument()"),
                Err(Error::Bind(_))
            ));
        }
    }
    Ok(())
}

struct ExternalArguments;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for ExternalArguments {
    fn len(&self) -> usize {
        1
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        if index == 0 {
            Ok(DataType::Integer)
        } else {
            Err(Error::Bind("outside signature".into()))
        }
    }
    fn constant(&self, _: usize) -> Result<Value> {
        panic!("default metadata request must not evaluate a constant")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn closed_metadata_validates_missing_capabilities_positions_and_cancellation() -> Result<()> {
    assert!(matches!(
        ExternalArguments.is_closed(0),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        ExternalArguments.is_closed(1),
        Err(Error::Bind(_))
    ));
    let mut c = DatabaseBuilder::new()
        .functions(registry(1, None)?)
        .build()?
        .connect();
    assert!(matches!(
        c.query("SELECT closed_argument(1)"),
        Err(Error::Bind(_))
    ));
    let slot = Arc::new(std::sync::Mutex::new(None));
    let mut connection = DatabaseBuilder::new()
        .functions(registry(0, Some(slot.clone()))?)
        .build()?
        .connect();
    // The selected function interrupts immediately before requesting metadata,
    // after SQL's ordinary initial cancellation checks have already succeeded.
    *slot.lock().unwrap() = Some(connection.interrupt_handle());
    assert!(matches!(
        connection.query("SELECT closed_argument(1)"),
        Err(Error::Interrupted)
    ));
    Ok(())
}
