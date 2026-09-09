use duckdb_rust::{
    DatabaseBuilder, Result, Value,
    common::{DataType, type_registry::TypeRegistry},
    function::{FunctionRegistry, ScalarFunction},
    parallel::QueryContext,
};
use std::sync::Arc;

#[derive(Debug)]
struct OwnedFunction;
impl ScalarFunction for OwnedFunction {
    fn name(&self) -> &str {
        "owned_value"
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Integer)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Ok(Value::Integer(7))
    }
}

#[test]
fn repeated_error_and_result_lifetimes_release_registered_adapters() -> Result<()> {
    for _ in 0..64 {
        let function = Arc::new(OwnedFunction);
        let weak = Arc::downgrade(&function);
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(function)?;
        let database = DatabaseBuilder::new().functions(functions).build()?;
        let mut connection = database.connect();
        let statement = connection.prepare("SELECT owned_value()")?;
        let result = connection.execute_prepared(&statement, &[])?;
        assert!(connection.query("SELECT CAST('bad' AS INTEGER)").is_err());
        drop(connection);
        drop(statement);
        drop(database);
        assert!(
            weak.upgrade().is_none(),
            "adapter retained after owners were dropped"
        );
        assert_eq!(result.rows, vec![vec![Value::Integer(7)]]);
    }
    Ok(())
}
