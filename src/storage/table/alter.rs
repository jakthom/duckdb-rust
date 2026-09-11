use super::*;
use crate::{catalog::TableAlteration, common::Value};
use std::collections::BTreeMap;

pub(crate) struct PreparedTableAlteration {
    definition: Option<TableDefinition>,
    /// One result per current physical slot, including deleted slots. Applying
    /// the same preparation to the transaction's catalog basis consumes its
    /// corresponding prefix without evaluating the expression again.
    add_values: Option<Vec<(PhysicalSlot, Value)>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    pub(super) fn alter(
        &mut self,
        name: &TableName,
        alteration: &TableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        let prepared = self.prepare_alter(name, alteration, context)?;
        self.apply_prepared_alter(name, alteration, &prepared, context)
    }

    pub(crate) fn prepare_alter(
        &self,
        name: &TableName,
        alteration: &TableAlteration,
        context: &QueryContext,
    ) -> Result<PreparedTableAlteration> {
        let context = &context.clone().with_types(self.types.clone());
        context.check()?;
        let before = self.get(name)?;
        let Some(definition) = alteration.definition(&before.definition)? else {
            return Ok(PreparedTableAlteration {
                definition: None,
                add_values: None,
            });
        };
        validate_definition(&definition, &self.types)?;
        if definition.name != *name && self.tables.contains_key(&definition.name.key()) {
            return Err(Error::Catalog(format!(
                "table {} already exists",
                definition.name
            )));
        }
        let add_values = if let TableAlteration::AddColumn { column, .. } = alteration {
            let mut values = Vec::with_capacity(before.physical_slots.len());
            for &slot in &before.physical_slots {
                context.check()?;
                let value = match &column.default {
                    Some(expression) => match expression.as_literal() {
                        Some((data_type, value)) if data_type == &column.data_type => value.clone(),
                        _ => context.stored_expressions()?.evaluate(
                            expression,
                            &column.data_type,
                            self,
                            context,
                        )?,
                    },
                    None => Value::Null,
                };
                self.types
                    .bind(&column.data_type)?
                    .validate(&value, context)?;
                if value.is_null() && !column.nullable {
                    return Err(not_null(name, &column.name));
                }
                values.push((slot, value));
            }
            Some(values)
        } else {
            None
        };
        Ok(PreparedTableAlteration {
            definition: Some(definition),
            add_values,
        })
    }

    pub(crate) fn apply_prepared_alter(
        &mut self,
        name: &TableName,
        alteration: &TableAlteration,
        prepared: &PreparedTableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        let context = &context.clone().with_types(self.types.clone());
        context.check()?;
        let before = self.get(name)?;
        let Some(definition) = &prepared.definition else {
            return Ok(false);
        };
        let mut after = before.clone();
        match alteration {
            TableAlteration::AddColumn { column, .. } => {
                let resolved = prepared.add_values.as_ref().ok_or_else(|| {
                    Error::Internal("ADD COLUMN has no prepared default values".into())
                })?;
                if resolved.len() < before.physical_slots.len() {
                    return Err(Error::Internal(
                        "ADD COLUMN preparation omits physical slots".into(),
                    ));
                }
                let mut values = BTreeMap::new();
                for (slot, (resolved_slot, value)) in before.physical_slots.iter().zip(resolved) {
                    context.check()?;
                    if slot.row_id() != resolved_slot.row_id() {
                        return Err(Error::Internal(
                            "ADD COLUMN physical slot identity changed after preparation".into(),
                        ));
                    }
                    if let Some(id) = slot.live() {
                        values.insert(id, value.clone());
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
        after.definition = definition.clone();
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
