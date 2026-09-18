use super::*;
use duckdb_rust::{
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec},
        type_registry::TypeRegistry,
    },
    execution::expression_executor::{
        BatchedEvaluator, EvaluationContext, ExpressionEvaluator, ScalarEvaluator,
    },
    function::{ArgumentEvaluation, FunctionEffects, ScalarBindArguments},
    optimizer::{Optimizer, PipelineOptimizer},
    planner::{BoundExpr, ExprKind},
};

#[derive(Debug, Clone)]
struct ConstantProbe {
    name: &'static str,
    mode: CastMode,
    index: usize,
    value: Option<Value>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ConstantProbe {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        let value = arguments.constant_as(self.index, &DataType::TinyInt, self.mode)?;
        Ok(Some(Arc::new(Self {
            value: Some(value),
            ..self.clone()
        })))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::TypeOnly
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::TinyInt)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        assert!(
            arguments.is_empty(),
            "the selected constant is not evaluated twice"
        );
        self.value
            .clone()
            .ok_or_else(|| Error::Internal("unbound typed constant probe".into()))
    }
}

#[derive(Debug)]
struct ConstantCast;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for ConstantCast {
    fn name(&self) -> &'static str {
        "selected-typed-constant"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target == DataType::TinyInt
            && spec.mode != CastMode::Implicit
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match value {
            Value::Varchar(text) if text == "conversion" => {
                Err(Error::Conversion("probe conversion".into()))
            }
            Value::Varchar(text) if text == "execution" => {
                Err(Error::Execution("probe execution".into()))
            }
            Value::Varchar(text) if text == "range" => Err(Error::OutOfRange("probe range".into())),
            Value::Varchar(text) if text == "input" => {
                Err(Error::InvalidInput("probe input".into()))
            }
            Value::Varchar(text) if text == "interrupted" => Err(Error::Interrupted),
            Value::Varchar(text) if text == "resource" => {
                Err(Error::Resource("typed constant resource".into()))
            }
            Value::Varchar(text) if text == "internal" => {
                Err(Error::Internal("typed constant internal".into()))
            }
            Value::Varchar(text) if text == "invalid" => {
                Ok(Value::Varchar("bad physical output".into()))
            }
            Value::Varchar(text) if text == "wide" => Ok(Value::Integer(256)),
            Value::Varchar(text) if text == "null" => Ok(Value::Null),
            Value::Varchar(_) => Ok(Value::Integer(if spec.mode == CastMode::Assignment {
                22
            } else {
                11
            })),
            _ => Err(Error::Internal("typed constant cast input".into())),
        }
    }
}

#[derive(Debug)]
struct EffectsProbe(bool, Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for EffectsProbe {
    fn name(&self) -> &str {
        if self.0 {
            "volatile_constant"
        } else {
            "external_constant"
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
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Integer(1))
    }
}

#[derive(Debug)]
struct OptionalProbe(Option<Value>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for OptionalProbe {
    fn name(&self) -> &str {
        "typed_optional"
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        let value = arguments
            .constant_if_closed(0)?
            .unwrap_or(Value::Integer(99));
        Ok(Some(Arc::new(Self(Some(value)))))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::TypeOnly
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::TinyInt)
    }
    fn evaluate(&self, arguments: &[Value], _: &QueryContext) -> Result<Value> {
        assert!(arguments.is_empty());
        self.0
            .clone()
            .ok_or_else(|| Error::Internal("unbound optional constant".into()))
    }
}

#[derive(Debug)]
struct NullProbe(Option<bool>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for NullProbe {
    fn name(&self) -> &str {
        "typed_null"
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        Ok(Some(Arc::new(Self(Some(arguments.is_provably_null(0)?)))))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::TypeOnly
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Boolean)
    }
    fn evaluate(&self, arguments: &[Value], _: &QueryContext) -> Result<Value> {
        assert!(arguments.is_empty());
        self.0
            .map(Value::Boolean)
            .ok_or_else(|| Error::Internal("unbound NULL probe".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn registry() -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(OptionalProbe(None)))?;
    functions.register_scalar(Arc::new(NullProbe(None)))?;
    for (name, mode, index) in [
        ("typed_implicit", CastMode::Implicit, 0),
        ("typed_explicit", CastMode::Explicit, 0),
        ("typed_assignment", CastMode::Assignment, 0),
        ("typed_missing", CastMode::Implicit, 1),
    ] {
        functions.register_scalar(Arc::new(ConstantProbe {
            name,
            mode,
            index,
            value: None,
        }))?;
    }
    Ok(functions)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_constants_retain_selected_cast_modes_literals_parameters_and_failures() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut functions = registry()?;
            functions.register_scalar(Arc::new(EffectsProbe(true, calls.clone())))?;
            functions.register_scalar(Arc::new(EffectsProbe(false, calls.clone())))?;
            let mut casts = CastRegistry::builtins();
            for mode in [CastMode::Explicit, CastMode::Assignment] {
                casts.replace(
                    CastSpec {
                        source: DataType::Varchar,
                        target: DataType::TinyInt,
                        mode,
                    },
                    Arc::new(ConstantCast),
                )?;
            }
            let mut c = DatabaseBuilder::new()
                .functions(functions)
                .casts(casts)
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            assert_eq!(c.query("SELECT typed_implicit('1'),typed_explicit('1'::VARCHAR),typed_assignment('1'),typed_implicit(1),typed_implicit(NULL),typed_explicit(NULL::VARCHAR)")?.rows,vec![vec![Value::Integer(11),Value::Integer(11),Value::Integer(22),Value::Integer(1),Value::Null,Value::Null]]);
            assert_eq!(c.query("SELECT typed_explicit(1+2),typed_explicit(CASE WHEN true THEN 4 ELSE 5 END) FROM range(3)")?.rows,vec![vec![Value::Integer(3),Value::Integer(4)];3]);
            for sql in [
                "SELECT typed_implicit('1'::VARCHAR)",
                "SELECT typed_implicit(1::INTEGER)",
                "SELECT typed_implicit(CASE WHEN true THEN 1 ELSE 2 END)",
                "SELECT typed_implicit(i) FROM range(1) t(i)",
                "SELECT typed_missing(1)",
                "SELECT typed_implicit()",
                "SELECT typed_explicit(volatile_constant())",
                "SELECT typed_explicit(external_constant())",
            ] {
                assert!(matches!(c.query(sql), Err(Error::Bind(_))), "{sql}");
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(c.query("SELECT typed_null(NULL::INTEGER),typed_null(i),typed_null(volatile_constant()),typed_null(external_constant()),typed_null('1'::TINYINT) FROM range(1) t(i)")?.rows,vec![vec![Value::Boolean(true),Value::Boolean(false),Value::Boolean(false),Value::Boolean(false),Value::Boolean(false)]]);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(matches!(
                c.query("SELECT typed_null()"),
                Err(Error::Bind(_))
            ));
            for input in ["conversion", "execution", "range", "input"] {
                assert_eq!(
                    c.query(&format!("SELECT typed_null('{input}'::TINYINT)"))?
                        .rows,
                    vec![vec![Value::Boolean(false)]]
                );
                // Speculation does not weaken either required or optional
                // constant evaluation, nor execute a rejected expression later.
                for function in ["typed_implicit", "typed_optional"] {
                    let expression = if function == "typed_optional" {
                        format!("'{input}'::TINYINT")
                    } else {
                        format!("'{input}'")
                    };
                    let error = c
                        .query(&format!("SELECT {function}({expression})"))
                        .unwrap_err();
                    assert!(match input {
                        "conversion" => matches!(error, Error::Conversion(_)),
                        "execution" => matches!(error, Error::Execution(_)),
                        "range" => matches!(error, Error::OutOfRange(_)),
                        _ => matches!(error, Error::InvalidInput(_)),
                    });
                }
            }
            assert_eq!(c.query("SELECT typed_optional(i),typed_optional(volatile_constant()),typed_optional(external_constant()),typed_optional(NULL::INTEGER),typed_optional('1'::TINYINT) FROM range(1) t(i)")?.rows, vec![vec![Value::Integer(99),Value::Integer(99),Value::Integer(99),Value::Null,Value::Integer(11)]]);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(matches!(
                c.query("SELECT typed_optional()"),
                Err(Error::Bind(_))
            ));
            assert_eq!(
                c.execute_params("SELECT typed_optional($1)", &[Value::Integer(7)])?
                    .remove(0)
                    .rows,
                vec![vec![Value::Integer(7)]]
            );
            let p = c.prepare("SELECT typed_explicit($1),typed_assignment($1)")?;
            assert_eq!(
                c.execute_prepared(&p, &[Value::Varchar("parameter".into())])?
                    .rows,
                vec![vec![Value::Integer(11), Value::Integer(22)]]
            );
            for value in [Value::Varchar("1".into()), Value::Integer(1)] {
                assert!(matches!(
                    c.execute_params("SELECT typed_implicit($1)", &[value]),
                    Err(Error::Bind(_))
                ));
            }
            for (value, resource) in [
                ("resource", true),
                ("internal", false),
                ("invalid", false),
                ("wide", false),
                ("null", false),
            ] {
                let error = c
                    .query(&format!(
                        "SELECT TRY_CAST(typed_implicit('{value}') AS VARCHAR)"
                    ))
                    .unwrap_err();
                if resource {
                    assert!(matches!(error, Error::Resource(_)));
                } else {
                    assert!(matches!(error, Error::Internal(_)));
                }
            }
            assert!(matches!(
                c.query("SELECT typed_explicit('interrupted')"),
                Err(Error::Interrupted)
            ));
            for input in [
                "resource",
                "internal",
                "invalid",
                "wide",
                "null",
                "interrupted",
            ] {
                let error = c
                    .query(&format!("SELECT typed_optional('{input}'::TINYINT)"))
                    .unwrap_err();
                assert!(match input {
                    "resource" => matches!(error, Error::Resource(_)),
                    "interrupted" => matches!(error, Error::Interrupted),
                    _ => matches!(error, Error::Internal(_)),
                });
                let error = c
                    .query(&format!("SELECT typed_null('{input}'::TINYINT)"))
                    .unwrap_err();
                assert!(match input {
                    "resource" => matches!(error, Error::Resource(_)),
                    "interrupted" => matches!(error, Error::Interrupted),
                    _ => matches!(error, Error::Internal(_)),
                });
            }
            c.execute("CREATE TABLE t(k TINYINT PRIMARY KEY); INSERT INTO t VALUES (typed_explicit('ok'))")?;
            assert!(matches!(
                c.execute("UPDATE t SET k=typed_explicit('resource')"),
                Err(Error::Resource(_))
            ));
            assert_eq!(
                c.query("SELECT k FROM t")?.rows,
                vec![vec![Value::Integer(11)]]
            );
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
        panic!("typed-constant default must not evaluate or use a hidden cast")
    }
}

struct InvalidCastEvaluator(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for InvalidCastEvaluator {
    fn name(&self) -> &'static str {
        "typed-constant-invalid-evaluator"
    }
    fn evaluate(
        &self,
        expression: &BoundExpr,
        row: &Vec<Value>,
        context: &dyn EvaluationContext,
    ) -> Result<Value> {
        if matches!(&expression.kind, ExprKind::Cast(..))
            && expression.data_type == DataType::TinyInt
        {
            if self.0 {
                return Err(Error::Resource("typed constant evaluator resource".into()));
            }
            return Ok(Value::Varchar("invalid evaluator output".into()));
        }
        ScalarEvaluator.evaluate(expression, row, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_constant_frontends_reject_missing_capability_and_invalid_evaluator_results() -> Result<()>
{
    assert!(matches!(
        ExternalArguments.constant_if_closed(0),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        ExternalArguments.constant_if_closed(1),
        Err(Error::Bind(_))
    ));
    assert!(matches!(
        ExternalArguments.is_provably_null(0),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        ExternalArguments.is_provably_null(1),
        Err(Error::Bind(_))
    ));
    for mode in [CastMode::Implicit, CastMode::Explicit, CastMode::Assignment] {
        assert!(matches!(
            ExternalArguments.constant_as(0, &DataType::TinyInt, mode),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            ExternalArguments.constant_as(1, &DataType::TinyInt, mode),
            Err(Error::Bind(_))
        ));
    }
    for resource in [false, true] {
        let mut c = DatabaseBuilder::new()
            .functions(registry()?)
            .expressions(Arc::new(InvalidCastEvaluator(resource)))
            .build()?
            .connect();
        for sql in [
            "SELECT typed_explicit(1)",
            "SELECT typed_optional(1::TINYINT)",
            "SELECT typed_null(1::TINYINT)",
        ] {
            let error = c.query(sql).unwrap_err();
            if resource {
                assert!(matches!(error, Error::Resource(_)));
            } else {
                assert!(matches!(error, Error::Internal(_)));
            }
        }
    }
    Ok(())
}
