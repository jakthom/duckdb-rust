use duckdb_rust::{
    DataType, DatabaseBuilder, Result, Value,
    common::{
        cast::{CastMode, CastRegistry, CastSpec},
        type_registry::{
            TypeRegistry,
            ascii::{self, AsciiCast, StreamingAscii},
        },
    },
};
use std::sync::Arc;

fn main() -> Result<()> {
    let mut types = TypeRegistry::builtins();
    types.register(ascii::FAMILY, Arc::new(StreamingAscii))?;
    let text_type = ascii::data_type(64)?;
    let mut casts = CastRegistry::builtins();
    casts.register_type(&text_type, &types)?;
    for mode in [CastMode::Assignment, CastMode::Explicit] {
        for (source, target) in [
            (DataType::Varchar, text_type.clone()),
            (text_type.clone(), DataType::Varchar),
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
    let database = DatabaseBuilder::new()
        .types(Arc::new(types))
        .casts(casts)
        .build()?;
    let mut connection = database.connect();
    connection.execute(
        "CREATE TABLE names(name ascii_ci(64) PRIMARY KEY); INSERT INTO names VALUES ('Alice')",
    )?;
    let result = connection
        .query("SELECT name::VARCHAR FROM names WHERE name=CAST('ALICE' AS ascii_ci(64))")?;
    assert_eq!(result.rows, vec![vec![Value::Varchar("Alice".into())]]);
    Ok(())
}
