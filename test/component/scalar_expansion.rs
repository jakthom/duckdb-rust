use super::*;
use duckdb_rust::{
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec},
        type_registry::TypeRegistry,
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionEffects, ScalarBindArguments, ScalarExpansion, ScalarExpansionNode as N},
    optimizer::{Optimizer, PipelineOptimizer},
};

#[derive(Debug)]
struct Template {
    nodes: Vec<N>,
    effectful: bool,
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
        panic!("expansion must not request a constant")
    }
}

#[derive(Debug)]
struct OrdinaryReplacement;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for OrdinaryReplacement {
    fn name(&self) -> &str {
        "selected_choose"
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Integer)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Ok(Value::Integer(42))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn expansion_capability_preserves_ordinary_replacements_and_explicit_frontend_rejection()
-> Result<()> {
    let query = QueryContext::background();
    let args = ExternalArguments;
    let template = Template {
        nodes: vec![N::Argument(0)],
        effectful: false,
    };
    assert!(template.expansion(&args, &query)?.is_some());
    assert!(matches!(
        template.bind(&args, &query),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        template.evaluate(&[Value::Integer(1)], &query),
        Err(Error::Unsupported(_))
    ));
    assert!(OrdinaryReplacement.expansion(&args, &query)?.is_none());
    let mut functions = FunctionRegistry::default();
    functions.register_scalar(Arc::new(OrdinaryReplacement))?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT selected_choose(1)")?.rows,
        vec![vec![Value::Integer(42)]]
    );
    let interrupt = duckdb_rust::parallel::InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2, 16)?;
    interrupt.interrupt();
    assert!(matches!(
        ScalarExpansion {
            nodes: vec![N::Null]
        }
        .validate(0, &query),
        Err(Error::Interrupted)
    ));
    Ok(())
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Template {
    fn name(&self) -> &str {
        "selected_choose"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: self.effectful,
            external_access: false,
        }
    }
    fn expansion(
        &self,
        _: &dyn ScalarBindArguments,
        q: &QueryContext,
    ) -> Result<Option<ScalarExpansion>> {
        q.check()?;
        Ok(Some(ScalarExpansion {
            nodes: self.nodes.clone(),
        }))
    }
    fn bind(
        &self,
        _: &dyn ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        Err(Error::Unsupported(
            "selected_choose requires expansion".into(),
        ))
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Err(Error::Unsupported(
            "selected_choose requires expansion".into(),
        ))
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Err(Error::Unsupported(
            "selected_choose requires expansion".into(),
        ))
    }
}

#[derive(Debug)]
struct Occurrence(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Occurrence {
    fn name(&self) -> &str {
        "expansion_occurrence"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: true,
        }
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::BigInt)
    }
    fn evaluate(&self, _: &[Value], q: &QueryContext) -> Result<Value> {
        q.check()?;
        Ok(Value::Integer(
            (self.0.fetch_add(1, Ordering::SeqCst) + 1) as i128,
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn functions(nodes: Vec<N>, calls: Arc<AtomicUsize>) -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(Template {
        nodes,
        effectful: false,
    }))?;
    functions.register_scalar(Arc::new(Occurrence(calls)))?;
    Ok(functions)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_expansion_keeps_case_metadata_casts_lazy_branches_and_argument_occurrences()
-> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let nodes = vec![
                N::Argument(0),
                N::Argument(1),
                N::Equal { left: 0, right: 1 },
                N::Argument(2),
                N::Argument(3),
                N::Case {
                    condition: 2,
                    then_value: 3,
                    otherwise: 4,
                },
            ];
            let mut c = DatabaseBuilder::new()
                .functions(functions(nodes, calls)?)
                .expressions(evaluator.clone())
                .optimizer(optimizer.clone())
                .build()?
                .connect();
            assert_eq!(c.query("SELECT selected_choose(1,1,1::UHUGEINT,'bad'::INTEGER),typeof(selected_choose(1,1,1::UHUGEINT,2::INTEGER)),selected_choose(1,2,'bad'::INTEGER,7::UHUGEINT)")?.rows,vec![vec![Value::Integer(1),Value::Varchar("BIGINT".into()),Value::Integer(7)]]);
            assert!(matches!(
                c.query("SELECT selected_choose(1,1,340282366920938463463374607431768211455,1)"),
                Err(Error::Conversion(_))
            ));
            let p =
                c.prepare("SELECT selected_choose(1,1,$1,1),typeof(selected_choose(1,1,$1,1))")?;
            assert_eq!(
                c.execute_prepared(&p, &[Value::Unsigned(u128::MAX)])?.rows,
                vec![vec![
                    Value::Unsigned(u128::MAX),
                    Value::Varchar("UHUGEINT".into())
                ]]
            );
            let calls = Arc::new(AtomicUsize::new(0));
            let nodes = vec![
                N::Argument(0),
                N::Argument(1),
                N::Equal { left: 0, right: 1 },
                N::Null,
                N::Case {
                    condition: 2,
                    then_value: 3,
                    otherwise: 0,
                },
            ];
            let mut c = DatabaseBuilder::new()
                .functions(functions(nodes, calls.clone())?)
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            assert_eq!(
                c.query("SELECT selected_choose(expansion_occurrence(),99)")?
                    .rows,
                vec![vec![Value::Integer(2)]]
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(
                c.query("SELECT selected_choose(expansion_occurrence(),3)")?
                    .rows,
                vec![vec![Value::Null]]
            );
            assert_eq!(calls.load(Ordering::SeqCst), 3);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct RetainedCast(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for RetainedCast {
    fn name(&self) -> &'static str {
        "expansion-comparison-cast"
    }
    fn supports(&self, s: &CastSpec) -> bool {
        s.source == DataType::UHugeInt && s.target == DataType::BigInt
    }
    fn cast(&self, _: &Value, _: &CastSpec, q: &QueryContext) -> Result<Value> {
        q.check()?;
        if self.0 {
            Err(Error::Resource("expansion comparison resource".into()))
        } else {
            Ok(Value::Integer(11))
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn expansion_comparisons_retain_selected_casts_and_fatal_errors() -> Result<()> {
    for fatal in [false, true] {
        let mut casts = CastRegistry::builtins();
        casts.replace(
            CastSpec {
                source: DataType::UHugeInt,
                target: DataType::BigInt,
                mode: CastMode::Explicit,
            },
            Arc::new(RetainedCast(fatal)),
        )?;
        let nodes = vec![
            N::Argument(0),
            N::Argument(1),
            N::Equal { left: 0, right: 1 },
            N::Null,
            N::Case {
                condition: 2,
                then_value: 3,
                otherwise: 0,
            },
        ];
        let mut c = DatabaseBuilder::new()
            .casts(casts)
            .functions(functions(nodes, Arc::new(AtomicUsize::new(0)))?)
            .build()?
            .connect();
        let result =
            c.query("SELECT TRY_CAST(selected_choose(1::UHUGEINT,11::INTEGER) AS UHUGEINT)");
        if fatal {
            assert!(matches!(result, Err(Error::Resource(_))));
        } else {
            assert_eq!(result?.rows, vec![vec![Value::Null]]);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn complete_expansion_validation_rejects_bad_graphs_effects_and_unbounded_duplication() -> Result<()>
{
    let query = QueryContext::background();
    let mut too_deep = vec![N::Null];
    for index in 1..65 {
        too_deep.push(N::Case {
            condition: index - 1,
            then_value: 0,
            otherwise: 0,
        });
    }
    let mut duplicate = vec![N::Argument(0)];
    for index in 1..13 {
        duplicate.push(N::Equal {
            left: index - 1,
            right: index - 1,
        });
    }
    for nodes in [
        vec![],
        vec![N::Argument(2)],
        vec![N::Equal { left: 0, right: 0 }],
        vec![N::Null, N::Equal { left: 0, right: 2 }],
        vec![N::Null; 1025],
        too_deep,
        duplicate,
    ] {
        assert!(
            ScalarExpansion {
                nodes: nodes.clone()
            }
            .validate(2, &query)
            .is_err()
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let mut c = DatabaseBuilder::new()
            .functions(functions(nodes, calls.clone())?)
            .build()?
            .connect();
        assert!(
            c.query("SELECT selected_choose(expansion_occurrence(),1)")
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(Template {
        nodes: vec![N::Argument(0)],
        effectful: true,
    }))?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert!(matches!(
        c.query("SELECT selected_choose(1)"),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}
