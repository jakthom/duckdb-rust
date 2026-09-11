use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    common::cast::{CastFunction, CastSpec},
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionRegistry, ScalarBindArguments, ScalarSignature},
    main::settings::{Configuration, SettingRegistry, SnapshotConfiguration},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::InterruptHandle,
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn assert_number(value: &Value, expected: f64) {
    let Value::Double(actual) = value else {
        panic!("expected DOUBLE, received {value:?}")
    };
    if expected.is_nan() {
        assert!(actual.is_nan());
    } else {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ieee_math_uses_selected_double_overloads_and_source_mode_error_boundaries() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            for mode in ["true", "false", "NULL"] {
                c.execute(&format!("SET ieee_floating_point_ops={mode}"))?;
                let result=c.query("SELECT sqrt(4),sqrt('-0.0'::DOUBLE),ln(1),log(100),log10(100),log2(8),log(2,8),pow(2,3),power(2,3),sqrt(NULL)")?;
                assert!(
                    result
                        .columns
                        .iter()
                        .all(|c| c.data_type == DataType::Double)
                );
                for (value, expected) in result.rows[0]
                    .iter()
                    .zip([2.0, -0.0, 0.0, 2.0, 2.0, 3.0, 3.0, 8.0, 8.0])
                {
                    assert_number(value, expected);
                }
                assert_eq!(result.rows[0][9], Value::Null);
                for (expression, expected) in [
                    ("sqrt(-1)", f64::NAN),
                    ("sqrt('-inf'::DOUBLE)", f64::NAN),
                    ("ln(0)", f64::NEG_INFINITY),
                    ("log(-1)", f64::NAN),
                    ("log2(0)", f64::NEG_INFINITY),
                    ("log10(-1)", f64::NAN),
                    ("log(1,2)", f64::INFINITY),
                    ("pow(0,-1)", f64::INFINITY),
                ] {
                    let result = c.query(&format!("SELECT {expression}"));
                    if mode == "false" {
                        assert!(
                            matches!(result, Err(Error::OutOfRange(_))),
                            "{expression}: {result:?}"
                        );
                    } else {
                        assert_number(&result?.rows[0][0], expected);
                    }
                }
                // Strict POW does not make all NaNs/infinities into errors.
                assert_number(&c.query("SELECT pow(-1,0.5)")?.rows[0][0], f64::NAN);
                assert_number(&c.query("SELECT pow(1e308,2)")?.rows[0][0], f64::INFINITY);
                assert_number(&c.query("SELECT sqrt('nan'::DOUBLE)")?.rows[0][0], f64::NAN);
                assert_eq!(
                    c.query("SELECT CASE WHEN false THEN sqrt(-1) ELSE 1 END")?
                        .rows,
                    vec![vec![Value::Double(1.0)]]
                );
            }
            for kind in [
                "TINYINT",
                "SMALLINT",
                "INTEGER",
                "BIGINT",
                "HUGEINT",
                "UTINYINT",
                "USMALLINT",
                "UINTEGER",
                "UBIGINT",
                "UHUGEINT",
                "FLOAT",
                "DOUBLE",
                "DECIMAL(12,2)",
                "BIGNUM",
            ] {
                assert_eq!(
                    c.query(&format!("SELECT sqrt(4::{kind}),typeof(sqrt(4::{kind}))"))?
                        .rows,
                    vec![vec![Value::Double(2.0), Value::Varchar("DOUBLE".into())]]
                );
            }
            assert_eq!(
                c.query("SELECT sqrt('4')")?.rows,
                vec![vec![Value::Double(2.0)]]
            );
            for expression in [
                "sqrt('4'::VARCHAR)",
                "sqrt(true)",
                "sqrt(DATE '2024-01-01')",
                "sqrt([4])",
                "sqrt()",
                "sqrt(1,2)",
                "log()",
                "log(1,2,3)",
                "pow(1)",
            ] {
                assert!(
                    matches!(
                        c.query(&format!("SELECT {expression}")),
                        Err(Error::Bind(_))
                    ),
                    "{expression}"
                );
            }
            let prepared = c.prepare("SELECT sqrt($1)")?;
            assert_eq!(
                c.execute_prepared(&prepared, &[Value::Integer(4)])?.rows,
                vec![vec![Value::Double(2.0)]]
            );
            assert!(matches!(
                c.execute_prepared(&prepared, &[Value::Varchar("4".into())]),
                Err(Error::Bind(_))
            ));
            c.execute("SET ieee_floating_point_ops=false")?;
            assert!(matches!(
                c.execute_prepared(&prepared, &[Value::Integer(-1)]),
                Err(Error::OutOfRange(_))
            ));
            c.execute("SET ieee_floating_point_ops=true")?;
            assert_number(
                &c.execute_prepared(&prepared, &[Value::Integer(-1)])?.rows[0][0],
                f64::NAN,
            );
        }
    }
    Ok(())
}

struct Metadata {
    count: usize,
    selected: usize,
    known_null: bool,
}

struct Unselected;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for Unselected {
    fn len(&self) -> usize {
        1
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        Metadata {
            count: 1,
            selected: 0,
            known_null: false,
        }
        .data_type(index)
    }
    fn constant(&self, _: usize) -> Result<Value> {
        panic!("missing overload capability must not evaluate arguments")
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for Metadata {
    fn len(&self) -> usize {
        self.count
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        if index < self.count {
            Ok(DataType::Double)
        } else {
            Err(Error::Bind("argument outside metadata".into()))
        }
    }
    fn constant(&self, _: usize) -> Result<Value> {
        panic!("IEEE binding must not evaluate arguments")
    }
    fn select_overload(&self, _: &str, _: &[ScalarSignature]) -> Result<usize> {
        Ok(self.selected)
    }
    fn is_provably_null(&self, index: usize) -> Result<bool> {
        self.data_type(index)?;
        Ok(self.known_null)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn mode_context(mode: Value) -> Result<QueryContext> {
    let query = QueryContext::background();
    let registry = Arc::new(SettingRegistry::builtins());
    let configuration = SnapshotConfiguration::new(registry.clone());
    let mut session = configuration.connect();
    session.apply(
        &registry.bind("ieee_floating_point_ops", None, Some(mode), &query)?,
        &query,
    )?;
    Ok(query.with_settings(session.snapshot(&QueryContext::background())?))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ieee_bound_callbacks_keep_selected_mode_without_ambient_rebinding_or_argument_evaluation()
-> Result<()> {
    let registry = FunctionRegistry::builtins();
    let sqrt = registry.scalar("sqrt")?;
    let on = mode_context(Value::Boolean(true))?;
    let off = mode_context(Value::Boolean(false))?;
    assert!(matches!(
        sqrt.bind(&Unselected, &on),
        Err(Error::Unsupported(_))
    ));
    let args = Metadata {
        count: 1,
        selected: 0,
        known_null: false,
    };
    let bound_on = sqrt.bind(&args, &on)?.unwrap();
    let bound_off = sqrt.bind(&args, &off)?.unwrap();
    assert_number(&bound_on.evaluate(&[Value::Double(-1.0)], &off)?, f64::NAN);
    assert!(matches!(
        bound_off.evaluate(&[Value::Double(-1.0)], &on),
        Err(Error::OutOfRange(_))
    ));
    let null_mode = mode_context(Value::Null)?;
    assert_eq!(
        null_mode
            .settings()
            .get("ieee_floating_point_ops", &null_mode)?,
        &Value::Null
    );
    assert_number(
        &sqrt
            .bind(&args, &null_mode)?
            .unwrap()
            .evaluate(&[Value::Double(-1.0)], &off)?,
        f64::NAN,
    );
    assert!(matches!(
        sqrt.evaluate(&[Value::Double(1.0)], &on),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        sqrt.bind(
            &Metadata {
                count: 1,
                selected: 9,
                known_null: false,
            },
            &on
        ),
        Err(Error::Internal(_))
    ));
    assert!(matches!(
        sqrt.bind(
            &Metadata {
                count: 2,
                selected: 0,
                known_null: false,
            },
            &on
        ),
        Err(Error::Internal(_))
    ));
    assert!(matches!(
        bound_on.evaluate(&[Value::Integer(4)], &on),
        Err(Error::Internal(_))
    ));
    // A proven NULL bypasses the native IEEE bind callback/settings read, but
    // still requires the selected signature and correct TypeOnly invocation.
    let known_null = sqrt
        .bind(
            &Metadata {
                count: 1,
                selected: 0,
                known_null: true,
            },
            &QueryContext::background(),
        )?
        .unwrap();
    assert_eq!(known_null.evaluate(&[], &on)?, Value::Null);
    assert!(matches!(
        known_null.evaluate(&[Value::Null], &on),
        Err(Error::Internal(_))
    ));
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
    interrupt.interrupt();
    assert!(matches!(
        sqrt.bind(&args, &cancelled),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        bound_on.evaluate(&[Value::Double(4.0)], &cancelled),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct SelectedDouble(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedDouble {
    fn name(&self) -> &'static str {
        "selected-ieee-double-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer
            && spec.target == DataType::Double
            && matches!(spec.mode, CastMode::Implicit | CastMode::Explicit)
    }
    fn cast(&self, _: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if self.0 {
            Err(Error::Resource("selected IEEE cast budget".into()))
        } else {
            Ok(Value::Double(81.0))
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ieee_math_retains_selected_casts_and_constant_null_demand_in_both_evaluators() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for fail in [false, true] {
            let mut casts = CastRegistry::builtins();
            casts.replace(
                CastSpec {
                    source: DataType::Integer,
                    target: DataType::Double,
                    mode: CastMode::Implicit,
                },
                Arc::new(SelectedDouble(fail)),
            )?;
            casts.replace(
                CastSpec {
                    source: DataType::Integer,
                    target: DataType::Double,
                    mode: CastMode::Explicit,
                },
                Arc::new(SelectedDouble(fail)),
            )?;
            let mut c = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .casts(casts)
                .build()?
                .connect();
            let result = c.query("SELECT sqrt(4::INTEGER)");
            if fail {
                assert!(matches!(result, Err(Error::Resource(_))));
            } else {
                assert_eq!(result?.rows, vec![vec![Value::Double(9.0)]]);
            }
            assert_eq!(
                c.query("SELECT pow(NULL::DOUBLE,CAST('bad' AS DOUBLE))")?
                    .rows,
                vec![vec![Value::Null]]
            );
            assert_eq!(
                c.query("SELECT pow(CAST('bad' AS DOUBLE),NULL::DOUBLE)")?
                    .rows,
                vec![vec![Value::Null]]
            );
            assert_eq!(
                c.query("SELECT log(CAST('bad' AS DOUBLE),NULL::DOUBLE)")?
                    .rows,
                vec![vec![Value::Null]]
            );
            let selected_probe = c.query("SELECT pow(4::DOUBLE,NULL::DOUBLE)");
            if fail {
                assert!(matches!(selected_probe, Err(Error::Resource(_))));
            } else {
                assert_eq!(selected_probe?.rows, vec![vec![Value::Null]]);
            }
            // TypeOnly does not invoke a cast inserted after the NULL probe.
            assert_eq!(
                c.query("SELECT pow(4::INTEGER,NULL::DOUBLE)")?.rows,
                vec![vec![Value::Null]]
            );
            assert!(matches!(c.query("SELECT pow(a,CAST('bad' AS DOUBLE)) FROM(VALUES(NULL::DOUBLE),(NULL::DOUBLE))t(a)"),Err(Error::Conversion(_))));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn verify_math_rows(c: &mut duckdb_rust::main::Connection) -> Result<()> {
    assert_eq!(c.query("SELECT k,sqrt(v),pow(sqrt(v),2),sum(sqrt(v)) OVER(ORDER BY k ROWS UNBOUNDED PRECEDING) FROM math WHERE k<4 ORDER BY k")?.rows,
        vec![vec![Value::Integer(1),Value::Double(1.0),Value::Double(1.0),Value::Double(1.0)],vec![Value::Integer(2),Value::Double(2.0),Value::Double(4.0),Value::Double(3.0)],vec![Value::Integer(3),Value::Double(3.0),Value::Double(9.0),Value::Double(6.0)]]);
    assert_eq!(
        c.query(
            "SELECT a.k FROM math a JOIN math b ON sqrt(a.v)=sqrt(b.v) WHERE a.k<4 ORDER BY a.k"
        )?
        .rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)]
        ]
    );
    assert_eq!(
        c.query("SELECT sqrt(v),count(*) FROM math WHERE k<4 GROUP BY sqrt(v) ORDER BY 1")?
            .rows,
        vec![
            vec![Value::Double(1.0), Value::Integer(1)],
            vec![Value::Double(2.0), Value::Integer(1)],
            vec![Value::Double(3.0), Value::Integer(1)]
        ]
    );
    assert_eq!(c.query("SELECT concat([sqrt(v),ln(1)]::VARCHAR,' / ',sqrt(v)),{'answer':sqrt(v)}::VARCHAR FROM math WHERE k=2")?.rows,
        vec![vec![Value::Varchar("[2.0, 0.0] / 2.0".into()),Value::Varchar("{'answer': 2.0}".into())]]);
    assert_number(
        &c.query("SELECT result FROM math WHERE k=4")?.rows[0][0],
        f64::NAN,
    );
    assert_number(
        &c.query("SELECT result FROM math WHERE k=5")?.rows[0][0],
        f64::NEG_INFINITY,
    );
    assert_eq!(
        c.query("SELECT result FROM math WHERE k=6")?.rows,
        vec![vec![Value::Null]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn populate_math(c: &mut duckdb_rust::main::Connection) -> Result<()> {
    c.execute("CREATE TABLE math(k INTEGER PRIMARY KEY,v DOUBLE,result DOUBLE);INSERT INTO math VALUES(1,1,sqrt(1)),(2,4,sqrt(4)),(3,9,sqrt(9)),(4,-1,sqrt(-1)),(5,0,ln(0)),(6,NULL,sqrt(NULL))")?;
    let prepared = c.prepare("UPDATE math SET result=pow(sqrt(v),$1) WHERE k=$2")?;
    c.execute_prepared(&prepared, &[Value::Integer(2), Value::Integer(2)])?;
    assert_eq!(
        c.query("SELECT result FROM math WHERE k=2")?.rows,
        vec![vec![Value::Double(4.0)]]
    );
    c.execute("BEGIN;UPDATE math SET result=99;ROLLBACK;SET ieee_floating_point_ops=false")?;
    assert!(matches!(
        c.execute("UPDATE math SET result=sqrt(v)"),
        Err(Error::OutOfRange(_))
    ));
    assert_eq!(
        c.query("SELECT result FROM math WHERE k=2")?.rows,
        vec![vec![Value::Double(4.0)]]
    );
    c.execute("SET ieee_floating_point_ops=true")?;
    verify_math_rows(c)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ieee_math_values_cross_relations_parameters_atomic_mutations_and_reopen() -> Result<()> {
    for format in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("ieee-math.snapshot");
        let durability = Arc::new(FileCheckpoint::open(
            &path,
            OpenMode::ReadWrite,
            format.clone(),
        )?);
        let database = DatabaseBuilder::new().durability(durability).build()?;
        let mut c = database.connect();
        populate_math(&mut c)?;
        drop(c);
        drop(database);
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                format,
            )?))
            .build()?;
        verify_math_rows(&mut database.connect())?;
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("ieee-math-wal.duckdb");
    {
        let database = Database::open(&path)?;
        populate_math(&mut database.connect())?;
    }
    let database = Database::open(&path)?;
    let mut c = database.connect();
    verify_math_rows(&mut c)?;
    c.execute("CHECKPOINT")?;
    drop(c);
    drop(database);
    verify_math_rows(&mut Database::open(&path)?.connect())?;
    Ok(())
}
