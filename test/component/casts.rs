use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

mod casts_attempts;
mod casts_contexts;

use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    catalog::{CatalogMut, ColumnDefinition, TableDefinition, TableName},
    common::{
        cast::{
            BoundCast, CastFunction, CastMode, CastRegistry, CastSpec, DigitIntegerCast,
            PrimitiveCast,
        },
        vector::{DataChunk, Vector},
    },
    execution::{
        expression_executor::{ExpressionEvaluator, ScalarEvaluator},
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
    },
    parallel::{InterruptHandle, QueryContext},
    planner::{BoundExpr, BoundStatement, ExprKind, Field, LogicalPlan, PlanNode},
    storage::{
        TableStorage, TableStorageMut,
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
        table::Snapshot,
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn spec(target: DataType, mode: CastMode) -> CastSpec {
    CastSpec {
        source: DataType::Varchar,
        target,
        mode,
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_coercion_costs_rank_only_available_selected_casts() -> Result<()> {
    let query = QueryContext::background();
    let casts = CastRegistry::builtins();
    for (target, cost) in [
        (DataType::Date, 110),
        (DataType::Time, 110),
        (DataType::Interval, 110),
        (DataType::TimestampNs, 119),
        (DataType::Timestamp, 120),
        (DataType::TimestampMs, 121),
        (DataType::TimestampS, 122),
        (DataType::TimestampTz, 123),
        (DataType::TimestampTzNs, 124),
    ] {
        assert_eq!(
            casts.coercion_cost_with_types(
                &DataType::Null,
                &target,
                CastMode::Implicit,
                query.types()
            )?,
            Some(cost)
        );
        assert_eq!(
            casts.coercion_cost_with_types(&target, &target, CastMode::Implicit, query.types())?,
            Some(0)
        );
    }
    assert_eq!(
        casts.coercion_cost_with_types(
            &DataType::Date,
            &DataType::Time,
            CastMode::Implicit,
            query.types()
        )?,
        None
    );
    assert_eq!(
        casts.coercion_cost_with_types(
            &DataType::Varchar,
            &DataType::Timestamp,
            CastMode::Implicit,
            query.types()
        )?,
        None
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn integer_registry(adapter: Arc<dyn CastFunction>) -> Result<CastRegistry> {
    let mut registry = CastRegistry::builtins();
    for target in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
    ] {
        for mode in [CastMode::Explicit, CastMode::Assignment] {
            registry.replace(spec(target.clone(), mode), adapter.clone())?;
        }
    }
    Ok(registry)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn structural_registration_handles_null_and_is_atomic_on_conflict() -> Result<()> {
    let query = QueryContext::background();
    let mut registry = CastRegistry::default();
    registry.register_type(&DataType::Null, query.types())?;
    registry.register_type(&DataType::Integer, query.types())?;
    for mode in [CastMode::Implicit, CastMode::Assignment, CastMode::Explicit] {
        for target in [DataType::Null, DataType::Integer] {
            assert_eq!(
                registry
                    .bind(&DataType::Null, &target, mode, query.types())?
                    .apply(&Value::Null, &query)?,
                Value::Null
            );
        }
        assert_eq!(
            registry
                .bind(&DataType::Integer, &DataType::Integer, mode, query.types())?
                .apply(&Value::Integer(42), &query)?,
            Value::Integer(42)
        );
    }
    registry.register(
        CastSpec {
            source: DataType::Varchar,
            target: DataType::Varchar,
            mode: CastMode::Explicit,
        },
        Arc::new(PrimitiveCast),
    )?;
    assert!(
        registry
            .register_type(&DataType::Varchar, query.types())
            .is_err()
    );
    // A late conflict must not leave earlier NULL/identity modes installed.
    for mode in [CastMode::Implicit, CastMode::Assignment, CastMode::Explicit] {
        assert!(
            registry
                .bind(&DataType::Null, &DataType::Varchar, mode, query.types())
                .is_err()
        );
    }
    assert_eq!(
        registry
            .bind(
                &DataType::Varchar,
                &DataType::Varchar,
                CastMode::Explicit,
                query.types()
            )?
            .apply(&Value::Varchar("retained".into()), &query)?,
        Value::Varchar("retained".into())
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn cast_modes_and_numeric_boundaries_are_explicit() -> Result<()> {
    let registry = CastRegistry::builtins();
    let q = QueryContext::background();
    for (value, source, target, expected) in [
        (
            Value::Integer(127),
            DataType::Integer,
            DataType::TinyInt,
            Value::Integer(127),
        ),
        (
            Value::Double(-1.5),
            DataType::Double,
            DataType::Integer,
            Value::Integer(-2),
        ),
        (
            Value::Float(1.5),
            DataType::Float,
            DataType::Integer,
            Value::Integer(2),
        ),
        (
            Value::Integer(16777217),
            DataType::Integer,
            DataType::Float,
            Value::Float(16777216.0),
        ),
        (
            Value::Boolean(true),
            DataType::Boolean,
            DataType::Double,
            Value::Double(1.0),
        ),
        (
            Value::Null,
            DataType::Varchar,
            DataType::Integer,
            Value::Null,
        ),
    ] {
        for mode in [CastMode::Explicit, CastMode::Assignment] {
            assert_eq!(
                registry
                    .bind(
                        &source,
                        &target,
                        mode,
                        &duckdb_rust::common::type_registry::builtin_types()
                    )?
                    .apply(&value, &q)?,
                expected
            );
        }
    }
    for (source, target) in [
        (DataType::Varchar, DataType::Integer),
        (DataType::Double, DataType::Float),
        (DataType::BigInt, DataType::TinyInt),
    ] {
        assert!(matches!(
            registry.bind(
                &source,
                &target,
                CastMode::Implicit,
                &duckdb_rust::common::type_registry::builtin_types()
            ),
            Err(Error::Bind(_))
        ));
    }
    for (source, target) in [
        (DataType::Null, DataType::Boolean),
        (DataType::Float, DataType::Double),
        (DataType::SmallInt, DataType::BigInt),
    ] {
        registry.bind(
            &source,
            &target,
            CastMode::Implicit,
            &duckdb_rust::common::type_registry::builtin_types(),
        )?;
    }
    for (value, target) in [
        (Value::Integer(128), DataType::TinyInt),
        (Value::Double(f64::INFINITY), DataType::HugeInt),
        (Value::Double(f64::MAX), DataType::Float),
    ] {
        let cast = registry.bind(
            &value.data_type(),
            &target,
            CastMode::Explicit,
            &duckdb_rust::common::type_registry::builtin_types(),
        )?;
        assert!(matches!(cast.apply(&value, &q), Err(Error::Conversion(_))));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn integer_adapters_share_valid_invalid_and_extreme_input_contracts() -> Result<()> {
    let registries = [
        integer_registry(Arc::new(PrimitiveCast))?,
        integer_registry(Arc::new(DigitIntegerCast))?,
    ];
    let q = QueryContext::background();
    let mut inputs = [
        "",
        "+",
        "-",
        "++1",
        "--1",
        "1_0",
        "1.0",
        "1e2",
        "0x10",
        "１２",
        "1\0",
        "  +42\t",
        "\u{2003}-42\u{2003}",
        "170141183460469231731687303715884105728",
        "-170141183460469231731687303715884105729",
    ]
    .map(str::to_owned)
    .to_vec();
    for n in [
        i128::MIN,
        i128::MAX,
        -129,
        -128,
        127,
        128,
        -32769,
        32768,
        i64::MIN as i128,
        i64::MAX as i128 + 1,
    ] {
        inputs.push(n.to_string());
    }
    let mut state = 731_u128;
    for _ in 0..2048 {
        state = state.wrapping_mul(0xda942042e4dd58b5).wrapping_add(1701);
        inputs.push((state as i128).to_string());
    }
    for target in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
    ] {
        for mode in [CastMode::Explicit, CastMode::Assignment] {
            let casts = registries
                .iter()
                .map(|r| {
                    r.bind(
                        &DataType::Varchar,
                        &target,
                        mode,
                        &duckdb_rust::common::type_registry::builtin_types(),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            for input in &inputs {
                let value = Value::Varchar(input.clone());
                match (casts[0].apply(&value, &q), casts[1].apply(&value, &q)) {
                    (Ok(a), Ok(b)) => assert_eq!(a, b, "{input:?} -> {target}"),
                    (Err(Error::Conversion(_)), Err(Error::Conversion(_))) => {}
                    result => panic!("adapter disagreement for {input:?} -> {target}: {result:?}"),
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Broken(u8);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for Broken {
    fn name(&self) -> &'static str {
        "broken-cast"
    }
    fn supports(&self, _: &CastSpec) -> bool {
        self.0 != 0
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        match self.0 {
            1 => Ok(Value::Null),
            2 => Ok(Value::Varchar("wrong".into())),
            3 => Err(Error::Resource("cast resource failure".into())),
            4 => Err(Error::Conversion("invalid value".into())),
            _ => Err(Error::Interrupted),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn registry_replacement_ownership_and_try_cast_errors_are_checked() -> Result<()> {
    let mut registry = CastRegistry::builtins();
    let s = spec(DataType::Integer, CastMode::Explicit);
    let old = registry.bind(
        &s.source,
        &s.target,
        s.mode,
        &duckdb_rust::common::type_registry::builtin_types(),
    )?;
    assert!(
        registry
            .register(s.clone(), Arc::new(PrimitiveCast))
            .is_err()
    );
    assert!(registry.replace(s.clone(), Arc::new(Broken(0))).is_err());
    assert!(
        CastRegistry::default()
            .replace(s.clone(), Arc::new(PrimitiveCast))
            .is_err()
    );
    let q = QueryContext::background();
    let text = Value::Varchar("42".into());
    assert_eq!(old.apply(&text, &q)?, Value::Integer(42));
    assert!(matches!(
        old.apply(&Value::Integer(42), &q),
        Err(Error::Internal(_))
    ));
    for mode in 1..=5 {
        registry.replace(s.clone(), Arc::new(Broken(mode)))?;
        let cast = registry.bind(
            &s.source,
            &s.target,
            s.mode,
            &duckdb_rust::common::type_registry::builtin_types(),
        )?;
        assert_eq!(cast.apply(&Value::Null, &q)?, Value::Null);
        let expr = BoundExpr {
            kind: ExprKind::Cast(
                Box::new(BoundExpr::literal(text.clone())),
                cast.into(),
                true,
            ),
            data_type: DataType::Integer,
        };
        match (mode, ScalarEvaluator.evaluate(&expr, &vec![], &q)) {
            (1 | 2, Err(Error::Internal(_)))
            | (3, Err(Error::Resource(_)))
            | (4, Ok(Value::Null))
            | (5, Err(Error::Interrupted)) => {}
            result => panic!("TRY_CAST swallowed an adapter failure: {result:?}"),
        }
    }
    assert_eq!(old.apply(&text, &q)?, Value::Integer(42));
    let interrupt = InterruptHandle::default();
    let q = QueryContext::new(interrupt.clone(), None, 64, 100)?;
    interrupt.interrupt();
    assert!(matches!(
        old.apply(&Value::Null, &q),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct Observed {
    inner: Arc<dyn CastFunction>,
    explicit: AtomicUsize,
    assignment: AtomicUsize,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for Observed {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        self.inner.supports(spec)
    }
    fn cast(&self, value: &Value, spec: &CastSpec, q: &QueryContext) -> Result<Value> {
        match spec.mode {
            CastMode::Explicit => &self.explicit,
            CastMode::Assignment => &self.assignment,
            CastMode::Implicit => panic!("unexpected implicit string conversion"),
        }
        .fetch_add(1, Ordering::Relaxed);
        self.inner.cast(value, spec, q)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn adapters_are_used_by_sql_defaults_mutations_indexes_and_both_formats() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut id = 0;
    for adapter in [
        Arc::new(PrimitiveCast) as Arc<dyn CastFunction>,
        Arc::new(DigitIntegerCast),
    ] {
        for indexes in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            for format in [
                Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
                Arc::new(DuckDbFormat::default()),
            ] {
                id += 1;
                let path = dir.path().join(format!("case{id}.db"));
                let adapter = Arc::new(Observed {
                    inner: adapter.clone(),
                    explicit: AtomicUsize::new(0),
                    assignment: AtomicUsize::new(0),
                });
                let casts = integer_registry(adapter.clone())?;
                {
                    let db = DatabaseBuilder::new()
                        .casts(casts.clone())
                        .indexes(indexes.clone())
                        .durability(Arc::new(FileCheckpoint::open(
                            &path,
                            OpenMode::ReadWrite,
                            format.clone(),
                        )?))
                        .build()?;
                    assert!(db.adapters().contains(&("casts", adapter.name())));
                    let mut c = db.connect();
                    c.execute("CREATE TABLE t(i INTEGER PRIMARY KEY, n SMALLINT DEFAULT '7'); INSERT INTO t(i) VALUES ('41'),('42'); UPDATE t SET n='8' WHERE i=41")?;
                    assert!(adapter.assignment.load(Ordering::Relaxed) >= 4);
                    assert_eq!(
                        c.query("SELECT i FROM t WHERE i=CAST('42' AS INTEGER)")?
                            .rows,
                        vec![vec![Value::Integer(42)]]
                    );
                    assert!(adapter.explicit.load(Ordering::Relaxed) > 0);
                    let p = c.prepare("SELECT $1::INTEGER, TRY_CAST($2 AS INTEGER)")?;
                    assert_eq!(
                        c.execute_prepared(
                            &p,
                            &[Value::Varchar("+17".into()), Value::Varchar("bad".into())]
                        )?
                        .rows,
                        vec![vec![Value::Integer(17), Value::Null]]
                    );
                    c.execute("BEGIN; INSERT INTO t(i) VALUES ('43'); ROLLBACK")?;
                    assert!(c.execute("INSERT INTO t(i) VALUES ('44'),('bad')").is_err());
                    assert_eq!(
                        c.query("SELECT i,n FROM t ORDER BY i")?.rows,
                        vec![
                            vec![Value::Integer(41), Value::Integer(8)],
                            vec![Value::Integer(42), Value::Integer(7)]
                        ]
                    );
                }
                let db = DatabaseBuilder::new()
                    .casts(casts)
                    .indexes(indexes.clone())
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format,
                    )?))
                    .build()?;
                assert_eq!(
                    db.connect().query("SELECT sum(n) FROM t")?.rows,
                    vec![vec![Value::Integer(15)]]
                );
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn representation_boundaries_never_perform_hidden_casts() -> Result<()> {
    let text = Value::Varchar("42".into());
    assert!(Vector::flat(DataType::Integer, vec![text.clone()]).is_err());
    assert!(Vector::constant(DataType::Integer, text.clone(), 3).is_err());
    assert!(DataChunk::from_rows(&[DataType::Integer], &[vec![text.clone()]]).is_err());
    let table = TableName::main("t");
    let mut snapshot = Snapshot::default();
    snapshot.create_table(
        TableDefinition {
            name: table.clone(),
            columns: vec![ColumnDefinition::new("i", DataType::Integer)],
            unique_keys: vec![],
        },
        false,
    )?;
    let q = QueryContext::background();
    snapshot.insert(&table, vec![vec![Value::Integer(1)]], &q)?;
    assert!(
        snapshot
            .insert(&table, vec![vec![Value::Integer(2)], vec![text]], &q)
            .is_err()
    );
    assert_eq!(snapshot.scan(&table, &q)?.len(), 1);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn cast_plan(cast: BoundCast, value: Value, target: DataType) -> BoundStatement {
    BoundStatement::Query(LogicalPlan {
        schema: vec![Field::new("cast", target.clone())],
        node: PlanNode::Values(vec![vec![BoundExpr {
            data_type: target,
            kind: ExprKind::Cast(Box::new(BoundExpr::literal(value)), cast.into(), false),
        }]]),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bound_plans_validate_retained_cast_signatures() -> Result<()> {
    let registry = integer_registry(Arc::new(DigitIntegerCast))?;
    let cast = registry.bind(
        &DataType::Varchar,
        &DataType::Integer,
        CastMode::Explicit,
        &duckdb_rust::common::type_registry::builtin_types(),
    )?;
    let mut c = DatabaseBuilder::new()
        .casts(CastRegistry::default())
        .build()?
        .connect();
    assert_eq!(
        c.execute_plan(cast_plan(
            cast.clone(),
            Value::Varchar("42".into()),
            DataType::Integer
        ))?
        .rows,
        vec![vec![Value::Integer(42)]]
    );
    assert!(matches!(
        c.execute_plan(cast_plan(
            cast.clone(),
            Value::Boolean(true),
            DataType::Integer
        )),
        Err(Error::Bind(_))
    ));
    assert!(matches!(
        c.execute_plan(cast_plan(
            cast,
            Value::Varchar("42".into()),
            DataType::Double
        )),
        Err(Error::Bind(_))
    ));
    assert!(matches!(c.query("SELECT 42::VARCHAR"), Err(Error::Bind(_))));
    Ok(())
}

struct InterruptingCast(InterruptHandle);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for InterruptingCast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InterruptingCast")
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for InterruptingCast {
    fn name(&self) -> &'static str {
        "interrupting-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        DigitIntegerCast.supports(spec)
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        self.0.interrupt();
        Err(Error::Conversion(
            "conversion failed after interruption".into(),
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn casts_preserve_concurrent_ownership_and_statement_cancellation() -> Result<()> {
    let registry = integer_registry(Arc::new(DigitIntegerCast))?;
    let cast = registry.bind(
        &DataType::Varchar,
        &DataType::BigInt,
        CastMode::Explicit,
        &duckdb_rust::common::type_registry::builtin_types(),
    )?;
    let threads: Vec<_> = (0..4)
        .map(|thread| {
            let cast = cast.clone();
            std::thread::spawn(move || -> Result<()> {
                let q = QueryContext::background();
                for i in 0..1024 {
                    let value = thread * 1024 + i;
                    assert_eq!(
                        cast.apply(&Value::Varchar(value.to_string()), &q)?,
                        Value::Integer(value)
                    );
                }
                Ok(())
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("cast worker panic")?;
    }

    let interrupt = InterruptHandle::default();
    let registry = integer_registry(Arc::new(InterruptingCast(interrupt.clone())))?;
    let cast = registry.bind(
        &DataType::Varchar,
        &DataType::Integer,
        CastMode::Explicit,
        &duckdb_rust::common::type_registry::builtin_types(),
    )?;
    let q = QueryContext::new(interrupt, None, 64, 100)?;
    assert!(matches!(
        cast.apply(&Value::Varchar("bad".into()), &q),
        Err(Error::Interrupted)
    ));

    let mut c = DatabaseBuilder::new().build()?.connect();
    c.execute("BEGIN; CREATE TABLE pending(i INTEGER)")?;
    c.set_timeout(Some(std::time::Duration::ZERO));
    assert!(matches!(
        c.query("SELECT CAST('42' AS INTEGER)"),
        Err(Error::Interrupted)
    ));
    c.set_timeout(None);
    // A binding failure retains the existing explicit transaction.
    c.execute("INSERT INTO pending VALUES (42); COMMIT")?;
    assert_eq!(
        c.query("SELECT * FROM pending")?.rows,
        vec![vec![Value::Integer(42)]]
    );
    Ok(())
}
