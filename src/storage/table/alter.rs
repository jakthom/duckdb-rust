use super::*;
use crate::{catalog::TableAlteration, common::Value};
use std::collections::BTreeMap;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    pub(super) fn alter(
        &mut self,
        name: &TableName,
        alteration: &TableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        let context = &context.clone().with_types(self.types.clone());
        context.check()?;
        let before = self.get(name)?;
        let Some(definition) = alteration.definition(&before.definition)? else {
            return Ok(false);
        };
        validate_definition(&definition, &self.types)?;
        if definition.name != *name && self.tables.contains_key(&definition.name.key()) {
            return Err(Error::Catalog(format!(
                "table {} already exists",
                definition.name
            )));
        }
        let mut after = before.clone();
        match alteration {
            TableAlteration::AddColumn { column, .. } => {
                let mut values = BTreeMap::new();
                for slot in &before.physical_slots {
                    context.check()?;
                    let value = match &column.default {
                        Some(expression) => context.stored_expressions()?.evaluate(
                            expression,
                            &column.data_type,
                            self,
                            context,
                        )?,
                        None => Value::Null,
                    };
                    self.types
                        .bind(&column.data_type)?
                        .validate(&value, context)?;
                    if value.is_null() && !column.nullable {
                        return Err(not_null(name, &column.name));
                    }
                    if let Some(id) = slot {
                        values.insert(*id, value);
                    }
                }
                after
                    .rows
                    .add_column_values(&column.data_type, &values, context)?;
            }
            TableAlteration::DropColumn { column, .. } => {
                after
                    .rows
                    .drop_column(before.definition.column_index(column)?, context)?;
            }
            TableAlteration::SetNullability {
                column,
                nullable: false,
            } => {
                let index = before.definition.column_index(column)?;
                for row in before.rows.values() {
                    context.check()?;
                    if matches!(row[index], Value::Null) {
                        return Err(not_null(name, column));
                    }
                }
            }
            _ => {}
        }
        // These operations preserve every indexed column's ordinal and type.
        // Existing immutable indexes and unaffected vectors can be retained.
        after.definition = definition;
        context.check()?;
        self.tables.remove(&name.key());
        self.tables
            .insert(after.definition.name.key(), Arc::new(after));
        Ok(true)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn not_null(table: &TableName, column: &str) -> Error {
    Error::Constraint(format!(
        "NOT NULL constraint failed: {}.{column}",
        table.name
    ))
}
