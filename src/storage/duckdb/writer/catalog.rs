use super::{Encoder, Result, TableDefinition};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn table_definition(
    output: &mut Encoder,
    table: &TableDefinition,
    version: u64,
    context: &crate::parallel::QueryContext,
) -> Result<()> {
    output.property(100, 1);
    // Development replay/checkpoint binding navigates the schema components
    // between catalog and table. The catalog is descriptive, not an attachment
    // target; readers rebind the table in the opened database. Retain a complete
    // three-part name even though Snapshot does not own a filesystem basename.
    output.field(101);
    output.string("duckdb_rust")?;
    output.field(102);
    output.string(&table.name.schema)?;
    output.property(105, 0);
    output.field(200);
    output.string(&table.name.name)?;
    output.field(201);
    output.property(100, table.columns.len() as u64);
    for column in &table.columns {
        column_definition(output, column, version, context)?;
    }
    output.end();
    let constraints: Vec<_> = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.nullable)
        .collect();
    if !constraints.is_empty() || !table.unique_keys.is_empty() {
        output.property(202, (constraints.len() + table.unique_keys.len()) as u64);
        for (index, _) in constraints {
            output.boolean(true);
            output.property(100, 1);
            output.property(200, index as u64);
            output.end();
        }
        for key in &table.unique_keys {
            output.boolean(true);
            output.property(100, 3);
            output.field(200);
            output.boolean(key.primary);
            output.property(201, u64::MAX);
            output.property(202, key.columns.len() as u64);
            for &column in &key.columns {
                output.string(&table.columns[column].name)?;
            }
            output.end();
        }
    }
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn column_definition(
    output: &mut Encoder,
    column: &crate::catalog::ColumnDefinition,
    version: u64,
    context: &crate::parallel::QueryContext,
) -> Result<()> {
    output.field(100);
    output.string(&column.name)?;
    output.field(101);
    super::super::primitive::write_type(output, &column.data_type)?;
    if let Some(default) = &column.default {
        output.field(102);
        output.boolean(true);
        super::super::parsed::write(output, default, version, context)?;
    }
    output.property(103, 0);
    output.property(104, 0);
    output.end();
    Ok(())
}
