use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    common::{
        cast::{CastMode, CastRegistry, CastSpec},
        type_registry::{
            TypeRegistry,
            ascii::{self, AsciiCast, StreamingAscii},
            builtin_types,
        },
    },
    function::{
        FunctionEffects,
        operator::{
            DynamicLike, GreedyLike, NumericArithmetic, Operator, OperatorArgument,
            OperatorFunction, OperatorRegistry, OperatorSignature,
        },
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::{InterruptHandle, QueryContext},
    planner::{BoundExpr, BoundStatement, ExprKind, Field, LogicalPlan, PlanNode},
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn signature(
    operator: Operator,
    arguments: Vec<DataType>,
    result: DataType,
    nullable: bool,
) -> OperatorSignature {
    OperatorSignature {
        operator,
        arguments,
        result,
        nullable,
    }
}
fn like_signature(operator: Operator) -> OperatorSignature {
    signature(
        operator,
        vec![DataType::Varchar; 2],
        DataType::Boolean,
        false,
    )
}
fn like_adapters() -> [Arc<dyn OperatorFunction>; 2] {
    [Arc::new(DynamicLike), Arc::new(GreedyLike)]
}

fn words(alphabet: &[char], max: usize) -> Vec<String> {
    let mut result = vec![String::new()];
    let mut previous = vec![String::new()];
    for _ in 0..max {
        let next: Vec<_> = previous
            .iter()
            .flat_map(|s| alphabet.iter().map(move |c| format!("{s}{c}")))
            .collect();
        result.extend(next.clone());
        previous = next;
    }
    result
}
fn oracle(value: &[char], pattern: &[char]) -> bool {
    match pattern.split_first() {
        None => value.is_empty(),
        Some(('%', rest)) => {
            oracle(value, rest) || (!value.is_empty() && oracle(&value[1..], pattern))
        }
        Some((c, rest)) => {
            !value.is_empty() && (*c == '_' || *c == value[0]) && oracle(&value[1..], rest)
        }
    }
}

#[test]
fn like_adapters_obey_the_same_unicode_wildcard_and_null_contracts() -> Result<()> {
    let values = words(&['a', 'b', '🦆', '\0'], 3);
    let patterns = words(&['a', 'b', '🦆', '\0', '%', '_'], 3);
    let query = QueryContext::background();
    for adapter in like_adapters() {
        let mut registry = OperatorRegistry::default();
        for op in [Operator::Like, Operator::NotLike] {
            registry.register(like_signature(op), adapter.clone())?;
        }
        for op in [Operator::Like, Operator::NotLike] {
            let function = registry.bind(
                op,
                &[DataType::Varchar, DataType::Varchar],
                &builtin_types(),
            )?;
            for value in &values {
                for pattern in &patterns {
                    let expected = oracle(
                        &value.chars().collect::<Vec<_>>(),
                        &pattern.chars().collect::<Vec<_>>(),
                    ) ^ (op == Operator::NotLike);
                    assert_eq!(
                        function.apply(
                            &[
                                Value::Varchar(value.clone()),
                                Value::Varchar(pattern.clone())
                            ],
                            &query
                        )?,
                        Value::Boolean(expected),
                        "{} {value:?} {pattern:?}",
                        adapter.name()
                    );
                }
            }
            for (value, pattern) in [
                ("🦆", "🦅"),
                ("x🦆🦅z", "%🦅_"),
                ("xéè", "%è"),
                ("x中丰", "%中_"),
                ("x中丰", "%丰"),
                ("🦆", "%🦅%"),
                ("🦆🦅", "_🦅"),
            ] {
                let expected = oracle(
                    &value.chars().collect::<Vec<_>>(),
                    &pattern.chars().collect::<Vec<_>>(),
                ) ^ (op == Operator::NotLike);
                assert_eq!(
                    function.apply(
                        &[Value::Varchar(value.into()), Value::Varchar(pattern.into())],
                        &query
                    )?,
                    Value::Boolean(expected)
                );
            }
            assert_eq!(
                function.apply(&[Value::Null, Value::Varchar("%".into())], &query)?,
                Value::Null
            );
            assert!(matches!(
                function.apply(&[Value::Integer(1), Value::Null], &query),
                Err(Error::Internal(_))
            ));
            assert!(matches!(
                function.apply(&[], &query),
                Err(Error::Internal(_))
            ));
        }
    }
    Ok(())
}

#[test]
fn numeric_overloads_preserve_literal_widths_division_and_overflow() -> Result<()> {
    let mut c = Database::memory()?.connect();
    let result = c.query("SELECT 1+2, 2::TINYINT+3::SMALLINT, 1::TINYINT+2, 5//2, NULL+NULL, 1::FLOAT/3::FLOAT, -5//2, -5%2")?;
    assert_eq!(
        result
            .columns
            .iter()
            .map(|f| f.data_type.clone())
            .collect::<Vec<_>>(),
        vec![
            DataType::Integer,
            DataType::SmallInt,
            DataType::TinyInt,
            DataType::Integer,
            DataType::BigInt,
            DataType::Float,
            DataType::Integer,
            DataType::Integer
        ]
    );
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Integer(3),
            Value::Integer(5),
            Value::Integer(3),
            Value::Integer(2),
            Value::Null,
            Value::Float(1.0 / 3.0),
            Value::Integer(-2),
            Value::Integer(-1)
        ]]
    );
    assert_eq!(
        c.query("SELECT 1//0, 1%0, 1::FLOAT//0::FLOAT, 1::DOUBLE//0::DOUBLE")?
            .rows,
        vec![vec![Value::Null; 4]]
    );
    for (data_type, min, max) in [
        ("TINYINT", i8::MIN as i128, i8::MAX as i128),
        ("SMALLINT", i16::MIN as i128, i16::MAX as i128),
        ("INTEGER", i32::MIN as i128, i32::MAX as i128),
        ("BIGINT", i64::MIN as i128, i64::MAX as i128),
        ("HUGEINT", i128::MIN, i128::MAX),
    ] {
        for expression in [
            format!("({max})::{data_type}+1"),
            format!("({min})::{data_type}-1"),
            format!("-(({min})::{data_type})"),
            format!("({min})::{data_type}//(-1)::{data_type}"),
            format!("({min})::{data_type}%(-1)::{data_type}"),
        ] {
            assert!(
                matches!(
                    c.query(&format!("SELECT {expression}")),
                    Err(Error::Execution(_))
                ),
                "{expression}"
            );
            assert_eq!(
                c.query(&format!(
                    "SELECT CASE WHEN false THEN {expression} ELSE 7 END"
                ))?
                .rows,
                vec![vec![Value::Integer(7)]]
            );
        }
    }
    assert!(matches!(
        c.query("SELECT TRY_CAST(127::TINYINT+1 AS BIGINT)"),
        Err(Error::Execution(_))
    ));
    assert!(matches!(
        c.execute("CREATE TABLE overflow(v INTEGER DEFAULT -((-128)::TINYINT))"),
        Err(Error::Execution(_))
    ));
    Ok(())
}

#[test]
fn date_operators_cover_extrema_infinities_offsets_and_prepared_parameters() -> Result<()> {
    let mut c = Database::memory()?.connect();
    let result = c.query("SELECT DATE 'epoch'+1, 1+DATE 'epoch', DATE '2000-03-01'-1, DATE 'epoch'-DATE '1969-12-31', DATE '5881580-07-10'-DATE '5877642-06-25 (BC)', DATE '-infinity'-DATE 'infinity', DATE 'infinity'+2147483647, DATE '-infinity'-(-2147483648), DATE 'epoch'-NULL")?;
    let date = |text: &str| Value::Date(text.parse().unwrap());
    assert_eq!(
        result.rows,
        vec![vec![
            date("1970-01-02"),
            date("1970-01-02"),
            date("2000-02-29"),
            Value::Integer(1),
            Value::Integer(4294967292),
            Value::Integer(-4294967294),
            date("infinity"),
            date("-infinity"),
            Value::Null
        ]]
    );
    for sql in [
        "SELECT DATE '5881580-07-10'+1",
        "SELECT DATE '5877642-06-25 (BC)'-1",
        "SELECT DATE 'epoch'+2147483647",
        "SELECT DATE 'epoch'+(-2147483648)",
    ] {
        assert!(matches!(c.query(sql), Err(Error::Execution(_))), "{sql}");
    }
    assert!(matches!(
        c.query("SELECT DATE 'epoch'+1::BIGINT"),
        Err(Error::Bind(_))
    ));
    let prepared = c.prepare("SELECT DATE 'epoch'+CAST($1 AS INTEGER)")?;
    for (offset, expected) in [(1, "1970-01-02"), (-1, "1969-12-31")] {
        assert_eq!(
            c.execute_prepared(&prepared, &[Value::Integer(offset)])?
                .rows,
            vec![vec![date(expected)]]
        );
    }
    Ok(())
}

#[test]
fn date_arithmetic_and_like_selection_survive_both_formats_and_optimizers() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let formats: Vec<Arc<dyn SnapshotFormat>> = vec![
        Arc::new(JsonSnapshotFormat),
        Arc::new(DuckDbFormat::default()),
    ];
    let optimizers: Vec<Arc<dyn Optimizer>> = vec![
        Arc::new(IdentityOptimizer),
        Arc::new(PipelineOptimizer::default()),
    ];
    for format in formats {
        for optimizer in &optimizers {
            for adapter in like_adapters() {
                let mut operators = OperatorRegistry::builtins();
                for op in [Operator::Like, Operator::NotLike] {
                    operators.replace(like_signature(op), adapter.clone())?;
                }
                let path = directory.path().join(format!(
                    "{}-{}-{}",
                    format.name(),
                    optimizer.name(),
                    adapter.name()
                ));
                let open = || {
                    DatabaseBuilder::new()
                        .operators(operators.clone())
                        .optimizer(optimizer.clone())
                        .durability(Arc::new(FileCheckpoint::open(
                            &path,
                            OpenMode::ReadWrite,
                            format.clone(),
                        )?))
                        .build()
                };
                {
                    let mut c = open()?.connect();
                    c.execute("CREATE TABLE t(d DATE PRIMARY KEY DEFAULT DATE '2000-03-01'-1, label VARCHAR DEFAULT 'du'||'ck'); INSERT INTO t DEFAULT VALUES; INSERT INTO t SELECT DATE 'epoch'+range::INTEGER, 'row-'||range FROM range(10)")?;
                }
                let mut c = open()?.connect();
                c.execute("BEGIN; DELETE FROM t WHERE d=DATE 'epoch'+1; ROLLBACK; UPDATE t SET d=d+1 WHERE label='duck'")?;
                assert_eq!(
                    c.query("SELECT label FROM t WHERE d=DATE 'epoch'+1")?.rows,
                    vec![vec![Value::Varchar("row-1".into())]]
                );
                assert_eq!(
                    c.query(
                        "SELECT count(*) FROM t WHERE label LIKE 'row-_' AND label NOT LIKE '%🦆%'"
                    )?
                    .rows,
                    vec![vec![Value::Integer(10)]]
                );
                assert_eq!(
                    c.query("SELECT d::VARCHAR FROM t WHERE label='duck'")?.rows,
                    vec![vec![Value::Varchar("2000-03-01".into())]]
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Identity;
impl OperatorFunction for Identity {
    fn name(&self) -> &'static str {
        "test-identity"
    }
    fn supports(&self, _: &OperatorSignature) -> bool {
        true
    }
    fn evaluate(
        &self,
        _: &OperatorSignature,
        arguments: &[Value],
        _: &QueryContext,
    ) -> Result<Value> {
        Ok(arguments[0].clone())
    }
}

#[test]
fn registry_checks_missing_ambiguous_invalid_and_retained_bindings() -> Result<()> {
    let casts = CastRegistry::builtins();
    let types = builtin_types();
    let query = QueryContext::background();
    let mut registry = OperatorRegistry::default();
    for t in [DataType::Boolean, DataType::Date] {
        registry.register(
            signature(Operator::Plus, vec![t.clone()], t, false),
            Arc::new(Identity),
        )?;
    }
    assert!(matches!(
        registry.resolve(
            Operator::Plus,
            &[OperatorArgument {
                data_type: &DataType::Null,
                integer_literal: None
            }],
            &casts,
            &types,
            &query
        ),
        Err(Error::Bind(_))
    ));
    assert!(
        registry
            .register(
                signature(
                    Operator::Add,
                    vec![DataType::Integer],
                    DataType::Integer,
                    false
                ),
                Arc::new(Identity)
            )
            .is_err()
    );
    assert!(
        registry
            .resolve(
                Operator::Plus,
                &[OperatorArgument {
                    data_type: &DataType::TinyInt,
                    integer_literal: Some(128)
                }],
                &casts,
                &types,
                &query
            )
            .is_err()
    );
    assert!(
        DatabaseBuilder::new()
            .operators(OperatorRegistry::default())
            .build()?
            .connect()
            .query("SELECT 1+2")
            .is_err()
    );
    let sig = like_signature(Operator::Like);
    registry.register(sig.clone(), Arc::new(DynamicLike))?;
    let retained = registry.bind(
        Operator::Like,
        &[DataType::Varchar, DataType::Varchar],
        &types,
    )?;
    registry.replace(sig.clone(), Arc::new(GreedyLike))?;
    assert_eq!(retained.adapter(), "like-dynamic-programming");
    assert_eq!(
        registry
            .bind(
                Operator::Like,
                &[DataType::Varchar, DataType::Varchar],
                &types
            )?
            .adapter(),
        "like-greedy"
    );
    let mut changed = sig.clone();
    changed.nullable = true;
    assert!(registry.replace(changed, Arc::new(Identity)).is_err());
    assert!(registry.register(sig, Arc::new(DynamicLike)).is_err());
    let bound = Arc::new(retained);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let bound = bound.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    assert_eq!(
                        bound
                            .apply(
                                &[Value::Varchar("ab🦆".into()), Value::Varchar("a%_".into())],
                                &QueryContext::background()
                            )
                            .unwrap(),
                        Value::Boolean(true)
                    );
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let invalid = BoundExpr {
        data_type: DataType::Boolean,
        kind: ExprKind::Operator(
            bound,
            vec![
                BoundExpr::literal(Value::Integer(1)),
                BoundExpr::literal(Value::Varchar("%".into())),
            ],
        ),
    };
    let plan = LogicalPlan {
        schema: vec![Field {
            name: "invalid".into(),
            qualifier: None,
            data_type: DataType::Boolean,
        }],
        node: PlanNode::Values(vec![vec![invalid]]),
    };
    assert!(matches!(
        Database::memory()?
            .connect()
            .execute_plan(BoundStatement::Query(plan)),
        Err(Error::Bind(_))
    ));
    Ok(())
}

#[derive(Debug)]
struct ObservedArithmetic(Arc<AtomicUsize>);
impl OperatorFunction for ObservedArithmetic {
    fn name(&self) -> &'static str {
        "test-observed-arithmetic"
    }
    fn supports(&self, s: &OperatorSignature) -> bool {
        NumericArithmetic.supports(s)
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: false,
        }
    }
    fn evaluate(&self, s: &OperatorSignature, a: &[Value], q: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        NumericArithmetic.evaluate(s, a, q)
    }
}

#[test]
fn effects_and_failed_folding_preserve_required_evaluation() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = OperatorRegistry::builtins();
    registry.replace(
        signature(
            Operator::Add,
            vec![DataType::Integer; 2],
            DataType::Integer,
            false,
        ),
        Arc::new(ObservedArithmetic(calls.clone())),
    )?;
    let mut c = DatabaseBuilder::new()
        .operators(registry)
        .build()?
        .connect();
    c.query("SELECT 1+2 FROM range(0)")?;
    c.query("SELECT CASE WHEN false THEN 1+2 ELSE 4 END")?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        c.execute("CREATE TABLE t(i INTEGER DEFAULT 1+2)"),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(c.query("SELECT 1+2")?.rows, vec![vec![Value::Integer(3)]]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[derive(Debug)]
struct InvalidResult(Value);
impl OperatorFunction for InvalidResult {
    fn name(&self) -> &'static str {
        "test-invalid-result"
    }
    fn supports(&self, _: &OperatorSignature) -> bool {
        true
    }
    fn evaluate(&self, _: &OperatorSignature, _: &[Value], _: &QueryContext) -> Result<Value> {
        Ok(self.0.clone())
    }
}

#[test]
fn bad_results_remain_errors_and_like_work_is_cancellable() -> Result<()> {
    for value in [Value::Null, Value::Boolean(true), Value::Integer(i128::MAX)] {
        let mut registry = OperatorRegistry::builtins();
        registry.replace(
            signature(
                Operator::Add,
                vec![DataType::Integer; 2],
                DataType::Integer,
                false,
            ),
            Arc::new(InvalidResult(value)),
        )?;
        let mut c = DatabaseBuilder::new()
            .operators(registry)
            .build()?
            .connect();
        assert!(matches!(
            c.query("SELECT TRY_CAST(1+2 AS BIGINT)"),
            Err(Error::Internal(_))
        ));
    }
    let value = Value::Varchar("a".repeat(100000));
    let pattern = Value::Varchar(format!("%{}b", "a".repeat(1000)));
    for adapter in like_adapters() {
        let mut registry = OperatorRegistry::default();
        registry.register(like_signature(Operator::Like), adapter)?;
        let function = registry.bind(
            Operator::Like,
            &[DataType::Varchar, DataType::Varchar],
            &builtin_types(),
        )?;
        let query = QueryContext::new(
            InterruptHandle::default(),
            Some(Duration::from_millis(2)),
            256,
            1000,
        )?;
        assert!(matches!(
            function.apply(&[value.clone(), pattern.clone()], &query),
            Err(Error::Interrupted)
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct AsciiAdd;
impl OperatorFunction for AsciiAdd {
    fn name(&self) -> &'static str {
        "test-ascii-add"
    }
    fn supports(&self, s: &OperatorSignature) -> bool {
        s.operator == Operator::Add
            && s.arguments.len() == 2
            && s.arguments.iter().all(|t| *t == s.result)
    }
    fn evaluate(&self, s: &OperatorSignature, a: &[Value], q: &QueryContext) -> Result<Value> {
        q.check()?;
        let [Value::Extension(left), Value::Extension(right)] = a else {
            return Err(Error::Internal("ASCII arguments".into()));
        };
        let mut bytes = left.bytes.clone();
        bytes.extend(&right.bytes);
        Ok(Value::extension(s.result.clone(), bytes))
    }
}

#[test]
fn extension_operators_use_existing_sql_callers_and_logical_validation() -> Result<()> {
    let t = ascii::data_type(16)?;
    let mut types = TypeRegistry::builtins();
    types.register(ascii::FAMILY, Arc::new(StreamingAscii))?;
    let mut casts = CastRegistry::builtins();
    casts.register_type(&t, &types)?;
    for mode in [CastMode::Explicit, CastMode::Assignment] {
        for (source, target) in [
            (DataType::Varchar, t.clone()),
            (t.clone(), DataType::Varchar),
        ] {
            casts.register(
                CastSpec {
                    source,
                    target,
                    mode,
                },
                Arc::new(AsciiCast),
            )?;
        }
    }
    let mut operators = OperatorRegistry::builtins();
    let sig = signature(Operator::Add, vec![t.clone(); 2], t.clone(), false);
    operators.register(sig.clone(), Arc::new(AsciiAdd))?;
    let mut c = DatabaseBuilder::new()
        .operators(operators.clone())
        .casts(casts.clone())
        .types(Arc::new(types.clone()))
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT ('du'::ascii_ci(16)+'ck'::ascii_ci(16))::VARCHAR")?
            .rows,
        vec![vec![Value::Varchar("duck".into())]]
    );
    operators.replace(sig, Arc::new(InvalidResult(Value::extension(t, vec![255]))))?;
    let mut c = DatabaseBuilder::new()
        .operators(operators)
        .casts(casts)
        .types(Arc::new(types))
        .build()?
        .connect();
    assert!(matches!(
        c.query("SELECT TRY_CAST('du'::ascii_ci(16)+'ck'::ascii_ci(16) AS VARCHAR)"),
        Err(Error::Internal(_))
    ));
    Ok(())
}
