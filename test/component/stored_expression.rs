//! The stored-tree prerequisite is not yet catalog/default persistence support.
use super::*;
use duckdb_rust::{
    catalog::expression::{
        StoredArgument, StoredArgumentStyle, StoredExpression, StoredExpressionKind,
    },
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec},
        type_registry::TypeRegistry,
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionEffects, operator::OperatorRegistry},
    parallel::InterruptHandle,
    planner::{
        BindContext, Binder, BoundExpr, BoundStatement, ExprKind, Field, LogicalPlan, PlanNode,
        SqlBinder,
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn call(name: &str, arguments: Vec<StoredExpression>) -> StoredExpression {
    StoredExpression {
        alias: None,
        kind: StoredExpressionKind::Function {
            name: vec![name.into()],
            arguments: arguments
                .into_iter()
                .map(|expression| StoredArgument {
                    name: None,
                    expression,
                })
                .collect(),
            is_operator: false,
            argument_style: StoredArgumentStyle::Named,
        },
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn cast(expression: StoredExpression, target: DataType, try_cast: bool) -> StoredExpression {
    StoredExpression {
        alias: None,
        kind: StoredExpressionKind::Cast {
            expression: Box::new(expression),
            target,
            try_cast,
        },
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bind(
    expression: &StoredExpression,
    binder: &dyn Binder,
    functions: &FunctionRegistry,
    casts: &CastRegistry,
    evaluator: &dyn ExpressionEvaluator,
    query: &QueryContext,
) -> Result<BoundExpr> {
    let catalog = Snapshot::new(query.type_registry());
    let operators = OperatorRegistry::builtins();
    let bound = binder.bind_stored_expression(
        expression,
        &BindContext {
            catalog: &catalog,
            functions,
            casts,
            operators: &operators,
            expressions: evaluator,
            query,
            parameters: &[],
        },
    )?;
    LogicalPlan {
        schema: vec![Field::new("stored", bound.data_type.clone())],
        node: PlanNode::Values(vec![vec![bound.clone()]]),
    }
    .validate(&catalog, query)?;
    Ok(bound)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stored_literals_and_calls_preserve_typed_metadata_and_defer_evaluation() -> Result<()> {
    let query = QueryContext::background();
    let functions = FunctionRegistry::builtins();
    let casts = CastRegistry::builtins();
    for evaluator in [
        &ScalarEvaluator as &dyn ExpressionEvaluator,
        &BatchedEvaluator,
    ] {
        for (expression, ty, expected) in [
            (
                StoredExpression::literal(DataType::TinyInt, Value::Integer(1)),
                DataType::TinyInt,
                Value::Integer(1),
            ),
            (
                StoredExpression::literal(DataType::UHugeInt, Value::Null),
                DataType::UHugeInt,
                Value::Null,
            ),
            (
                StoredExpression::literal(DataType::UHugeInt, Value::Unsigned(u128::MAX)),
                DataType::UHugeInt,
                Value::Unsigned(u128::MAX),
            ),
            (
                call(
                    "from_base64",
                    vec![StoredExpression::literal(
                        DataType::Varchar,
                        Value::Varchar("AP8=".into()),
                    )],
                ),
                DataType::Blob,
                Value::Blob(vec![0, 255]),
            ),
            (
                call(
                    "abs",
                    vec![StoredExpression::literal(
                        DataType::SmallInt,
                        Value::Integer(-9),
                    )],
                ),
                DataType::SmallInt,
                Value::Integer(9),
            ),
        ] {
            let encoded = serde_json::to_vec(&expression).unwrap();
            let decoded: StoredExpression = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(expression, decoded);
            let bound = bind(&decoded, &SqlBinder, &functions, &casts, evaluator, &query)?;
            assert_eq!(bound.data_type, ty);
            assert_eq!(evaluator.evaluate(&bound, &vec![], &query)?, expected);
        }
        let bad = cast(
            StoredExpression::literal(DataType::Varchar, Value::Varchar("bad".into())),
            DataType::Integer,
            false,
        );
        let bound = bind(&bad, &SqlBinder, &functions, &casts, evaluator, &query)?;
        assert!(matches!(
            evaluator.evaluate(&bound, &vec![], &query),
            Err(Error::Conversion(_))
        ));
        let lazy = call(
            "coalesce",
            vec![
                StoredExpression::literal(DataType::Integer, Value::Integer(4)),
                bad,
            ],
        );
        let bound = bind(&lazy, &SqlBinder, &functions, &casts, evaluator, &query)?;
        assert_eq!(
            evaluator.evaluate(&bound, &vec![], &query)?,
            Value::Integer(4)
        );
        let explicit = cast(
            StoredExpression::literal(DataType::Integer, Value::Integer(1)),
            DataType::Integer,
            false,
        );
        let bound = bind(&explicit, &SqlBinder, &functions, &casts, evaluator, &query)?;
        assert!(matches!(bound.kind, ExprKind::Cast(_, _, false)));
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedCast(i128);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedCast {
    fn name(&self) -> &'static str {
        "stored-selected-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target == DataType::Integer
            && spec.mode == CastMode::Explicit
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match value {
            Value::Varchar(v) if v == "resource" => {
                Err(Error::Resource("stored selected resource".into()))
            }
            Value::Varchar(v) if v == "invalid" => Ok(Value::Varchar("wrong type".into())),
            Value::Varchar(v) if v == "conversion" => {
                Err(Error::Conversion("stored selected conversion".into()))
            }
            _ => Ok(Value::Integer(self.0)),
        }
    }
}

#[derive(Debug)]
struct SelectedFunction(i128, bool, Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for SelectedFunction {
    fn name(&self) -> &str {
        "stored_function"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: self.1,
            external_access: false,
        }
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Integer)
    }
    fn evaluate(&self, _: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.2.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Integer(self.0))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stored_binding_retains_selected_functions_casts_and_fatal_try_cast_failures() -> Result<()> {
    let query = QueryContext::background();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::default();
    functions.register_scalar(Arc::new(SelectedFunction(7, false, calls.clone())))?;
    let mut casts = CastRegistry::default();
    casts.register(
        CastSpec {
            source: DataType::Varchar,
            target: DataType::Integer,
            mode: CastMode::Explicit,
        },
        Arc::new(SelectedCast(13)),
    )?;
    let function = bind(
        &call("stored_function", vec![]),
        &SqlBinder,
        &functions,
        &casts,
        &ScalarEvaluator,
        &query,
    )?;
    let selected = cast(
        StoredExpression::literal(DataType::Varchar, Value::Varchar("x".into())),
        DataType::Integer,
        false,
    );
    let bound_cast = bind(
        &selected,
        &SqlBinder,
        &functions,
        &casts,
        &ScalarEvaluator,
        &query,
    )?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "ordinary binding cannot execute defaults"
    );
    for (text, expected_null) in [
        ("conversion", true),
        ("resource", false),
        ("invalid", false),
    ] {
        let expression = cast(
            StoredExpression::literal(DataType::Varchar, Value::Varchar(text.into())),
            DataType::Integer,
            true,
        );
        let bound = bind(
            &expression,
            &SqlBinder,
            &functions,
            &casts,
            &ScalarEvaluator,
            &query,
        )?;
        let result = ScalarEvaluator.evaluate(&bound, &vec![], &query);
        if expected_null {
            assert_eq!(result?, Value::Null);
        } else if text == "resource" {
            assert!(matches!(result, Err(Error::Resource(_))));
        } else {
            assert!(matches!(result, Err(Error::Internal(_))));
        }
    }
    // A newly composed registry does not replace adapters already bound above.
    drop(functions);
    drop(casts);
    let mut replacement = FunctionRegistry::default();
    replacement.register_scalar(Arc::new(SelectedFunction(99, false, calls.clone())))?;
    assert_eq!(
        ScalarEvaluator.evaluate(&function, &vec![], &query)?,
        Value::Integer(7)
    );
    assert_eq!(
        ScalarEvaluator.evaluate(&bound_cast, &vec![], &query)?,
        Value::Integer(13)
    );
    let rebound = bind(
        &call("stored_function", vec![]),
        &SqlBinder,
        &replacement,
        &CastRegistry::default(),
        &ScalarEvaluator,
        &query,
    )?;
    assert_eq!(
        ScalarEvaluator.evaluate(&rebound, &vec![], &query)?,
        Value::Integer(99)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}

struct OrdinaryBinder;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Binder for OrdinaryBinder {
    fn name(&self) -> &'static str {
        "ordinary-only"
    }
    fn bind(
        &self,
        statement: &duckdb_rust::parser::Statement,
        context: &BindContext<'_>,
    ) -> Result<BoundStatement> {
        SqlBinder.bind(statement, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stored_trees_reject_malformed_metadata_limits_effects_and_missing_capabilities() -> Result<()> {
    let query = QueryContext::background();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(SelectedFunction(1, true, calls.clone())))?;
    let casts = CastRegistry::builtins();
    let check = |expression: &StoredExpression| {
        bind(
            expression,
            &SqlBinder,
            &functions,
            &casts,
            &ScalarEvaluator,
            &query,
        )
    };
    assert!(matches!(
        check(&call("stored_function", vec![])),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let literal = StoredExpression::literal(DataType::Integer, Value::Integer(1));
    assert!(matches!(
        bind(
            &literal,
            &OrdinaryBinder,
            &functions,
            &casts,
            &ScalarEvaluator,
            &query
        ),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        check(&StoredExpression::literal(
            DataType::TinyInt,
            Value::Integer(256)
        )),
        Err(Error::Conversion(_))
    ));
    assert!(
        check(&StoredExpression::literal(
            DataType::Integer,
            Value::Blob(vec![1])
        ))
        .is_err()
    );
    let mut deep = literal.clone();
    for _ in 0..65 {
        deep = cast(deep, DataType::Integer, false);
    }
    assert!(matches!(check(&deep), Err(Error::Resource(_))));
    assert!(matches!(
        check(&call("abs", vec![literal.clone(); 16_384])),
        Err(Error::Resource(_))
    ));
    assert!(matches!(
        check(&call(&"a".repeat(16 * 1024 * 1024 + 1), vec![])),
        Err(Error::Resource(_))
    ));
    assert!(matches!(check(&call("", vec![])), Err(Error::Bind(_))));
    for kind in 0..3 {
        let mut expression = call("abs", vec![literal.clone()]);
        if let StoredExpressionKind::Function {
            name,
            arguments,
            is_operator,
            ..
        } = &mut expression.kind
        {
            match kind {
                0 => name.insert(0, "foreign".into()),
                1 => arguments[0].name = Some("arg".into()),
                _ => *is_operator = true,
            }
        }
        assert!(matches!(check(&expression), Err(Error::Unsupported(_))));
    }
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 100)?;
    interrupt.interrupt();
    assert!(matches!(
        bind(
            &literal,
            &SqlBinder,
            &functions,
            &casts,
            &ScalarEvaluator,
            &cancelled
        ),
        Err(Error::Interrupted)
    ));
    assert!(
        matches!(check(&call("missing", vec![call("also_missing", vec![])])), Err(Error::Catalog(message)) if message == "Scalar Function with name missing does not exist!")
    );
    Ok(())
}

struct ReturnedBinder(BoundExpr);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Binder for ReturnedBinder {
    fn name(&self) -> &'static str {
        "stored-returned-binder"
    }
    fn bind(
        &self,
        _: &duckdb_rust::parser::Statement,
        _: &BindContext<'_>,
    ) -> Result<BoundStatement> {
        Err(Error::Unsupported(
            "ordinary binding on returned binder".into(),
        ))
    }
    fn bind_stored_expression(
        &self,
        _: &StoredExpression,
        _: &BindContext<'_>,
    ) -> Result<BoundExpr> {
        Ok(self.0.clone())
    }
}

struct ReturnedEvaluator(Value, Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for ReturnedEvaluator {
    fn name(&self) -> &'static str {
        "stored-returned-evaluator"
    }
    fn evaluate(
        &self,
        _: &BoundExpr,
        row: &duckdb_rust::common::Row,
        context: &dyn duckdb_rust::execution::expression_executor::EvaluationContext,
    ) -> Result<Value> {
        context.query().check()?;
        assert!(row.is_empty());
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(self.0.clone())
    }
}

#[derive(Debug)]
struct AssignmentCast(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for AssignmentCast {
    fn name(&self) -> &'static str {
        "stored-assignment-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer
            && spec.target == DataType::Varchar
            && spec.mode == CastMode::Assignment
    }
    fn cast(&self, _: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        assert_eq!(spec.mode, CastMode::Assignment);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Varchar("selected assignment".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stored_service_retains_selected_assignment_and_evaluates_once() -> Result<()> {
    use duckdb_rust::planner::stored::SelectedStoredExpressions;
    let calls = Arc::new(AtomicUsize::new(0));
    let cast_calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::default();
    functions.register_scalar(Arc::new(SelectedFunction(7, false, calls.clone())))?;
    let mut casts = CastRegistry::default();
    casts.register(
        CastSpec {
            source: DataType::Integer,
            target: DataType::Varchar,
            mode: CastMode::Assignment,
        },
        Arc::new(AssignmentCast(cast_calls.clone())),
    )?;
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let service = Arc::new(SelectedStoredExpressions::new(
            Arc::new(SqlBinder),
            casts.clone(),
            OperatorRegistry::builtins(),
            functions.clone(),
            evaluator,
        ));
        let query = QueryContext::background().with_stored_expressions(service);
        let catalog = Snapshot::new(query.type_registry());
        let previous = calls.load(Ordering::SeqCst);
        let previous_casts = cast_calls.load(Ordering::SeqCst);
        let expression = call("stored_function", vec![]);
        assert_eq!(
            query.stored_expressions()?.evaluate(
                &expression,
                &DataType::Varchar,
                &catalog,
                &query
            )?,
            Value::Varchar("selected assignment".into())
        );
        assert_eq!(calls.load(Ordering::SeqCst), previous + 1);
        assert_eq!(cast_calls.load(Ordering::SeqCst), previous_casts + 1);
        let cloned = query.clone();
        assert_eq!(
            cloned.stored_expressions()?.evaluate(
                &expression,
                &DataType::Integer,
                &catalog,
                &cloned
            )?,
            Value::Integer(7)
        );
        assert_eq!(calls.load(Ordering::SeqCst), previous + 2);
        assert_eq!(cast_calls.load(Ordering::SeqCst), previous_casts + 1);
    }
    assert!(matches!(
        QueryContext::background().stored_expressions(),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stored_service_validates_replacement_boundaries_before_evaluation() -> Result<()> {
    use duckdb_rust::{
        catalog::expression::StoredExpressionEvaluator, planner::stored::SelectedStoredExpressions,
    };
    let query = QueryContext::background();
    let catalog = Snapshot::new(query.type_registry());
    let literal = StoredExpression::literal(DataType::Integer, Value::Integer(1));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut bad_literal = BoundExpr::literal(Value::Integer(1));
    bad_literal.data_type = DataType::Varchar;
    let mut column = BoundExpr::literal(Value::Integer(1));
    column.kind = ExprKind::Column(0);
    let effects = Arc::new(AtomicUsize::new(0));
    let mut effectful = BoundExpr::literal(Value::Integer(1));
    effectful.kind = ExprKind::Scalar(Arc::new(SelectedFunction(9, true, effects.clone())), vec![]);
    for bound in [bad_literal, column, effectful] {
        let service = SelectedStoredExpressions::new(
            Arc::new(ReturnedBinder(bound)),
            CastRegistry::builtins(),
            OperatorRegistry::builtins(),
            FunctionRegistry::builtins(),
            Arc::new(ReturnedEvaluator(Value::Integer(42), calls.clone())),
        );
        assert!(
            service
                .evaluate(&literal, &DataType::Integer, &catalog, &query)
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
    for (value, valid) in [
        (Value::Integer(42), true),
        (Value::Varchar("invalid result".into()), false),
    ] {
        let service = SelectedStoredExpressions::new(
            Arc::new(SqlBinder),
            CastRegistry::builtins(),
            OperatorRegistry::builtins(),
            FunctionRegistry::builtins(),
            Arc::new(ReturnedEvaluator(value.clone(), calls.clone())),
        );
        let result = service.evaluate(&literal, &DataType::Integer, &catalog, &query);
        if valid {
            assert_eq!(result?, value);
        } else {
            assert!(matches!(result, Err(Error::Internal(_))));
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let service = SelectedStoredExpressions::new(
        Arc::new(OrdinaryBinder),
        CastRegistry::builtins(),
        OperatorRegistry::builtins(),
        FunctionRegistry::builtins(),
        Arc::new(ReturnedEvaluator(Value::Integer(42), calls.clone())),
    );
    assert!(matches!(
        service.evaluate(&literal, &DataType::Integer, &catalog, &query),
        Err(Error::Unsupported(_))
    ));
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
    interrupt.interrupt();
    assert!(matches!(
        service.evaluate(&literal, &DataType::Integer, &catalog, &cancelled),
        Err(Error::Interrupted)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}
