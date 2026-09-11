use super::*;
use duckdb_rust::{
    common::Row,
    execution::expression_executor::{EvaluationContext, ExpressionEvaluator},
    function::ArgumentProvenance,
    planner::BoundExpr,
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
