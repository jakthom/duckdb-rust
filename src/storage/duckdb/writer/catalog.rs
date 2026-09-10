use super::{Encoder, Result, TableDefinition, constant, type_id};

pub(in crate::storage::duckdb) fn table_definition(
    output: &mut Encoder,
    table: &TableDefinition,
) -> Result<()> {
    output.property(100, 1);
    output.field(102);
    output.string(&table.name.schema)?;
    output.property(105, 0);
    output.field(200);
    output.string(&table.name.name)?;
    output.field(201);
    output.property(100, table.columns.len() as u64);
    for column in &table.columns {
        column_definition(output, column)?;
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

pub(in crate::storage::duckdb) fn column_definition(
    output: &mut Encoder,
    column: &crate::catalog::ColumnDefinition,
) -> Result<()> {
    output.field(100);
    output.string(&column.name)?;
    output.field(101);
    output.property(100, type_id(&column.data_type)?);
    output.end();
    if !column.default.is_null() {
        output.field(102);
        output.boolean(true);
        constant::write(output, &column.default, &column.data_type)?;
    }
    output.property(103, 0);
    output.property(104, 0);
    output.end();
    Ok(())
}
