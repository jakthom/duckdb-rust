//! Replace an operator without changing its SQL callers.
use duckdb_rust::{
    DataType, DatabaseBuilder, Result, Value,
    common::type_registry::builtin_types,
    function::operator::{DynamicLike, Operator, OperatorRegistry},
};
use std::sync::Arc;

fn main() -> Result<()> {
    let mut operators = OperatorRegistry::builtins();
    let selected = operators.bind(
        Operator::Like,
        &[DataType::Varchar, DataType::Varchar],
        &builtin_types(),
    )?;
    operators.replace(selected.signature().clone(), Arc::new(DynamicLike))?;
    let database = DatabaseBuilder::new().operators(operators).build()?;
    let result = database.connect().query(
        "SELECT 'duck🦆' LIKE 'd%_' AS matched, (DATE '2000-03-01'-1)::VARCHAR AS leap_day",
    )?;
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Boolean(true),
            Value::Varchar("2000-02-29".into())
        ]]
    );
    Ok(())
}
