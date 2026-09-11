use super::*;
use duckdb_rust::{
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec},
        type_registry::TypeRegistry,
    },
    function::{ArgumentEvaluation, FunctionEffects, ScalarBindArguments, ScalarSignature},
};

#[derive(Debug, Clone)]
struct Probe {
    name: &'static str,
    candidates: Vec<ScalarSignature>,
    selected: Option<usize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Probe {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        ScalarSignature::validate_candidates(self.name, &self.candidates, query)?;
        let selected = arguments.select_overload(self.name, &self.candidates)?;
        ScalarSignature::selected(&self.candidates, selected)?;
        Ok(Some(Arc::new(Self {
            selected: Some(selected),
            ..self.clone()
        })))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::TypeOnly
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::BigInt)
    }
    fn evaluate(&self, args: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        assert!(args.is_empty());
        Ok(Value::Integer(
            self.selected
                .ok_or_else(|| Error::Internal("unbound overload probe".into()))?
                as i128,
        ))
    }
}

#[derive(Debug)]
struct Unread(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Unread {
    fn name(&self) -> &str {
        "overload_unread"
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
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(Error::Internal(
            "metadata request evaluated an argument".into(),
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn signature(kind: DataType) -> ScalarSignature {
    ScalarSignature {
        argument_names: None,
        arguments: vec![kind.clone()],
        return_type: kind,
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn advertised_argument_labels_are_owned_bounded_and_not_named_call_resolution() -> Result<()> {
    let query = QueryContext::background();
    let labeled = ScalarSignature {
        argument_names: Some(vec!["data".into()]),
        ..signature(DataType::Date)
    };
    let functions = registry("labeled_overload", vec![labeled.clone()])?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    let Err(Error::Bind(body)) = c.query("SELECT labeled_overload(true)") else {
        panic!("expected candidate error");
    };
    assert_eq!(
        body,
        "No function matches the given name and argument types 'labeled_overload(BOOLEAN)'. You might need to add explicit type casts.\n\tCandidate functions:\n\tlabeled_overload(data DATE) -> DATE\n"
    );
    for names in [
        vec![],
        vec!["".into()],
        vec!["bad\0name".into()],
        vec!["a".into(), "b".into()],
    ] {
        let invalid = ScalarSignature {
            argument_names: Some(names),
            ..labeled.clone()
        };
        assert!(matches!(
            ScalarSignature::validate_candidates("labels", &[invalid], &query),
            Err(Error::Internal(_))
        ));
    }
    let large = ScalarSignature {
        argument_names: Some(vec!["x".repeat(4097)]),
        ..labeled.clone()
    };
    assert!(matches!(
        ScalarSignature::validate_candidates("labels", &[large], &query),
        Err(Error::Resource(_))
    ));
    let bounded = ScalarSignature {
        argument_names: Some(vec!["x".repeat(4096)]),
        ..labeled
    };
    ScalarSignature::validate_candidates("labels", &vec![bounded.clone(); 16], &query)?;
    assert!(matches!(
        ScalarSignature::validate_candidates("labels", &vec![bounded; 17], &query),
        Err(Error::Resource(_))
    ));
    // An arity-inapplicable candidate is still completely validated.
    let invalid = ScalarSignature {
        arguments: vec![DataType::Date; 2],
        return_type: DataType::Date,
        argument_names: Some(vec!["one".into()]),
    };
    let functions = registry(
        "invalid_labels",
        vec![signature(DataType::Integer), invalid],
    )?;
    assert!(matches!(
        DatabaseBuilder::new()
            .functions(functions)
            .build()?
            .connect()
            .query("SELECT invalid_labels(1)"),
        Err(Error::Internal(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn registry(name: &'static str, candidates: Vec<ScalarSignature>) -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(Probe {
        name,
        candidates,
        selected: None,
    }))?;
    Ok(functions)
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn selected_overloads_retain_literal_null_identity_order_and_unevaluated_children() -> Result<()> {
    let functions = registry(
        "overload_probe",
        vec![
            signature(DataType::Date),
            signature(DataType::Interval),
            signature(DataType::Timestamp),
        ],
    )?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT overload_probe(DATE 'epoch')")?.rows,
        vec![vec![Value::Integer(0)]]
    );
    let Error::Bind(message) = c.query("SELECT overload_probe(NULL)").unwrap_err() else {
        panic!("expected overload ambiguity")
    };
    assert_eq!(
        message,
        "Could not choose a best candidate function for the function call \"overload_probe(\"NULL\")\". In order to select one, please add explicit type casts.\n\tCandidate functions:\n\toverload_probe(col0 INTERVAL) -> INTERVAL\n\toverload_probe(col0 DATE) -> DATE\n"
    );
    let Error::Bind(message) = c.query("SELECT overload_probe('epoch')").unwrap_err() else {
        panic!("expected string pseudo-type ambiguity")
    };
    assert!(message.contains("overload_probe(STRING_LITERAL)"));
    assert!(message.ends_with("\toverload_probe(col0 INTERVAL) -> INTERVAL\n\toverload_probe(col0 TIMESTAMP) -> TIMESTAMP\n\toverload_probe(col0 DATE) -> DATE\n"));
    let Error::Bind(message) = c
        .query("SELECT overload_probe('epoch'::VARCHAR)")
        .unwrap_err()
    else {
        panic!("VARCHAR is not a literal pseudo-type")
    };
    assert!(message.starts_with(
        "No function matches the given name and argument types 'overload_probe(VARCHAR)'"
    ));
    assert!(message.ends_with("\toverload_probe(col0 DATE) -> DATE\n\toverload_probe(col0 INTERVAL) -> INTERVAL\n\toverload_probe(col0 TIMESTAMP) -> TIMESTAMP\n"));
    let prepared = c.prepare("SELECT overload_probe($1)")?;
    assert_eq!(
        c.execute_prepared(&prepared, &[Value::Date("2001-02-03".parse()?)])?
            .rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(
        matches!(c.execute_prepared(&prepared, &[Value::Varchar("epoch".into())]), Err(Error::Bind(message)) if message.contains("overload_probe(VARCHAR)"))
    );

    let counter = Arc::new(AtomicUsize::new(0));
    let mut functions = registry("selected_alias", vec![signature(DataType::Integer)])?;
    functions.register_scalar(Arc::new(Unread(counter.clone())))?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    for sql in [
        "SELECT selected_alias(CAST('bad' AS INTEGER))",
        "SELECT selected_alias(overload_unread())",
    ] {
        assert_eq!(c.query(sql)?.rows, vec![vec![Value::Integer(0)]]);
    }
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert!(
        matches!(c.query("SELECT selected_alias(TRUE)"), Err(Error::Bind(message)) if message.contains("selected_alias(BOOLEAN)"))
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn overload_integer_hints_preserve_unsigned_domains_without_inventing_literals() -> Result<()> {
    let functions = registry(
        "integer_overload",
        vec![
            signature(DataType::HugeInt),
            signature(DataType::UHugeInt),
            signature(DataType::BigInt),
        ],
    )?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert_eq!(c.query("SELECT integer_overload(340282366920938463463374607431768211455),integer_overload(170141183460469231731687303715884105728),integer_overload(-170141183460469231731687303715884105728),integer_overload(1)")?.rows, vec![vec![Value::Integer(1),Value::Integer(1),Value::Integer(0),Value::Integer(2)]]);
    let functions = registry("narrow_overload", vec![signature(DataType::UTinyInt)])?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT narrow_overload(255)")?.rows,
        vec![vec![Value::Integer(0)]]
    );
    for sql in [
        "SELECT narrow_overload(256)",
        "SELECT narrow_overload(-1)",
        "SELECT narrow_overload(255::INTEGER)",
        "SELECT narrow_overload(250+5)",
    ] {
        assert!(matches!(c.query(sql), Err(Error::Bind(_))), "{sql}");
    }
    let prepared = c.prepare("SELECT narrow_overload($1)")?;
    assert!(matches!(
        c.execute_prepared(&prepared, &[Value::Integer(255)]),
        Err(Error::Bind(_))
    ));
    Ok(())
}

#[derive(Debug)]
struct Cost {
    fail: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for Cost {
    fn name(&self) -> &'static str {
        "selected-overload-cost"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::SmallInt
            && spec.target == DataType::BigInt
            && spec.mode == CastMode::Implicit
    }
    fn coercion_cost_with_registry(
        &self,
        _: &CastSpec,
        _: &CastRegistry,
        _: &TypeRegistry,
    ) -> Result<Option<u32>> {
        if self.fail {
            Err(Error::Resource("selected score failed".into()))
        } else {
            Ok(Some(1000))
        }
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        panic!("overload selection executed a cast")
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn overload_ranking_retains_selected_cost_errors_and_validates_every_candidate() -> Result<()> {
    for fail in [false, true] {
        let mut casts = CastRegistry::builtins();
        casts.replace(
            CastSpec {
                source: DataType::SmallInt,
                target: DataType::BigInt,
                mode: CastMode::Implicit,
            },
            Arc::new(Cost { fail }),
        )?;
        let functions = registry(
            "cost_overload",
            vec![signature(DataType::BigInt), signature(DataType::Integer)],
        )?;
        let mut c = DatabaseBuilder::new()
            .casts(casts)
            .functions(functions)
            .build()?
            .connect();
        let result = c.query("SELECT cost_overload(1::SMALLINT)");
        if fail {
            assert!(
                matches!(result, Err(Error::Resource(message)) if message == "selected score failed")
            );
        } else {
            assert_eq!(result?.rows, vec![vec![Value::Integer(1)]]);
        }
    }
    let query = QueryContext::background();
    assert!(matches!(
        ScalarSignature::validate_candidates("", &[signature(DataType::Integer)], &query),
        Err(Error::Internal(_))
    ));
    assert!(matches!(
        ScalarSignature::validate_candidates("empty", &[], &query),
        Err(Error::Internal(_))
    ));
    assert!(matches!(
        ScalarSignature::selected(&[signature(DataType::Integer)], 1),
        Err(Error::Internal(_))
    ));
    let functions = registry(
        "malformed_overload",
        vec![
            signature(DataType::Integer),
            ScalarSignature {
                argument_names: None,
                arguments: vec![DataType::Integer; 2],
                return_type: DataType::Decimal { width: 0, scale: 0 },
            },
        ],
    )?;
    assert!(
        DatabaseBuilder::new()
            .functions(functions)
            .build()?
            .connect()
            .query("SELECT malformed_overload(1)")
            .is_err()
    );
    let interrupt = duckdb_rust::parallel::InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2, 100)?;
    interrupt.interrupt();
    assert!(matches!(
        ScalarSignature::validate_candidates("cancelled", &[signature(DataType::Integer)], &query),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unimplemented_frontend_does_not_inherit_an_ambient_overload_resolver() {
    struct Frontend;
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl ScalarBindArguments for Frontend {
        fn len(&self) -> usize {
            0
        }
        fn data_type(&self, _: usize) -> Result<DataType> {
            Err(Error::Bind("no argument".into()))
        }
        fn constant(&self, _: usize) -> Result<Value> {
            panic!("metadata capability evaluated a constant")
        }
    }
    assert!(matches!(
        Frontend.select_overload("selected", &[signature(DataType::Integer)]),
        Err(Error::Unsupported(_))
    ));
}

#[derive(Debug)]
struct LiteralCost(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for LiteralCost {
    fn name(&self) -> &'static str {
        "literal-selected-cost"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target == DataType::Date
            && spec.mode == CastMode::Explicit
    }
    fn coercion_cost_with_registry(
        &self,
        _: &CastSpec,
        _: &CastRegistry,
        _: &TypeRegistry,
    ) -> Result<Option<u32>> {
        if self.0 {
            Err(Error::Resource("literal cast metadata failed".into()))
        } else {
            Ok(None)
        }
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        panic!("metadata-only selection cast a literal")
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn literal_priority_cannot_grant_declined_casts_or_hide_selected_metadata_failures() -> Result<()> {
    for fail in [false, true] {
        let mut casts = CastRegistry::builtins();
        casts.replace(
            CastSpec {
                source: DataType::Varchar,
                target: DataType::Date,
                mode: CastMode::Explicit,
            },
            Arc::new(LiteralCost(fail)),
        )?;
        let functions = registry("literal_overload", vec![signature(DataType::Date)])?;
        let result = DatabaseBuilder::new()
            .casts(casts)
            .functions(functions)
            .build()?
            .connect()
            .query("SELECT literal_overload('epoch')");
        if fail {
            assert!(
                matches!(result, Err(Error::Resource(message)) if message == "literal cast metadata failed")
            );
        } else {
            assert!(
                matches!(result, Err(Error::Bind(message)) if message.starts_with("No function matches"))
            );
        }
    }
    Ok(())
}
