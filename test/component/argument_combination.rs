use super::*;
use duckdb_rust::{
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec},
        type_registry::TypeRegistry,
    },
    function::{ArgumentCombination, ArgumentEvaluation, FunctionEffects, ScalarBindArguments},
    parallel::InterruptHandle,
};

#[derive(Clone)]
struct ProposalProbe {
    indices: Vec<usize>,
    result: Option<ArgumentCombination>,
    interrupt: Option<Arc<std::sync::Mutex<Option<InterruptHandle>>>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for ProposalProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProposalProbe")
            .field("indices", &self.indices)
            .field("result", &self.result)
            .finish_non_exhaustive()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ProposalProbe {
    fn name(&self) -> &str {
        "combination_probe"
    }
    fn bind(
        &self,
        args: &dyn ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        if let Some(interrupt) = &self.interrupt {
            interrupt.lock().unwrap().as_ref().unwrap().interrupt();
        }
        Ok(Some(Arc::new(Self {
            result: Some(args.combination(&self.indices)?),
            ..self.clone()
        })))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::TypeOnly
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, args: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        assert!(args.is_empty());
        let result = self
            .result
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound combination probe".into()))?;
        Ok(Value::Varchar(format!(
            "{}|{:?}",
            result.data_type, result.cast_modes
        )))
    }
}

#[derive(Debug)]
struct UnreadChild;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for UnreadChild {
    fn name(&self) -> &str {
        "combination_effect"
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
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        panic!("combination metadata evaluated an effectful child")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn probe(indices: Vec<usize>) -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(ProposalProbe {
        indices,
        result: None,
        interrupt: None,
    }))?;
    functions.register_scalar(Arc::new(UnreadChild))?;
    Ok(functions)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn combination_metadata_preserves_source_order_full_hints_and_unevaluated_children() -> Result<()> {
    let mut c = DatabaseBuilder::new()
        .functions(probe(vec![0, 1, 2])?)
        .optimizer(Arc::new(IdentityOptimizer))
        .build()?
        .connect();
    for (sql, expected) in [
        (
            "1,NULL,1::UHUGEINT",
            "BIGINT|[Implicit, Implicit, Explicit]",
        ),
        (
            "1,1::UHUGEINT,NULL",
            "UHUGEINT|[Explicit, Implicit, Implicit]",
        ),
        (
            "340282366920938463463374607431768211455,NULL,1",
            "UHUGEINT|[Implicit, Implicit, Explicit]",
        ),
        (
            "'bad'::UHUGEINT,combination_effect(),i::INTEGER",
            "BIGINT|[Explicit, Implicit, Implicit]",
        ),
    ] {
        assert_eq!(
            c.query(&format!(
                "SELECT combination_probe({sql}) FROM range(1)t(i)"
            ))?
            .rows,
            vec![vec![Value::Varchar(expected.into())]],
            "{sql}"
        );
    }
    let mut c = DatabaseBuilder::new()
        .functions(probe(vec![0, 1])?)
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT combination_probe(340282366920938463463374607431768211455,1)")?
            .rows,
        vec![vec![Value::Varchar("BIGINT|[Explicit, Implicit]".into())]]
    );
    let p = c.prepare("SELECT combination_probe($1,1)")?;
    assert_eq!(
        c.execute_prepared(&p, &[Value::Unsigned(u128::MAX)])?.rows,
        vec![vec![Value::Varchar("UHUGEINT|[Implicit, Explicit]".into())]]
    );
    Ok(())
}

struct ExternalArguments;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for ExternalArguments {
    fn len(&self) -> usize {
        2
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        if index < 2 {
            Ok(DataType::Integer)
        } else {
            Err(Error::Bind("outside signature".into()))
        }
    }
    fn constant(&self, _: usize) -> Result<Value> {
        panic!("default combination must not evaluate")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn combination_metadata_rejects_missing_capabilities_indices_invalid_proposals_and_cancellation()
-> Result<()> {
    assert!(matches!(
        ExternalArguments.combination(&[0, 1]),
        Err(Error::Unsupported(_))
    ));
    for indices in [vec![], vec![2], vec![1, 0], vec![0, 0]] {
        assert!(matches!(
            ExternalArguments.combination(&indices),
            Err(Error::Bind(_))
        ));
        let mut c = DatabaseBuilder::new()
            .functions(probe(indices)?)
            .build()?
            .connect();
        assert!(matches!(
            c.query("SELECT combination_probe(1,2)"),
            Err(Error::Bind(_))
        ));
    }
    for proposal in [
        ArgumentCombination {
            data_type: DataType::Integer,
            cast_modes: vec![],
        },
        ArgumentCombination {
            data_type: DataType::Integer,
            cast_modes: vec![CastMode::Implicit; 2],
        },
        ArgumentCombination {
            data_type: DataType::Integer,
            cast_modes: vec![CastMode::Assignment],
        },
        ArgumentCombination {
            data_type: DataType::Decimal { width: 0, scale: 0 },
            cast_modes: vec![CastMode::Implicit],
        },
    ] {
        assert!(proposal.validate(1, &TypeRegistry::builtins()).is_err());
    }
    let slot = Arc::new(std::sync::Mutex::new(None));
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(ProposalProbe {
        indices: vec![0],
        result: None,
        interrupt: Some(slot.clone()),
    }))?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    *slot.lock().unwrap() = Some(c.interrupt_handle());
    assert!(matches!(
        c.query("SELECT combination_probe(1)"),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct ModeFunction(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ModeFunction {
    fn name(&self) -> &str {
        if self.0 {
            "literal_privilege"
        } else {
            "literal_selected"
        }
    }
    fn argument_literal_coercion(&self, _: usize) -> bool {
        self.0
    }
    fn argument_types(&self, args: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        Ok(vec![DataType::TinyInt; args.len()])
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::TinyInt)
    }
    fn evaluate(&self, args: &[Value], _: &QueryContext) -> Result<Value> {
        Ok(args[0].clone())
    }
}
#[derive(Debug)]
struct ModeCast(Arc<std::sync::Mutex<Vec<CastMode>>>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for ModeCast {
    fn name(&self) -> &'static str {
        "combination-mode-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer && spec.target == DataType::TinyInt
    }
    fn cast(&self, _: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.0.lock().unwrap().push(spec.mode);
        Ok(Value::Integer(if spec.mode == CastMode::Implicit {
            11
        } else {
            22
        }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_argument_modes_can_disable_only_the_additional_literal_rewrite() -> Result<()> {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut casts = CastRegistry::builtins();
    for mode in [CastMode::Implicit, CastMode::Explicit] {
        let spec = CastSpec {
            source: DataType::Integer,
            target: DataType::TinyInt,
            mode,
        };
        let adapter = Arc::new(ModeCast(calls.clone()));
        if mode == CastMode::Implicit {
            casts.register(spec, adapter)?;
        } else {
            casts.replace(spec, adapter)?;
        }
    }
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(ModeFunction(true)))?;
    functions.register_scalar(Arc::new(ModeFunction(false)))?;
    let mut c = DatabaseBuilder::new()
        .casts(casts)
        .functions(functions)
        .optimizer(Arc::new(IdentityOptimizer))
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT literal_privilege(1),literal_selected(1)")?
            .rows,
        vec![vec![Value::Integer(22), Value::Integer(11)]]
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![CastMode::Explicit, CastMode::Implicit]
    );
    Ok(())
}

struct ForeignCombination(ArgumentCombination);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for ForeignCombination {
    fn len(&self) -> usize {
        2
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        ExternalArguments.data_type(index)
    }
    fn constant(&self, _: usize) -> Result<Value> {
        panic!("combination must not evaluate")
    }
    fn combination(&self, indices: &[usize]) -> Result<ArgumentCombination> {
        assert_eq!(indices, &[0, 1]);
        Ok(self.0.clone())
    }
}

#[derive(Debug)]
struct ReplacedCoalesce;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ReplacedCoalesce {
    fn name(&self) -> &str {
        "coalesce"
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::TypeOnly
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Ok(Value::Varchar("selected replacement".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn coalesce_requires_valid_selected_frontend_proposals_and_keeps_catalog_replacements() -> Result<()>
{
    let registry = FunctionRegistry::builtins();
    let selected = registry.scalar("coalesce")?;
    let query = QueryContext::background();
    assert!(matches!(
        selected.bind(&ExternalArguments, &query),
        Err(Error::Unsupported(_))
    ));
    for proposal in [
        ArgumentCombination {
            data_type: DataType::Integer,
            cast_modes: vec![],
        },
        ArgumentCombination {
            data_type: DataType::Decimal { width: 0, scale: 0 },
            cast_modes: vec![CastMode::Implicit; 2],
        },
        ArgumentCombination {
            data_type: DataType::Integer,
            cast_modes: vec![CastMode::Assignment; 2],
        },
    ] {
        assert!(
            selected
                .bind(&ForeignCombination(proposal), &query)
                .is_err()
        );
    }
    let bound = selected
        .bind(
            &ForeignCombination(ArgumentCombination {
                data_type: DataType::BigInt,
                cast_modes: vec![CastMode::Implicit; 2],
            }),
            &query,
        )?
        .unwrap();
    assert!(
        bound
            .argument_types(&[DataType::Integer], query.types())
            .is_err()
    );
    assert!(
        bound
            .return_type(&[DataType::Integer, DataType::Integer], query.types())
            .is_err()
    );
    assert!(bound.evaluate(&[Value::Null], &query).is_err());
    let mut functions = FunctionRegistry::default();
    functions.register_scalar(Arc::new(ReplacedCoalesce))?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT coalesce('bad'::INTEGER,1::UHUGEINT)")?.rows,
        vec![vec![Value::Varchar("selected replacement".into())]]
    );
    Ok(())
}
