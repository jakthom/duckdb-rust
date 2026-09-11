use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionRegistry, ScalarBindArguments, ScalarSignature},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::{InterruptHandle, QueryContext},
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

struct BindMetadata {
    types: Vec<DataType>,
    selected: usize,
    known_null: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for BindMetadata {
    fn len(&self) -> usize {
        self.types.len()
    }

    fn data_type(&self, index: usize) -> Result<DataType> {
        self.types
            .get(index)
            .cloned()
            .ok_or_else(|| Error::Bind("argument outside metadata".into()))
    }

    fn constant(&self, _: usize) -> Result<Value> {
        panic!("selected math binding must not evaluate arguments")
    }

    fn select_overload(&self, _: &str, _: &[ScalarSignature]) -> Result<usize> {
        Ok(self.selected)
    }

    fn is_provably_null(&self, index: usize) -> Result<bool> {
        self.data_type(index).map(|_| self.known_null)
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn assert_close(value: &Value, expected: f64) {
    let Value::Double(actual) = value else {
        panic!("expected DOUBLE, received {value:?}")
    };
    if expected.is_nan() {
        assert!(actual.is_nan(), "expected NaN, received {actual}");
    } else if expected.is_infinite() || expected == 0.0 {
        assert_eq!(actual.to_bits(), expected.to_bits());
    } else {
        let relative = ((actual - expected) / expected).abs();
        assert!(
            relative <= 8.0 * f64::EPSILON,
            "expected {expected}, received {actual} (relative error {relative})"
        );
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn binom_selected_adapter_validates_metadata_null_invocation_and_cancellation() -> Result<()> {
    let registry = FunctionRegistry::builtins();
    let root = registry.scalar("binom")?;
    let query = QueryContext::background();
    let metadata = BindMetadata {
        types: vec![DataType::Integer, DataType::Integer],
        selected: 0,
        known_null: false,
    };
    let bound = root.bind(&metadata, &query)?.unwrap();
    assert_eq!(
        bound.evaluate(&[Value::Integer(5), Value::Integer(2)], &query)?,
        Value::Integer(10)
    );
    assert!(matches!(
        root.bind(
            &BindMetadata {
                types: metadata.types.clone(),
                selected: 1,
                known_null: false,
            },
            &query
        ),
        Err(Error::Internal(_))
    ));
    let known_null = root
        .bind(
            &BindMetadata {
                types: metadata.types,
                selected: 0,
                known_null: true,
            },
            &query,
        )?
        .unwrap();
    assert_eq!(known_null.evaluate(&[], &query)?, Value::Null);
    assert!(matches!(
        known_null.evaluate(&[Value::Null, Value::Null], &query),
        Err(Error::Internal(_))
    ));
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 1, 1)?;
    interrupt.interrupt();
    assert!(matches!(
        bound.evaluate(&[Value::Integer(130), Value::Integer(65)], &cancelled),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn generated_math_tail_matches_values_types_casts_and_boundaries() -> Result<()> {
    for evaluator in evaluators() {
        for optimizer in optimizers() {
            let mut connection = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            connection.execute("SET ieee_floating_point_ops=true")?;
            let result = connection.query(
                "SELECT gamma(1),gamma(2),gamma(10),gamma(171),gamma(172),
                        gamma('inf'::DOUBLE),gamma('-inf'::DOUBLE),gamma('nan'::DOUBLE),
                        lgamma(1),lgamma(3),lgamma('-0.1'::DOUBLE),
                        lgamma('inf'::DOUBLE),lgamma('-inf'::DOUBLE),lgamma('nan'::DOUBLE),
                        typeof(gamma(1)),typeof(lgamma(1))",
            )?;
            let row = &result.rows[0];
            assert_close(&row[0], 1.0);
            assert_close(&row[1], 1.0);
            assert_close(&row[2], 362_880.0);
            assert_close(&row[3], 7.257_415_615_307_999e306);
            assert_close(&row[4], f64::INFINITY);
            assert_close(&row[5], f64::INFINITY);
            assert_close(&row[6], f64::NAN);
            assert_close(&row[7], f64::NAN);
            assert_close(&row[8], 0.0);
            assert_close(&row[9], 2.0_f64.ln());
            assert_close(&row[10], 2.368_961_332_728_789_5);
            assert_close(&row[11], f64::INFINITY);
            assert_close(&row[12], f64::INFINITY);
            assert_close(&row[13], f64::NAN);
            assert_eq!(row[14], Value::Varchar("DOUBLE".into()));
            assert_eq!(row[15], Value::Varchar("DOUBLE".into()));

            let result = connection.query(
                "SELECT binom(0,0),binom(5,2),binom(10,5),binom(2,5),binom(60,30),
                        binom(120,60),binom(130,65),binom(2147483647,0),
                        binom(2147483647,1),binom(2147483647,2147483647),
                        typeof(binom(NULL,NULL)),binom('5','2')",
            )?;
            assert_eq!(
                result.rows,
                vec![vec![
                    Value::Integer(1),
                    Value::Integer(10),
                    Value::Integer(252),
                    Value::Integer(0),
                    Value::Integer(118_264_581_564_861_424),
                    Value::Integer(96_614_908_840_363_322_603_893_139_521_372_656),
                    Value::Integer(95_067_625_827_960_698_145_584_333_020_095_113_100),
                    Value::Integer(1),
                    Value::Integer(2_147_483_647),
                    Value::Integer(1),
                    Value::Varchar("HUGEINT".into()),
                    Value::Integer(10),
                ]]
            );
            for expression in ["binom(-1,2)", "binom(2,-1)", "binom(-1,5)"] {
                assert!(
                    matches!(connection.query(&format!("SELECT {expression}")), Err(Error::OutOfRange(message)) if message.contains("negative input")),
                    "{expression}"
                );
            }
            assert!(matches!(
                connection.query("SELECT binom(131,65)"),
                Err(Error::OutOfRange(message)) if message.contains("Value out of range")
            ));
            for expression in [
                "binom(5::BIGINT,2)",
                "binom(5::HUGEINT,2)",
                "binom(5::UINTEGER,2)",
                "binom(5::DOUBLE,2)",
                "binom(5::DECIMAL(2,0),2)",
                "binom('5'::VARCHAR,2)",
                "binom(true,2)",
            ] {
                assert!(
                    matches!(
                        connection.query(&format!("SELECT {expression}")),
                        Err(Error::Bind(_))
                    ),
                    "{expression}"
                );
            }

            let result = connection.query(
                "SELECT isnan('nan'::FLOAT),isnan('-nan'::FLOAT),
                        isnan('nan'::DOUBLE),isnan('-nan'::DOUBLE),
                        isnan('inf'::FLOAT),isnan('-inf'::DOUBLE),isnan(1),
                        isnan(NULL::FLOAT),typeof(isnan(1::FLOAT)),typeof(isnan(1::DOUBLE))",
            )?;
            assert_eq!(
                result.rows,
                vec![vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Boolean(false),
                    Value::Boolean(false),
                    Value::Null,
                    Value::Varchar("BOOLEAN".into()),
                    Value::Varchar("BOOLEAN".into()),
                ]]
            );
            for expression in [
                "isnan('nan')",
                "isnan('nan'::VARCHAR)",
                "isnan(true)",
                "isnan()",
                "isnan(1,2)",
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
                connection.prepare("SELECT gamma($1),lgamma($1),binom($2,$3),isnan($4)")?;
            assert_eq!(
                connection
                    .execute_prepared(
                        &prepared,
                        &[
                            Value::Integer(2),
                            Value::Integer(5),
                            Value::Integer(2),
                            Value::Double(f64::NAN),
                        ],
                    )?
                    .rows,
                vec![vec![
                    Value::Double(1.0),
                    Value::Double(0.0),
                    Value::Integer(10),
                    Value::Boolean(true),
                ]]
            );
            let isnan = connection.prepare("SELECT isnan($1)")?;
            for value in [
                Value::Float(f32::from_bits(0xffc0_1234)),
                Value::Double(f64::from_bits(0xfff8_0000_0000_5678)),
            ] {
                assert_eq!(
                    connection.execute_prepared(&isnan, &[value])?.rows,
                    vec![vec![Value::Boolean(true)]]
                );
            }

            connection.execute(
                "CREATE TABLE inputs(n INTEGER,k INTEGER,x DOUBLE);
                 INSERT INTO inputs VALUES (5,2,1),(10,5,'nan'::DOUBLE),(NULL,2,NULL)",
            )?;
            assert_eq!(
                connection
                    .query(
                        "SELECT binom(n,k),isnan(x),gamma(n) FROM inputs ORDER BY k,n NULLS LAST"
                    )?
                    .rows,
                vec![
                    vec![
                        Value::Integer(10),
                        Value::Boolean(false),
                        Value::Double(24.0)
                    ],
                    vec![Value::Null, Value::Null, Value::Null],
                    vec![
                        Value::Integer(252),
                        Value::Boolean(true),
                        Value::Double(362_880.0)
                    ],
                ]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn generated_math_tail_preserves_ieee_and_default_null_demand() -> Result<()> {
    for evaluator in evaluators() {
        for optimizer in optimizers() {
            let mut connection = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            connection.execute("SET ieee_floating_point_ops=false")?;
            let prepared = connection.prepare("SELECT gamma($1),lgamma($1)")?;
            assert!(matches!(
                connection.execute_prepared(&prepared, &[Value::Double(0.0)]),
                Err(Error::OutOfRange(_))
            ));
            let strict = &connection
                .query(
                    "SELECT gamma(-1),gamma('-0.1'::DOUBLE),gamma(172),
                            gamma('inf'::DOUBLE),gamma('-inf'::DOUBLE),gamma('nan'::DOUBLE),
                            lgamma(-1),lgamma('-0.1'::DOUBLE),lgamma('-inf'::DOUBLE),
                            lgamma('nan'::DOUBLE)",
                )?
                .rows[0];
            assert_close(&strict[0], f64::NAN);
            assert_close(&strict[1], -10.686_287_021_193_193);
            assert_close(&strict[2], f64::INFINITY);
            assert_close(&strict[3], f64::INFINITY);
            assert_close(&strict[4], f64::NAN);
            assert_close(&strict[5], f64::NAN);
            assert_close(&strict[6], f64::INFINITY);
            assert_close(&strict[7], 2.368_961_332_728_789_5);
            assert_close(&strict[8], f64::INFINITY);
            assert_close(&strict[9], f64::NAN);
            for expression in ["gamma(0.0)", "gamma('-0.0'::DOUBLE)"] {
                assert!(
                    matches!(connection.query(&format!("SELECT {expression}")), Err(Error::OutOfRange(message)) if message == "cannot take gamma of zero"),
                    "{expression}"
                );
            }
            for expression in ["lgamma(0.0)", "lgamma('-0.0'::DOUBLE)"] {
                assert!(
                    matches!(connection.query(&format!("SELECT {expression}")), Err(Error::OutOfRange(message)) if message == "cannot take log gamma of zero"),
                    "{expression}"
                );
            }
            connection.execute("SET ieee_floating_point_ops=true")?;
            let row = &connection
                .query(
                    "SELECT gamma(0.0),gamma('-0.0'::DOUBLE),lgamma(0.0),lgamma('-0.0'::DOUBLE)",
                )?
                .rows[0];
            assert_close(&row[0], f64::INFINITY);
            assert_close(&row[1], f64::NEG_INFINITY);
            assert_close(&row[2], f64::INFINITY);
            assert_close(&row[3], f64::INFINITY);
            let rebound = &connection
                .execute_prepared(&prepared, &[Value::Double(-0.0)])?
                .rows[0];
            assert_close(&rebound[0], f64::NEG_INFINITY);
            assert_close(&rebound[1], f64::INFINITY);
            connection.execute("SET ieee_floating_point_ops=NULL")?;
            assert_close(
                &connection.query("SELECT gamma(0)")?.rows[0][0],
                f64::INFINITY,
            );

            assert_eq!(
                connection
                    .query(
                        "SELECT binom(NULL::INTEGER,CAST('rhs' AS INTEGER)),
                                binom(CAST('lhs' AS INTEGER),NULL::INTEGER),
                                gcd(NULL::BIGINT,CAST('rhs' AS BIGINT)),
                                lcm(CAST('lhs' AS BIGINT),NULL::BIGINT),
                                factorial(NULL::INTEGER)"
                    )?
                    .rows,
                vec![vec![Value::Null; 5]]
            );
            connection.execute(
                "CREATE TABLE runtime_null(i INTEGER); INSERT INTO runtime_null VALUES(NULL)",
            )?;
            for expression in [
                "binom(i,CAST('rhs' AS INTEGER))",
                "binom(CAST('lhs' AS INTEGER),i)",
                "gcd(i,CAST('rhs' AS BIGINT))",
                "lcm(CAST('lhs' AS BIGINT),i)",
            ] {
                assert!(
                    matches!(connection.query(&format!("SELECT {expression} FROM runtime_null")), Err(Error::Conversion(message)) if message.contains("lhs") || message.contains("rhs")),
                    "{expression}"
                );
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn generated_math_catalog_aliases_and_retained_defaults_are_lazy_and_atomic() -> Result<()> {
    let registry = FunctionRegistry::builtins();
    for name in ["**", "^", "!__postfix", "@"] {
        assert_eq!(registry.scalar(name)?.name(), name);
    }

    for evaluator in evaluators() {
        for optimizer in optimizers() {
            let mut connection = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            connection.execute(
                "SET ieee_floating_point_ops=false;
                 CREATE TABLE retained(
                   id INTEGER,
                   g DOUBLE DEFAULT gamma(0),
                   b HUGEINT DEFAULT binom(5,2),
                   n BOOLEAN DEFAULT isnan('nan'::DOUBLE),
                   p DOUBLE DEFAULT \"**\"(2,3),
                   q DOUBLE DEFAULT \"^\"(3,2),
                   f HUGEINT DEFAULT \"!__postfix\"(4),
                   a INTEGER DEFAULT \"@\"(-2));
                 INSERT INTO retained VALUES(1,7,11,false,12,13,14,15)",
            )?;
            assert!(matches!(
                connection.execute("INSERT INTO retained(id) VALUES(2)"),
                Err(Error::OutOfRange(message)) if message.contains("gamma of zero")
            ));
            assert_eq!(
                connection.query("SELECT count(*) FROM retained")?.rows,
                vec![vec![Value::Integer(1)]]
            );
            connection
                .execute("SET ieee_floating_point_ops=true; INSERT INTO retained(id) VALUES(2)")?;
            let row = &connection
                .query("SELECT g,b,n,p,q,f,a FROM retained WHERE id=2")?
                .rows[0];
            assert_close(&row[0], f64::INFINITY);
            assert_eq!(
                row[1..],
                [
                    Value::Integer(10),
                    Value::Boolean(true),
                    Value::Double(8.0),
                    Value::Double(9.0),
                    Value::Integer(24),
                    Value::Integer(2),
                ]
            );

            connection.execute(
                "CREATE TABLE invalid_default(id INTEGER,b HUGEINT DEFAULT binom(-1,2));
                 INSERT INTO invalid_default VALUES(1,9)",
            )?;
            assert!(matches!(
                connection.execute("INSERT INTO invalid_default(id) VALUES(2)"),
                Err(Error::OutOfRange(message)) if message.contains("negative input")
            ));
            assert_eq!(
                connection.query("SELECT * FROM invalid_default")?.rows,
                vec![vec![Value::Integer(1), Value::Integer(9)]]
            );
            connection.execute(
                "CREATE TABLE altered(id INTEGER); INSERT INTO altered VALUES(1);
                 ALTER TABLE altered ADD COLUMN b HUGEINT DEFAULT binom(6,2)",
            )?;
            assert_eq!(
                connection.query("SELECT * FROM altered")?.rows,
                vec![vec![Value::Integer(1), Value::Integer(15)]]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn populate_persistent_math(connection: &mut duckdb_rust::main::Connection) -> Result<()> {
    connection.execute(
        "SET ieee_floating_point_ops=true;
         CREATE TABLE special(
           id INTEGER PRIMARY KEY,
           g DOUBLE DEFAULT gamma(5),
           l DOUBLE DEFAULT lgamma(3),
           b HUGEINT DEFAULT binom(10,5),
           n BOOLEAN DEFAULT isnan('nan'::DOUBLE),
           p DOUBLE DEFAULT \"**\"(2,3),
           q DOUBLE DEFAULT \"^\"(3,2),
           f HUGEINT DEFAULT \"!__postfix\"(4),
           a INTEGER DEFAULT \"@\"(-2));
         INSERT INTO special(id) VALUES(1);
         INSERT INTO special VALUES(2,gamma(6),lgamma(4),binom(8,4),isnan(1::FLOAT),10,11,12,13);
         BEGIN; UPDATE special SET b=binom(130,65); ROLLBACK;
         UPDATE special SET g=gamma(4),l=lgamma(5),b=binom(7,3),n=isnan('nan'::FLOAT) WHERE id=2",
    )?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn verify_persistent_math(connection: &mut duckdb_rust::main::Connection) -> Result<()> {
    let rows = connection
        .query("SELECT id,g,l,b,n,p,q,f,a FROM special ORDER BY id")?
        .rows;
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_close(&rows[0][1], 24.0);
    assert_close(&rows[0][2], 2.0_f64.ln());
    assert_eq!(rows[0][3], Value::Integer(252));
    assert_eq!(rows[0][4], Value::Boolean(true));
    assert_eq!(
        rows[0][5..],
        [
            Value::Double(8.0),
            Value::Double(9.0),
            Value::Integer(24),
            Value::Integer(2),
        ]
    );
    assert_eq!(rows[1][0], Value::Integer(2));
    assert_close(&rows[1][1], 6.0);
    assert_close(&rows[1][2], 24.0_f64.ln());
    assert_eq!(rows[1][3], Value::Integer(35));
    assert_eq!(rows[1][4], Value::Boolean(true));
    assert_eq!(
        rows[1][5..],
        [
            Value::Double(10.0),
            Value::Double(11.0),
            Value::Integer(12),
            Value::Integer(13),
        ]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn generated_math_tail_survives_private_native_and_wal_reopen() -> Result<()> {
    for format in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("special.snapshot");
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                format.clone(),
            )?))
            .build()?;
        populate_persistent_math(&mut database.connect())?;
        drop(database);
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                format,
            )?))
            .build()?;
        verify_persistent_math(&mut database.connect())?;
    }

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("special.duckdb");
    {
        let database = Database::open(&path)?;
        populate_persistent_math(&mut database.connect())?;
        database.connect().execute("CHECKPOINT")?;
    }
    verify_persistent_math(&mut Database::open(&path)?.connect())?;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("special-wal.duckdb");
    {
        populate_persistent_math(&mut Database::open_logged(&path)?.connect())?;
    }
    verify_persistent_math(&mut Database::open_logged(&path)?.connect())?;
    Ok(())
}
