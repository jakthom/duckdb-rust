use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Result, Value,
    execution::index::BTreeIndexFactory,
    optimizer::{PipelineOptimizer, UseKeyLookup},
};

fn main() -> Result<()> {
    let database = DatabaseBuilder::new()
        .indexes(Arc::new(BTreeIndexFactory))
        .optimizer(Arc::new(PipelineOptimizer::new(vec![Arc::new(
            UseKeyLookup,
        )])))
        .batch_size(64)
        .max_intermediate_rows(100_000)
        .build()?;
    let mut connection = database.connect();
    connection.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, name VARCHAR)")?;
    let insert = connection.prepare("INSERT INTO items VALUES ($1,$2)")?;
    connection.execute_prepared(&insert, &[Value::Integer(1), Value::Varchar("duck".into())])?;
    println!(
        "{:?}",
        connection.query("SELECT * FROM items WHERE id=1")?.rows
    );
    println!("{:?}", database.adapters());
    Ok(())
}
