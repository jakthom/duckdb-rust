use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionEffects, FunctionRegistry, ScalarFunction},
    optimizer::IdentityOptimizer,
    parallel::QueryContext,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct RetainedSyntaxEffect(Arc<AtomicUsize>);

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
        if arguments.len() != 1 {
            return Err(Error::Bind("nextval accepts one argument".into()));
        }
        Ok(vec![DataType::Varchar])
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments != [DataType::Varchar] {
            return Err(Error::Bind("nextval requires a VARCHAR name".into()));
        }
        Ok(DataType::Integer)
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [Value::Varchar(name)] = arguments else {
            return Err(Error::Internal("bound nextval arguments".into()));
        };
        let value = self.0.fetch_add(1, Ordering::SeqCst) as i128 + 1;
        Ok(if name == "null" {
            Value::Null
        } else {
            Value::Integer(value)
        })
    }
}

#[derive(Debug)]
struct PredicateError;

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
        if arguments.len() != 1 {
            return Err(Error::Bind("error accepts one argument".into()));
        }
        Ok(vec![DataType::Varchar])
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments != [DataType::Varchar] {
            return Err(Error::Bind("error requires a VARCHAR message".into()));
        }
        Ok(DataType::Integer)
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let [Value::Varchar(message)] = arguments else {
            return Err(Error::Internal("bound error arguments".into()));
        };
        Err(Error::InvalidInput(message.clone()))
    }
}

fn predicate_functions(calls: Arc<AtomicUsize>) -> Result<FunctionRegistry> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(NextValue(calls)))?;
    functions.register_scalar(Arc::new(PredicateError))?;
    Ok(functions)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for RetainedSyntaxEffect {
    fn name(&self) -> &str {
        "retained_syntax_effect"
    }

    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: true,
        }
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if !arguments.is_empty() {
            return Err(Error::Bind(
                "retained_syntax_effect accepts no arguments".into(),
            ));
        }
        Ok(DataType::Integer)
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if !arguments.is_empty() {
            return Err(Error::Internal(
                "bound retained_syntax_effect arguments".into(),
            ));
        }
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Integer(2))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn add_column_if_not_exists_skips_invalid_default_binding() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute("CREATE TABLE t(i INTEGER)")?;
    connection
        .execute("ALTER TABLE t ADD COLUMN IF NOT EXISTS i INTEGER DEFAULT missing(); SELECT 1")?;
    let result = connection.query("SELECT * FROM t")?;
    assert_eq!(result.columns.len(), 1);
    assert!(result.rows.is_empty());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn closed_default_syntax_defers_effects_until_insert_demand() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(RetainedSyntaxEffect(calls.clone())))?;
    let mut connection = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();

    connection.execute(
        "CREATE TABLE retained(
            case_value INTEGER DEFAULT CASE WHEN true THEN retained_syntax_effect() ELSE 0 END,
            null_value BOOLEAN DEFAULT retained_syntax_effect() IS NOT NULL,
            range_value BOOLEAN DEFAULT retained_syntax_effect() BETWEEN 1 AND 3,
            in_value BOOLEAN DEFAULT retained_syntax_effect() IN (1, 2, 3),
            like_value BOOLEAN DEFAULT CAST(retained_syntax_effect() AS VARCHAR) LIKE '2'
        )",
    )?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    connection.execute("INSERT INTO retained DEFAULT VALUES")?;
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    assert_eq!(
        connection.query("SELECT * FROM retained")?.rows,
        vec![vec![
            Value::Integer(2),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
        ]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn retained_default_syntax_rejects_row_and_subquery_dependencies() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    for (index, expression) in [
        "CASE WHEN true THEN missing_column ELSE 0 END",
        "missing_column IS NULL",
        "missing_column BETWEEN 1 AND 2",
        "1 IN (2, missing_column)",
        "missing_column LIKE 'x%'",
        "CASE WHEN EXISTS (SELECT 1) THEN 1 ELSE 0 END",
    ]
    .into_iter()
    .enumerate()
    {
        let sql = format!("CREATE TABLE dependent_{index}(v INTEGER DEFAULT ({expression}))");
        assert!(
            matches!(connection.execute(&sql), Err(Error::Unsupported(message)) if message.contains("dependent stored expression")),
            "{expression}"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn predicate_demand_skips_only_branches_that_cannot_change_the_result() -> Result<()> {
    for identity_optimizer in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let builder = DatabaseBuilder::new().functions(predicate_functions(calls.clone())?);
        let builder = if identity_optimizer {
            builder.optimizer(Arc::new(IdentityOptimizer))
        } else {
            builder
        };
        let mut connection = builder.build()?.connect();

        assert_eq!(
            connection
                .query("SELECT 42 IN (nextval('s'),42,nextval('s'))")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(
            connection
                .query("SELECT 42 NOT IN (nextval('s'),42,nextval('s'))")?
                .rows,
            vec![vec![Value::Boolean(false)]]
        );
        assert_eq!(
            connection
                .query("SELECT 42 IN (42,nextval('s'),nextval('s'))")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(
            connection
                .query("SELECT 42 IN (nextval('s'),0,1,2,3,4,5,42,nextval('s'))")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(
            connection
                .query("SELECT 42 NOT IN (nextval('s'),0,1,2,3,4,5,42,nextval('s'))")?
                .rows,
            vec![vec![Value::Boolean(false)]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        assert_eq!(
            connection.query("SELECT 42 IN (error('first'),42)")?.rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(
            connection
                .query("SELECT 42 NOT IN (error('first'),42)")?
                .rows,
            vec![vec![Value::Boolean(false)]]
        );
        assert_eq!(
            connection
                .query("SELECT 42 IN (NULL,error('first'),42)")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(
            connection
                .query("SELECT 42 NOT IN (NULL,error('first'),42)")?
                .rows,
            vec![vec![Value::Boolean(false)]]
        );
        for sql in ["SELECT 41 IN (NULL,42)", "SELECT 41 NOT IN (NULL,42)"] {
            assert_eq!(
                connection.query(sql)?.rows,
                vec![vec![Value::Null]],
                "{sql}"
            );
        }
        for sql in [
            "SELECT NULL::INTEGER IN (error('rhs'))",
            "SELECT NULL::INTEGER NOT IN (error('rhs'))",
        ] {
            assert_eq!(
                connection.query(sql)?.rows,
                vec![vec![Value::Null]],
                "{sql}"
            );
        }
        for sql in [
            "SELECT nextval('null') IN (error('rhs'))",
            "SELECT nextval('null') NOT IN (error('rhs'))",
        ] {
            calls.store(0, Ordering::SeqCst);
            assert_eq!(
                connection.query(sql)?.rows,
                vec![vec![Value::Null]],
                "{sql}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{sql}");
        }
        for sql in [
            "SELECT 41 IN (error('first'),42)",
            "SELECT 41 NOT IN (error('first'),42)",
            "SELECT 41 IN (42,error('first'))",
            "SELECT 41 NOT IN (42,error('first'))",
        ] {
            assert!(
                matches!(connection.query(sql), Err(Error::InvalidInput(message)) if message == "first"),
                "{sql}"
            );
        }

        calls.store(0, Ordering::SeqCst);
        assert_eq!(
            connection.query("SELECT 1 IN (NULL,nextval('s'))")?.rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        calls.store(0, Ordering::SeqCst);
        assert_eq!(
            connection
                .query("SELECT 99 NOT IN (NULL,nextval('s'))")?
                .rows,
            vec![vec![Value::Null]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        assert_eq!(
            connection
                .query("SELECT NULL::INTEGER BETWEEN error('low') AND error('high')")?
                .rows,
            vec![vec![Value::Null]]
        );
        calls.store(0, Ordering::SeqCst);
        assert_eq!(
            connection
                .query("SELECT nextval('null') BETWEEN error('low') AND error('high')")?
                .rows,
            vec![vec![Value::Null]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            connection
                .query("SELECT 0 BETWEEN 1 AND error('high')")?
                .rows,
            vec![vec![Value::Boolean(false)]]
        );
        assert_eq!(
            connection
                .query("SELECT 0 BETWEEN error('low') AND -1")?
                .rows,
            vec![vec![Value::Boolean(false)]]
        );
        assert_eq!(
            connection
                .query("SELECT 0 BETWEEN NULL::INTEGER AND -1")?
                .rows,
            vec![vec![Value::Boolean(false)]]
        );
        assert_eq!(
            connection
                .query("SELECT 0 BETWEEN NULL::INTEGER AND 1")?
                .rows,
            vec![vec![Value::Null]]
        );
        calls.store(0, Ordering::SeqCst);
        assert_eq!(
            connection
                .query("SELECT nextval('s') BETWEEN 0 AND 100")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        calls.store(0, Ordering::SeqCst);
        assert_eq!(
            connection
                .query("SELECT 0 BETWEEN -1 AND nextval('s')")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        calls.store(0, Ordering::SeqCst);
        assert!(matches!(
            connection.query("SELECT nextval('s') BETWEEN 2 AND error('high')"),
            Err(Error::InvalidInput(message)) if message == "high"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn conjunction_comparison_and_like_preserve_pinned_demand() -> Result<()> {
    for identity_optimizer in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let builder = DatabaseBuilder::new().functions(predicate_functions(calls.clone())?);
        let builder = if identity_optimizer {
            builder.optimizer(Arc::new(IdentityOptimizer))
        } else {
            builder
        };
        let mut connection = builder.build()?.connect();

        assert_eq!(
            connection
                .query("SELECT nextval('s')<0 AND nextval('s')<100")?
                .rows,
            vec![vec![Value::Boolean(false)]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        calls.store(0, Ordering::SeqCst);
        assert_eq!(
            connection
                .query("SELECT nextval('s')>0 OR nextval('s')<100")?
                .rows,
            vec![vec![Value::Boolean(true)]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        for sql in [
            "SELECT nextval('s')<0 AND error('rhs')=0",
            "SELECT nextval('s')>0 OR error('rhs')=0",
        ] {
            calls.store(0, Ordering::SeqCst);
            assert!(
                matches!(connection.query(sql), Err(Error::InvalidInput(message)) if message == "rhs"),
                "{sql}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{sql}");
        }
        assert_eq!(
            connection.query("SELECT false AND error('rhs')=0")?.rows,
            vec![vec![Value::Boolean(false)]]
        );
        assert_eq!(
            connection.query("SELECT true OR error('rhs')=0")?.rows,
            vec![vec![Value::Boolean(true)]]
        );

        for sql in [
            "SELECT NULL::INTEGER=error('rhs')",
            "SELECT error('lhs')=NULL::INTEGER",
        ] {
            assert_eq!(
                connection.query(sql)?.rows,
                vec![vec![Value::Null]],
                "{sql}"
            );
        }
        calls.store(0, Ordering::SeqCst);
        assert!(matches!(
            connection.query("SELECT nextval('s')=error('rhs')"),
            Err(Error::InvalidInput(message)) if message == "rhs"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        calls.store(0, Ordering::SeqCst);
        assert!(matches!(
            connection.query("SELECT error('lhs')=nextval('s')"),
            Err(Error::InvalidInput(message)) if message == "lhs"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        for sql in [
            "SELECT NULL::VARCHAR LIKE CAST(error('rhs') AS VARCHAR)",
            "SELECT CAST(error('lhs') AS VARCHAR) LIKE NULL::VARCHAR",
            "SELECT NULL::VARCHAR NOT LIKE CAST(error('rhs') AS VARCHAR)",
            "SELECT CAST(error('lhs') AS VARCHAR) NOT LIKE NULL::VARCHAR",
        ] {
            assert_eq!(
                connection.query(sql)?.rows,
                vec![vec![Value::Null]],
                "{sql}"
            );
        }
        calls.store(0, Ordering::SeqCst);
        assert!(matches!(
            connection.query(
                "SELECT CAST(nextval('s') AS VARCHAR) LIKE CAST(error('rhs') AS VARCHAR)"
            ),
            Err(Error::InvalidInput(message)) if message == "rhs"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        calls.store(0, Ordering::SeqCst);
        assert!(matches!(
            connection.query(
                "SELECT CAST(error('lhs') AS VARCHAR) LIKE CAST(nextval('s') AS VARCHAR)"
            ),
            Err(Error::InvalidInput(message)) if message == "lhs"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn predicate_selection_short_circuits_runtime_conjunctions() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut connection = DatabaseBuilder::new()
            .functions(predicate_functions(calls.clone())?)
            .optimizer(Arc::new(IdentityOptimizer))
            .expressions(expressions)
            .build()?
            .connect();
        connection.execute("CREATE TABLE selected(i INTEGER); INSERT INTO selected VALUES (1)")?;

        assert!(
            connection
                .query("SELECT i FROM selected WHERE nextval('s')<0 AND error('rhs')=0")?
                .rows
                .is_empty()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        calls.store(0, Ordering::SeqCst);
        assert_eq!(
            connection
                .query("SELECT i FROM selected WHERE nextval('s')>0 OR error('rhs')=0")?
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        calls.store(0, Ordering::SeqCst);
        let result = connection
            .execute("UPDATE selected SET i=2 WHERE nextval('s')<0 AND error('update')=0")?;
        assert_eq!(result[0].affected_rows, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            connection.query("SELECT i FROM selected")?.rows,
            vec![vec![Value::Integer(1)]]
        );

        calls.store(0, Ordering::SeqCst);
        let result =
            connection.execute("DELETE FROM selected WHERE nextval('s')>0 OR error('delete')=0")?;
        assert_eq!(result[0].affected_rows, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        calls.store(0, Ordering::SeqCst);
        assert!(
            connection
                .query(
                    "SELECT * FROM range(1)a(i) JOIN range(1)b(j) \
                     ON nextval('s')<0 AND error('join')=0"
                )?
                .rows
                .is_empty()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        calls.store(0, Ordering::SeqCst);
        assert!(matches!(
            connection.query(
                "SELECT sum(i) FILTER (WHERE nextval('s')<0 AND error('aggregate')=0) \
                 FROM range(1)t(i)"
            ),
            Err(Error::InvalidInput(message)) if message == "aggregate"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn retained_predicate_defaults_share_ordinary_demand() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut connection = DatabaseBuilder::new()
        .functions(predicate_functions(calls.clone())?)
        .optimizer(Arc::new(IdentityOptimizer))
        .build()?
        .connect();
    connection.execute(
        "CREATE TABLE demand(
            in_effect BOOLEAN DEFAULT 42 IN (nextval('s'),42,nextval('s')),
            not_in_effect BOOLEAN DEFAULT 42 NOT IN (nextval('s'),42,nextval('s')),
            in_error BOOLEAN DEFAULT 42 IN (error('first'),42),
            not_in_error BOOLEAN DEFAULT 42 NOT IN (error('first'),42),
            in_null BOOLEAN DEFAULT 41 IN (NULL,42),
            not_in_null BOOLEAN DEFAULT 41 NOT IN (NULL,42),
            in_null_needle BOOLEAN DEFAULT NULL::INTEGER IN (error('rhs')),
            not_in_null_needle BOOLEAN DEFAULT NULL::INTEGER NOT IN (error('rhs')),
            in_volatile_null BOOLEAN DEFAULT nextval('null') IN (error('rhs')),
            not_in_volatile_null BOOLEAN DEFAULT nextval('null') NOT IN (error('rhs')),
            comparison_left_null BOOLEAN DEFAULT NULL::INTEGER=error('rhs'),
            comparison_right_null BOOLEAN DEFAULT error('lhs')=NULL::INTEGER,
            like_left_null BOOLEAN DEFAULT NULL::VARCHAR LIKE CAST(error('rhs') AS VARCHAR),
            like_right_null BOOLEAN DEFAULT CAST(error('lhs') AS VARCHAR) LIKE NULL::VARCHAR,
            not_like_left_null BOOLEAN DEFAULT NULL::VARCHAR NOT LIKE CAST(error('rhs') AS VARCHAR),
            not_like_right_null BOOLEAN DEFAULT CAST(error('lhs') AS VARCHAR) NOT LIKE NULL::VARCHAR,
            constant_and BOOLEAN DEFAULT false AND error('rhs')=0,
            constant_or BOOLEAN DEFAULT true OR error('rhs')=0,
            between_null BOOLEAN DEFAULT NULL::INTEGER BETWEEN error('low') AND error('high'),
            between_false BOOLEAN DEFAULT 0 BETWEEN 1 AND error('high'),
            between_volatile_null BOOLEAN DEFAULT nextval('null') BETWEEN error('low') AND error('high'),
            between_once BOOLEAN DEFAULT nextval('s') BETWEEN 0 AND 100,
            runtime_and BOOLEAN DEFAULT nextval('s')<0 AND nextval('s')<100,
            runtime_or BOOLEAN DEFAULT nextval('s')>0 OR nextval('s')<100
        )",
    )?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    connection.execute("INSERT INTO demand DEFAULT VALUES")?;
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    assert_eq!(
        connection.query("SELECT * FROM demand")?.rows,
        vec![vec![
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Null,
            Value::Boolean(false),
            Value::Null,
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
        ]]
    );

    calls.store(0, Ordering::SeqCst);
    connection.execute(
        "CREATE TABLE eager_bound(
            value BOOLEAN DEFAULT nextval('s') BETWEEN 2 AND error('high')
        )",
    )?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        connection.execute("INSERT INTO eager_bound DEFAULT VALUES"),
        Err(Error::InvalidInput(message)) if message == "high"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        connection
            .query("SELECT * FROM eager_bound")?
            .rows
            .is_empty()
    );
    Ok(())
}
