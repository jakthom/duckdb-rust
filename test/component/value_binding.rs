//! Contextual literal metadata must not leak from typed constants or early
//! branch pruning. The selected scalar adapter owns its overload decision.
use super::*;
use duckdb_rust::{
    common::cast::{CastFunction, CastMode, CastRegistry, CastSpec, PrimitiveCast},
    common::type_registry::TypeRegistry,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::ScalarBindArguments,
    optimizer::{Optimizer, PipelineOptimizer},
};

#[derive(Debug)]
struct CoercionProbe(CastMode);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for CoercionProbe {
    fn name(&self) -> &str {
        if self.0 == CastMode::Assignment {
            "assignment_probe"
        } else {
            "coercion_probe"
        }
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if arguments.len() != 2 {
            return Err(Error::Bind("coercion probe arity".into()));
        }
        Ok(vec![DataType::Integer, DataType::BigInt])
    }
    fn argument_cast_mode(&self, index: usize) -> CastMode {
        if index == 0 {
            self.0
        } else {
            CastMode::Implicit
        }
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::BigInt)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        Ok(Value::Integer(
            arguments[0].as_i128()? + arguments[1].as_i128()?,
        ))
    }
}

#[derive(Debug)]
struct SelectedArgumentCast(Arc<AtomicUsize>, CastMode);

#[derive(Debug)]
struct CombinationCast(Arc<AtomicUsize>, CastMode);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for CombinationCast {
    fn name(&self) -> &'static str {
        "selected-combination-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Boolean && spec.target == DataType::Integer && spec.mode == self.1
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::Relaxed);
        assert_eq!(spec.mode, self.1);
        // Delegation is explicit here even when the selected replacement elects
        // to offer an implicit conversion. Built-ins still do not offer it.
        PrimitiveCast.cast(
            value,
            &CastSpec {
                mode: CastMode::Explicit,
                ..spec.clone()
            },
            query,
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn combination_contexts_retain_selected_casts_without_widening_function_overloads() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for selected_mode in [CastMode::Explicit, CastMode::Implicit] {
                let calls = Arc::new(AtomicUsize::new(0));
                let mut casts = CastRegistry::builtins();
                assert!(
                    casts
                        .coercion_cost_with_types(
                            &DataType::Boolean,
                            &DataType::Integer,
                            CastMode::Implicit,
                            &TypeRegistry::builtins()
                        )?
                        .is_none()
                );
                let spec = CastSpec {
                    source: DataType::Boolean,
                    target: DataType::Integer,
                    mode: selected_mode,
                };
                let adapter = Arc::new(CombinationCast(calls.clone(), selected_mode));
                if selected_mode == CastMode::Implicit {
                    casts.register(spec, adapter)?;
                } else {
                    casts.replace(spec, adapter)?;
                }
                let mut c = DatabaseBuilder::new()
                    .casts(casts)
                    .expressions(evaluator.clone())
                    .optimizer(optimizer.clone())
                    .batch_size(2)
                    .build()?
                    .connect();
                c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, b BOOLEAN); INSERT INTO t VALUES (1,true),(2,false),(3,NULL)")?;
                for (sql, expected) in [
                    (
                        "SELECT CASE WHEN id<3 THEN b ELSE 7 END FROM t ORDER BY id",
                        vec![1, 0, 7],
                    ),
                    (
                        "SELECT CASE WHEN id=3 THEN 7 ELSE b END FROM t ORDER BY id",
                        vec![1, 0, 7],
                    ),
                    (
                        "SELECT x FROM (VALUES (true),(false),(2)) v(x) ORDER BY x",
                        vec![0, 1, 2],
                    ),
                    (
                        "SELECT b AS x FROM t WHERE id<3 UNION SELECT 2 ORDER BY x",
                        vec![0, 1, 2],
                    ),
                    (
                        "SELECT 2 AS x UNION ALL SELECT b FROM t WHERE id<3 ORDER BY x",
                        vec![0, 1, 2],
                    ),
                    (
                        "SELECT b AS x FROM t WHERE id<3 INTERSECT SELECT 1 ORDER BY x",
                        vec![1],
                    ),
                    (
                        "SELECT b AS x FROM t WHERE id<3 EXCEPT SELECT 1 ORDER BY x",
                        vec![0],
                    ),
                ] {
                    let before = calls.load(Ordering::Relaxed);
                    let result = c.query(sql)?;
                    assert_eq!(result.columns[0].data_type, DataType::Integer, "{sql}");
                    assert_eq!(
                        result.rows,
                        expected
                            .into_iter()
                            .map(|n| vec![Value::Integer(n)])
                            .collect::<Vec<_>>(),
                        "{sql}"
                    );
                    assert!(
                        calls.load(Ordering::Relaxed) > before,
                        "selected cast must execute: {sql}"
                    );
                }
                let prepared =
                    c.prepare("SELECT CASE WHEN id<3 THEN b ELSE ? END FROM t ORDER BY id")?;
                assert_eq!(
                    c.execute_prepared(&prepared, &[Value::Integer(7)])?.rows,
                    vec![
                        vec![Value::Integer(1)],
                        vec![Value::Integer(0)],
                        vec![Value::Integer(7)]
                    ]
                );
                c.execute("CREATE TABLE combined AS SELECT CASE WHEN id<3 THEN b ELSE 7 END AS x FROM t; BEGIN; UPDATE combined SET x=9 WHERE x=1; ROLLBACK")?;
                assert_eq!(
                    c.query("SELECT x,sum(x) OVER (ORDER BY x) FROM combined ORDER BY x")?
                        .rows,
                    vec![integers(&[0, 0]), integers(&[1, 1]), integers(&[7, 8])]
                );
                for sql in [
                    "SELECT CASE WHEN true THEN true ELSE 1.0::DOUBLE END",
                    "SELECT CASE WHEN true THEN 1.0::DECIMAL(2,1) ELSE false END",
                ] {
                    assert!(matches!(c.query(sql), Err(Error::Bind(_))), "{sql}");
                }
                if selected_mode == CastMode::Explicit {
                    assert!(matches!(
                        c.query("SELECT abs(b) FROM t"),
                        Err(Error::Bind(_))
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedArgumentCast {
    fn name(&self) -> &'static str {
        "selected-scalar-argument-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar && spec.target == DataType::Integer && spec.mode == self.1
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::Relaxed);
        assert_eq!(spec.mode, self.1);
        if value == &Value::Varchar("fatal".into()) {
            return Err(Error::Resource("argument cast witness".into()));
        }
        PrimitiveCast.cast(value, spec, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_scalar_coercion_policy_keeps_cast_registry_null_errors_and_parameter_identity()
-> Result<()> {
    assert_eq!(
        LiteralProbe::default().argument_cast_mode(0),
        CastMode::Implicit
    );
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let assignments = Arc::new(AtomicUsize::new(0));
            let mut casts = CastRegistry::builtins();
            casts.replace(
                CastSpec {
                    source: DataType::Varchar,
                    target: DataType::Integer,
                    mode: CastMode::Explicit,
                },
                Arc::new(SelectedArgumentCast(calls.clone(), CastMode::Explicit)),
            )?;
            casts.replace(
                CastSpec {
                    source: DataType::Varchar,
                    target: DataType::Integer,
                    mode: CastMode::Assignment,
                },
                Arc::new(SelectedArgumentCast(
                    assignments.clone(),
                    CastMode::Assignment,
                )),
            )?;
            let mut functions = FunctionRegistry::builtins();
            functions.register_scalar(Arc::new(CoercionProbe(CastMode::Explicit)))?;
            functions.register_scalar(Arc::new(CoercionProbe(CastMode::Assignment)))?;
            let mut c = DatabaseBuilder::new()
                .casts(casts)
                .functions(functions)
                .expressions(evaluator.clone())
                .optimizer(optimizer.clone())
                .batch_size(2)
                .build()?
                .connect();
            c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,s VARCHAR); INSERT INTO t VALUES (1,'2'),(2,NULL),(3,'4')")?;
            assert_eq!(
                c.query("SELECT coercion_probe(s,id) FROM t ORDER BY id")?
                    .rows,
                vec![
                    vec![Value::Integer(3)],
                    vec![Value::Null],
                    vec![Value::Integer(7)]
                ]
            );
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            let before = assignments.load(Ordering::Relaxed);
            assert_eq!(
                c.query("SELECT assignment_probe('2',3)")?.rows,
                vec![vec![Value::Integer(5)]]
            );
            assert!(
                assignments.load(Ordering::Relaxed) > before,
                "literal privilege must not replace an explicitly selected assignment cast"
            );
            // A declared explicit conversion is local to that argument. It
            // neither grants implicit VARCHAR conversion nor changes typed
            // parameters into SQL literals for the remaining arguments.
            assert!(matches!(
                c.query("SELECT coercion_probe(id,s) FROM t"),
                Err(Error::Bind(_))
            ));
            assert_eq!(
                c.query("SELECT coercion_probe('2','3')")?.rows,
                vec![vec![Value::Integer(5)]]
            );
            let prepared = c.prepare("SELECT coercion_probe(?,?)")?;
            assert_eq!(
                c.execute_prepared(&prepared, &[Value::Varchar("2".into()), Value::Integer(3)])?
                    .rows,
                vec![vec![Value::Integer(5)]]
            );
            assert!(matches!(
                c.execute_prepared(
                    &prepared,
                    &[Value::Varchar("2".into()), Value::Varchar("3".into())]
                ),
                Err(Error::Bind(_))
            ));
            c.execute("BEGIN; UPDATE t SET s='fatal' WHERE id=1")?;
            assert!(matches!(
                c.query("SELECT TRY_CAST(coercion_probe(s,id) AS VARCHAR) FROM t ORDER BY id"),
                Err(Error::Resource(_))
            ));
            c.execute("ROLLBACK")?;
            assert_eq!(
                c.query("SELECT s FROM t WHERE id=1")?.rows,
                vec![vec![Value::Varchar("2".into())]]
            );
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct LiteralProbe {
    bound: Option<(String, DataType)>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for LiteralProbe {
    fn name(&self) -> &str {
        "literal_probe"
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        _: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        if arguments.len() != 1 {
            return Err(Error::Bind("literal probe needs one argument".into()));
        }
        let source = arguments.data_type(0)?;
        let integer = arguments.integer_literal(0)?;
        let string = arguments.is_string_literal(0)?;
        let target = if integer.is_some_and(|n| (0..=255).contains(&n)) {
            DataType::UTinyInt
        } else {
            source.clone()
        };
        let text = format!("{source}/{integer:?}/{string}/{target}");
        Ok(Some(Arc::new(Self {
            bound: Some((text, target)),
        })))
    }
    fn argument_types(&self, _: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        Ok(vec![self.bound.as_ref().expect("bound probe").1.clone()])
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Varchar)
    }
    fn evaluate(&self, arguments: &[Value], _: &QueryContext) -> Result<Value> {
        let (text, target) = self.bound.as_ref().expect("bound probe");
        assert!(arguments[0].fits_type(target));
        Ok(Value::Varchar(format!("{text}/{}", arguments[0])))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn literal_provenance_survives_case_pruning_casts_parameters_and_overload_binding() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut functions = FunctionRegistry::builtins();
            functions.register_scalar(Arc::new(LiteralProbe::default()))?;
            let mut c = DatabaseBuilder::new()
                .functions(functions)
                .expressions(evaluator.clone())
                .optimizer(optimizer.clone())
                .batch_size(3)
                .build()?
                .connect();
            for (expression, expected) in [
                ("1", "INTEGER/Some(1)/false/UTINYINT/1"),
                ("(1)", "INTEGER/Some(1)/false/UTINYINT/1"),
                ("-1", "INTEGER/Some(-1)/false/INTEGER/-1"),
                ("+1", "INTEGER/None/false/INTEGER/1"),
                ("1::INTEGER", "INTEGER/None/false/INTEGER/1"),
                ("CAST(1 AS BIGINT)", "BIGINT/None/false/BIGINT/1"),
                (
                    "2147483648",
                    "BIGINT/Some(2147483648)/false/BIGINT/2147483648",
                ),
                (
                    "9223372036854775808",
                    "HUGEINT/Some(9223372036854775808)/false/HUGEINT/9223372036854775808",
                ),
                (
                    "CASE WHEN true THEN 1 ELSE 2 END",
                    "INTEGER/None/false/INTEGER/1",
                ),
                (
                    "CASE WHEN false THEN 2 ELSE 1 END",
                    "INTEGER/None/false/INTEGER/1",
                ),
                ("'1'", "VARCHAR/None/true/VARCHAR/1"),
                ("'1'::VARCHAR", "VARCHAR/None/false/VARCHAR/1"),
                (
                    "CASE WHEN true THEN '1' ELSE '2' END",
                    "VARCHAR/None/false/VARCHAR/1",
                ),
            ] {
                let sql = format!("SELECT literal_probe({expression}) FROM range(7)");
                assert_eq!(
                    c.query(&sql)?.rows,
                    vec![vec![Value::Varchar(expected.into())]; 7],
                    "{sql}"
                );
            }
            let prepared = c.prepare("SELECT literal_probe(?) FROM range(7)")?;
            for (value, expected) in [
                (Value::Integer(1), "INTEGER/None/false/INTEGER/1"),
                (
                    Value::Integer(2147483648),
                    "BIGINT/None/false/BIGINT/2147483648",
                ),
                (Value::Varchar("1".into()), "VARCHAR/None/false/VARCHAR/1"),
            ] {
                assert_eq!(
                    c.execute_prepared(&prepared, &[value])?.rows,
                    vec![vec![Value::Varchar(expected.into())]; 7]
                );
            }
            assert_eq!(c.query("SELECT typeof(1::UTINYINT+1), typeof(1::UTINYINT+CASE WHEN true THEN 1 ELSE 2 END), typeof(1::UBIGINT+9223372036854775807), typeof(1::UHUGEINT+9223372036854775808)")?.rows,
                vec![vec![Value::Varchar("UTINYINT".into()), Value::Varchar("INTEGER".into()), Value::Varchar("UBIGINT".into()), Value::Varchar("UHUGEINT".into())]]);
            let prepared = c.prepare("SELECT typeof(1::UTINYINT+?)")?;
            assert_eq!(
                c.execute_prepared(&prepared, &[Value::Integer(1)])?.rows,
                vec![vec![Value::Varchar("INTEGER".into())]]
            );
            // A pruned CASE stays constant for functions that explicitly ask
            // for values and for template NULL-result inference.
            assert_eq!(c.query("SELECT [4,5][CASE WHEN true THEN 2 ELSE 1 END],typeof((CASE WHEN true THEN NULL::VARCHAR ELSE 'x' END)||'a')")?.rows,
                vec![vec![Value::Integer(5),Value::Varchar("\"NULL\"".into())]]);
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
            Err(Error::Bind("outside arguments".into()))
        }
    }
    fn constant(&self, _: usize) -> Result<Value> {
        panic!("metadata default must not evaluate constants")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn other_frontends_do_not_implicitly_grant_sql_literal_identity() -> Result<()> {
    assert_eq!(ExternalArguments.integer_literal(0)?, None);
    assert!(!ExternalArguments.is_string_literal(0)?);
    assert!(matches!(
        ExternalArguments.integer_literal(1),
        Err(Error::Bind(_))
    ));
    assert!(matches!(
        ExternalArguments.is_string_literal(1),
        Err(Error::Bind(_))
    ));
    Ok(())
}
