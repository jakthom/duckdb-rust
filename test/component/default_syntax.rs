use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    function::{FunctionEffects, FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct RetainedSyntaxEffect(Arc<AtomicUsize>);

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
    assert_eq!(calls.load(Ordering::SeqCst), 6);
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
