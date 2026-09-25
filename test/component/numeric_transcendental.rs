use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{
        FunctionEffects, FunctionRegistry, ScalarBindArguments, ScalarFunction, ScalarSignature,
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::InterruptHandle,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn assert_double(value: &Value, expected: f64) {
    let Value::Double(actual) = value else {
        panic!("expected DOUBLE, received {value:?}")
    };
    if expected.is_nan() {
        assert!(actual.is_nan(), "expected NaN, received {actual}");
    } else {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn evaluators() -> [Arc<dyn ExpressionEvaluator>; 2] {
    [Arc::new(ScalarEvaluator), Arc::new(BatchedEvaluator)]
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn optimizers() -> [Arc<dyn Optimizer>; 2] {
    [
        Arc::new(IdentityOptimizer),
        Arc::new(PipelineOptimizer::default()),
    ]
}

#[derive(Debug)]
struct PredicateError;

#[derive(Debug)]
struct NextValue(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for NextValue {
    fn name(&self) -> &str {
        "nextval"
    }

    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: true,
        }
    }

    fn argument_types(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if arguments.len() == 1 {
            Ok(vec![DataType::Varchar])
        } else {
            Err(Error::Bind("nextval accepts one argument".into()))
        }
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments == [DataType::Varchar] {
            Ok(DataType::Integer)
        } else {
            Err(Error::Bind("nextval requires a VARCHAR name".into()))
        }
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [Value::Varchar(_)] = arguments else {
            return Err(Error::Internal("bound nextval arguments".into()));
        };
        Ok(Value::Integer(
            self.0.fetch_add(1, Ordering::SeqCst) as i128 + 1,
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for PredicateError {
    fn name(&self) -> &str {
        "error"
    }

    fn argument_types(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if arguments.len() == 1 {
            Ok(vec![DataType::Varchar])
        } else {
            Err(Error::Bind("error accepts one argument".into()))
        }
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments == [DataType::Varchar] {
            Ok(DataType::Integer)
        } else {
            Err(Error::Bind("error requires a VARCHAR message".into()))
        }
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [Value::Varchar(message)] = arguments else {
            return Err(Error::Internal("bound error arguments".into()));
        };
        Err(Error::InvalidInput(message.clone()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn test_functions(calls: Arc<AtomicUsize>) -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(PredicateError))?;
    functions.register_scalar(Arc::new(NextValue(calls)))?;
    Ok(functions)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn transcendental_values_types_overloads_parameters_and_batches_match_pins() -> Result<()> {
    for evaluator in evaluators() {
        for optimizer in optimizers() {
            let mut connection = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            let result = connection.query(
                "SELECT acos(0.5),asin(0.5),atan(1),atan2(1,0),cbrt(-8),
                        cos(0),cot(pi()/4),sin(pi()/2),tan(0),
                        cosh(0),sinh(0),tanh(0),acosh(1),asinh(0),atanh(0.5),
                        degrees(pi()),radians(180),exp(1),pi(),
                        signbit('-0.0'::DOUBLE),signbit(0::FLOAT),
                        even(2.1),even(-2.1),
                        nextafter(1::DOUBLE,2::DOUBLE),nextafter(1::FLOAT,2::FLOAT)",
            )?;
            let expected = [
                1.047_197_551_196_597_6,
                0.523_598_775_598_298_8,
                std::f64::consts::FRAC_PI_4,
                std::f64::consts::FRAC_PI_2,
                -2.0,
                1.0,
                1.000_000_000_000_000_2,
                1.0,
                0.0,
                1.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.549_306_144_334_054_9,
                180.0,
                std::f64::consts::PI,
                std::f64::consts::E,
                std::f64::consts::PI,
            ];
            for (value, expected) in result.rows[0][..19].iter().zip(expected) {
                assert_double(value, expected);
            }
            assert_eq!(result.rows[0][19], Value::Boolean(true));
            assert_eq!(result.rows[0][20], Value::Boolean(false));
            assert_double(&result.rows[0][21], 4.0);
            assert_double(&result.rows[0][22], -4.0);
            assert_double(&result.rows[0][23], f64::from_bits(1.0_f64.to_bits() + 1));
            assert_eq!(
                result.rows[0][24],
                Value::Float(f32::from_bits(1.0_f32.to_bits() + 1))
            );
            assert!(result.columns[..24].iter().enumerate().all(
                |(index, column)| column.data_type
                    == if matches!(index, 19 | 20) {
                        DataType::Boolean
                    } else {
                        DataType::Double
                    }
            ));
            assert_eq!(result.columns[24].data_type, DataType::Float);

            let boundaries = &connection
                .query(
                    "SELECT nextafter('0.0'::DOUBLE,'-0.0'::DOUBLE),
                            nextafter(0::DOUBLE,-1::DOUBLE),
                            nextafter('inf'::DOUBLE,0::DOUBLE),
                            nextafter('1.7976931348623157e308'::DOUBLE,'inf'::DOUBLE),
                            nextafter('nan'::DOUBLE,0::DOUBLE),
                            signbit(even('-0.0'::DOUBLE)),
                            even('inf'::DOUBLE),even('nan'::DOUBLE)",
                )?
                .rows[0];
            assert_double(&boundaries[0], -0.0);
            assert_double(&boundaries[1], f64::from_bits((1_u64 << 63) | 1));
            assert_double(&boundaries[2], f64::from_bits(f64::INFINITY.to_bits() - 1));
            assert_double(&boundaries[3], f64::INFINITY);
            assert_double(&boundaries[4], f64::NAN);
            assert_eq!(boundaries[5], Value::Boolean(true));
            assert_double(&boundaries[6], f64::INFINITY);
            assert_double(&boundaries[7], f64::NAN);

            assert_eq!(
                connection
                    .query(
                        "SELECT typeof(signbit(1)),typeof(signbit(NULL)),
                                typeof(nextafter(1,2)),typeof(nextafter(NULL,NULL)),
                                typeof(nextafter(1::FLOAT,2)),
                                typeof(nextafter(1::DOUBLE,2::FLOAT)),typeof(cbrt(8::BIGNUM))"
                    )?
                    .rows,
                vec![vec![
                    Value::Varchar("BOOLEAN".into()),
                    Value::Varchar("BOOLEAN".into()),
                    Value::Varchar("DOUBLE".into()),
                    Value::Varchar("DOUBLE".into()),
                    Value::Varchar("FLOAT".into()),
                    Value::Varchar("DOUBLE".into()),
                    Value::Varchar("DOUBLE".into()),
                ]]
            );
            assert_double(
                &connection.query("SELECT acos('0.5')")?.rows[0][0],
                expected[0],
            );
            for expression in [
                "acos('0.5'::VARCHAR)",
                "acos(true)",
                "signbit('1')",
                "nextafter('1','2')",
                "pi(1)",
                "atan2(1)",
                "atan2(1,2,3)",
            ] {
                assert!(
                    matches!(
                        connection.query(&format!("SELECT {expression}")),
                        Err(Error::Bind(_))
                    ),
                    "{expression}"
                );
            }

            let prepared =
                connection.prepare("SELECT atan2($1,$2),nextafter($3,$4),signbit($5),even($6)")?;
            let row = &connection
                .execute_prepared(
                    &prepared,
                    &[
                        Value::Integer(1),
                        Value::Integer(0),
                        Value::Float(1.0),
                        Value::Float(2.0),
                        Value::Double(-0.0),
                        Value::Integer(3),
                    ],
                )?
                .rows[0];
            assert_double(&row[0], std::f64::consts::FRAC_PI_2);
            assert_eq!(row[1], Value::Float(f32::from_bits(1.0_f32.to_bits() + 1)));
            assert_eq!(row[2], Value::Boolean(true));
            assert_double(&row[3], 4.0);

            connection.execute(
                "CREATE TABLE inputs(v DOUBLE);
                 INSERT INTO inputs VALUES (-2),(0),(2),(NULL)",
            )?;
            let rows = connection
                .query(
                    "SELECT v,cbrt(v*v*v),even(v+0.1),signbit(v),nextafter(v,10)
                     FROM inputs ORDER BY v NULLS LAST",
                )?
                .rows;
            assert_eq!(
                rows[0][..4],
                [
                    Value::Double(-2.0),
                    Value::Double(-2.0),
                    Value::Double(-2.0),
                    Value::Boolean(true)
                ]
            );
            assert_double(&rows[0][4], f64::from_bits((-2.0_f64).to_bits() - 1));
            assert_eq!(
                rows[1][..4],
                [
                    Value::Double(0.0),
                    Value::Double(0.0),
                    Value::Double(2.0),
                    Value::Boolean(false)
                ]
            );
            assert_double(&rows[1][4], f64::from_bits(1));
            assert_eq!(
                rows[2][..4],
                [
                    Value::Double(2.0),
                    Value::Double(2.0),
                    Value::Double(4.0),
                    Value::Boolean(false)
                ]
            );
            assert_double(&rows[2][4], f64::from_bits(2.0_f64.to_bits() + 1));
            assert_eq!(rows[3], vec![Value::Null; 5]);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ieee_domains_and_default_null_demand_match_the_development_contract() -> Result<()> {
    for evaluator in evaluators() {
        for optimizer in optimizers() {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut connection = DatabaseBuilder::new()
                .functions(test_functions(calls.clone())?)
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            connection.execute("SET ieee_floating_point_ops=true")?;
            let row = &connection
                .query(
                    "SELECT acos(2),asin(2),atanh(2),sin('inf'::DOUBLE),
                            cos('inf'::DOUBLE),tan('inf'::DOUBLE),
                            cot(0),cot('-0.0'::DOUBLE),cot('inf'::DOUBLE)",
                )?
                .rows[0];
            for value in &row[..6] {
                assert_double(value, f64::NAN);
            }
            assert_double(&row[6], f64::INFINITY);
            assert_double(&row[7], f64::NEG_INFINITY);
            assert_double(&row[8], f64::NAN);

            connection.execute("SET ieee_floating_point_ops=false")?;
            let nan_functions = connection
                .prepare("SELECT acos($1),asin($1),cos($1),cot($1),sin($1),tan($1),even($1)")?;
            for bits in [0x7ff8_0000_0000_1234, 0xfff8_0000_0000_5678] {
                let row = &connection
                    .execute_prepared(&nan_functions, &[Value::Double(f64::from_bits(bits))])?
                    .rows[0];
                for value in &row[..6] {
                    let Value::Double(value) = value else {
                        panic!("expected DOUBLE NaN, received {value:?}")
                    };
                    assert_eq!(value.to_bits(), bits);
                }
                let Value::Double(even) = row[6] else {
                    panic!("expected DOUBLE NaN, received {:?}", row[6])
                };
                assert_eq!(even.to_bits(), bits ^ (1_u64 << 63));
            }
            for expression in ["acos(2)", "asin(-2)", "atanh(2)"] {
                assert!(
                    matches!(
                        connection.query(&format!("SELECT {expression}")),
                        Err(Error::InvalidInput(_))
                    ),
                    "{expression}"
                );
            }
            for expression in [
                "sin('inf'::DOUBLE)",
                "cos('-inf'::DOUBLE)",
                "tan('inf'::DOUBLE)",
                "cot(0)",
                "cot('-0.0'::DOUBLE)",
                "cot('inf'::DOUBLE)",
            ] {
                assert!(
                    matches!(
                        connection.query(&format!("SELECT {expression}")),
                        Err(Error::OutOfRange(_))
                    ),
                    "{expression}"
                );
            }
            let row = &connection
                .query(
                    "SELECT atanh(-1),atanh(1),acosh(0),asin('nan'::DOUBLE),
                            atan2(NULL,error('rhs')),atan2(error('lhs'),NULL),
                            nextafter(NULL::DOUBLE,error('rhs')),
                            nextafter(error('lhs'),NULL::DOUBLE)",
                )?
                .rows[0];
            assert_double(&row[0], f64::NEG_INFINITY);
            assert_double(&row[1], f64::INFINITY);
            assert_double(&row[2], f64::NAN);
            assert_double(&row[3], f64::NAN);
            assert!(row[4..].iter().all(Value::is_null));

            connection.execute(
                "CREATE TABLE runtime_null(x DOUBLE);
                 INSERT INTO runtime_null VALUES (NULL)",
            )?;
            assert_eq!(
                connection
                    .query(
                        "SELECT atan2(nextval('demand'),x),
                                nextafter(x,nextval('demand'))
                         FROM runtime_null"
                    )?
                    .rows,
                vec![vec![Value::Null, Value::Null]]
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Metadata {
    arguments: Vec<DataType>,
    selected: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for Metadata {
    fn len(&self) -> usize {
        self.arguments.len()
    }

    fn data_type(&self, index: usize) -> Result<DataType> {
        self.arguments
            .get(index)
            .cloned()
            .ok_or_else(|| Error::Bind("argument outside metadata".into()))
    }

    fn select_overload(&self, _: &str, _: &[ScalarSignature]) -> Result<usize> {
        Ok(self.selected)
    }

    fn constant(&self, _: usize) -> Result<Value> {
        panic!("floating math binding must not evaluate arguments")
    }

    fn is_provably_null(&self, index: usize) -> Result<bool> {
        self.data_type(index).map(|_| false)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_adapter_validates_frontend_metadata_and_cancellation() -> Result<()> {
    let registry = FunctionRegistry::builtins();
    let nextafter = registry.scalar("nextafter")?;
    let query = QueryContext::background();
    let selected = nextafter
        .bind(
            &Metadata {
                arguments: vec![DataType::Float, DataType::Float],
                selected: 1,
            },
            &query,
        )?
        .unwrap();
    assert_eq!(
        selected.evaluate(&[Value::Float(1.0), Value::Float(2.0)], &query)?,
        Value::Float(f32::from_bits(1.0_f32.to_bits() + 1))
    );
    assert_eq!(
        selected.evaluate(&[Value::Float(0.0), Value::Float(-0.0)], &query)?,
        Value::Float(-0.0)
    );
    assert_eq!(
        selected.evaluate(&[Value::Float(0.0), Value::Float(-1.0)], &query)?,
        Value::Float(f32::from_bits((1_u32 << 31) | 1))
    );
    assert_eq!(
        selected.evaluate(&[Value::Float(f32::INFINITY), Value::Float(0.0)], &query)?,
        Value::Float(f32::from_bits(f32::INFINITY.to_bits() - 1))
    );
    assert!(matches!(
        selected.evaluate(&[Value::Double(1.0), Value::Double(2.0)], &query),
        Err(Error::Internal(_))
    ));
    assert!(matches!(
        nextafter.bind(
            &Metadata {
                arguments: vec![DataType::Double, DataType::Double],
                selected: 9,
            },
            &query
        ),
        Err(Error::Internal(_))
    ));

    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
    interrupt.interrupt();
    assert!(matches!(
        nextafter.bind(
            &Metadata {
                arguments: vec![DataType::Double, DataType::Double],
                selected: 0,
            },
            &cancelled
        ),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        selected.evaluate(&[Value::Float(1.0), Value::Float(2.0)], &cancelled),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn values_defaults_atomic_mutations_and_native_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("transcendental.duckdb");
    {
        let mut connection = Database::open(&path)?.connect();
        connection.execute(
            "CREATE TABLE results(
                 id INTEGER PRIMARY KEY,
                 angle DOUBLE DEFAULT radians(180),
                 step FLOAT DEFAULT nextafter(1::FLOAT,2::FLOAT),
                 negative BOOLEAN DEFAULT signbit('-0.0'::DOUBLE)
             );
             INSERT INTO results(id) VALUES (1),(2)",
        )?;
        connection.execute("SET ieee_floating_point_ops=false")?;
        assert!(matches!(
            connection.execute("UPDATE results SET angle=acos(id)"),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(
            connection
                .query("SELECT angle FROM results ORDER BY id")?
                .rows,
            vec![
                vec![Value::Double(std::f64::consts::PI)],
                vec![Value::Double(std::f64::consts::PI)],
            ]
        );
        connection.execute(
            "SET ieee_floating_point_ops=true;
             UPDATE results SET angle=cbrt(8)+cos(0) WHERE id=2",
        )?;
    }

    {
        let mut connection = Database::open(&path)?.connect();
        connection.execute("INSERT INTO results(id) VALUES (3); CHECKPOINT")?;
        let rows = connection.query("SELECT * FROM results ORDER BY id")?.rows;
        for id in [0, 2] {
            assert_eq!(rows[id][0], Value::Integer((id + 1) as i128));
            assert_double(&rows[id][1], std::f64::consts::PI);
            assert_eq!(
                rows[id][2],
                Value::Float(f32::from_bits(1.0_f32.to_bits() + 1))
            );
            assert_eq!(rows[id][3], Value::Boolean(true));
        }
        assert_double(&rows[1][1], 3.0);
    }

    let rows = Database::open_read_only(&path)?
        .connect()
        .query("SELECT * FROM results ORDER BY id")?
        .rows;
    assert_eq!(rows.len(), 3);
    assert_double(&rows[0][1], std::f64::consts::PI);
    assert_double(&rows[1][1], 3.0);
    assert_double(&rows[2][1], std::f64::consts::PI);
    Ok(())
}
