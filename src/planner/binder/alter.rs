use super::*;
use crate::catalog::TableAlteration;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn alter(&mut self, table: &ast::AlterTable) -> Result<BoundStatement> {
        if table.operations.len() != 1 {
            return Err(Error::Parse(
                "Only one ALTER command per statement is supported".into(),
            ));
        }
        if table.only
            || table.location.is_some()
            || table.on_cluster.is_some()
            || table.table_type.is_some()
        {
            return Err(unsupported("ALTER TABLE modifiers"));
        }
        let name = table_name(&table.name)?;
        if table.if_exists
            && !self
                .context
                .catalog
                .tables()?
                .iter()
                .any(|t| t.name == name)
        {
            return Ok(BoundStatement::Noop);
        }
        let definition = self.context.catalog.table(&name)?;
        use ast::{AlterColumnOperation as C, AlterTableOperation as A};
        let alteration = match &table.operations[0] {
            A::RenameTable {
                table_name: ast::RenameTableNameKind::To(new_name),
            } if new_name.0.len() == 1 => {
                let new_name = new_name.0[0]
                    .as_ident()
                    .ok_or_else(|| unsupported(new_name))?;
                TableAlteration::RenameTable(new_name.value.clone())
            }
            A::RenameColumn {
                old_column_name,
                new_column_name,
            } => TableAlteration::RenameColumn {
                column: old_column_name.value.clone(),
                name: new_column_name.value.clone(),
            },
            A::AddColumn {
                column_def,
                if_not_exists,
                column_position: None,
                ..
            } => {
                let mut column = ColumnDefinition::new(
                    &column_def.name.value,
                    self.data_type(&column_def.data_type)?,
                );
                for option in &column_def.options {
                    match &option.option {
                        ast::ColumnOption::Default(value) => {
                            let value = self.alter_default(value, &column.data_type)?;
                            column.default =
                                Some(crate::catalog::expression::StoredExpression::literal(
                                    column.data_type.clone(),
                                    value,
                                ));
                        }
                        ast::ColumnOption::Null => {}
                        _ => {
                            return Err(Error::Parse(
                                "Adding columns with constraints not yet supported".into(),
                            ));
                        }
                    }
                }
                TableAlteration::AddColumn {
                    column,
                    if_not_exists: *if_not_exists,
                }
            }
            A::DropColumn {
                column_names,
                if_exists,
                drop_behavior: None | Some(ast::DropBehavior::Restrict),
                ..
            } if column_names.len() == 1 => TableAlteration::DropColumn {
                column: column_names[0].value.clone(),
                if_exists: *if_exists,
            },
            A::AlterColumn { column_name, op } => {
                let column = column_name.value.clone();
                match op {
                    C::SetNotNull | C::DropNotNull => TableAlteration::SetNullability {
                        column,
                        nullable: matches!(op, C::DropNotNull),
                    },
                    C::DropDefault => TableAlteration::SetDefault {
                        column,
                        expression: None,
                    },
                    C::SetDefault { value } => {
                        let index = definition.column_index(&column)?;
                        TableAlteration::SetDefault {
                            column,
                            expression: Some(
                                crate::catalog::expression::StoredExpression::literal(
                                    definition.columns[index].data_type.clone(),
                                    self.alter_default(
                                        value,
                                        &definition.columns[index].data_type,
                                    )?,
                                ),
                            ),
                        }
                    }
                    _ => return Err(unsupported(op)),
                }
            }
            operation => return Err(unsupported(operation)),
        };
        alteration.definition(&definition)?;
        Ok(BoundStatement::AlterTable {
            table: name,
            alteration,
        })
    }

    fn alter_default(&self, expression: &ast::Expr, target: &DataType) -> Result<Value> {
        let value = self.literal(expression)?;
        self.context
            .casts
            .bind(
                &value.data_type(),
                target,
                CastMode::Assignment,
                self.context.query.types(),
            )?
            .apply(&value, self.context.query)
    }
}
