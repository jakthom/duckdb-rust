//! Owned table mutations shared by binding, transaction adapters and recovery.
use super::{ColumnDefinition, TableDefinition, TableName};
use crate::common::{Error, Result, Value};

#[derive(Clone, Debug)]
pub enum TableAlteration {
    RenameTable(String),
    RenameColumn {
        column: String,
        name: String,
    },
    AddColumn {
        column: ColumnDefinition,
        if_not_exists: bool,
    },
    DropColumn {
        column: String,
        if_exists: bool,
    },
    SetDefault {
        column: String,
        value: Value,
    },
    SetNullability {
        column: String,
        nullable: bool,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableDefinition {
    pub fn column_index(&self, name: &str) -> Result<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                Error::Bind(format!(
                    "Table \"{}\" does not have a column with name \"{name}\"",
                    self.name.name
                ))
            })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableAlteration {
    /// Derive metadata without effects. The catalog must additionally validate
    /// name collisions, registered types/defaults and all affected stored values.
    /// None is an explicit conditional no-op, never an unsupported operation.
    pub fn definition(&self, before: &TableDefinition) -> Result<Option<TableDefinition>> {
        let mut after = before.clone();
        match self {
            Self::RenameTable(name) => {
                if name.is_empty() {
                    return Err(Error::Catalog("empty table name".into()));
                }
                after.name = TableName::new(&before.name.schema, name);
            }
            Self::RenameColumn { column, name } => {
                let index = before.column_index(column)?;
                if name.is_empty() {
                    return Err(Error::Catalog("empty column name".into()));
                }
                if before
                    .columns
                    .iter()
                    .enumerate()
                    .any(|(i, c)| i != index && c.name.eq_ignore_ascii_case(name))
                {
                    return Err(Error::Catalog(format!(
                        "Column with name {name} already exists!"
                    )));
                }
                after.columns[index].name = name.clone();
            }
            Self::AddColumn {
                column,
                if_not_exists,
            } => {
                if !column.nullable {
                    return Err(Error::Unsupported("adding columns with constraints".into()));
                }
                if before.column_index(&column.name).is_ok() {
                    return if *if_not_exists {
                        Ok(None)
                    } else {
                        Err(Error::Catalog(format!(
                            "Column with name {} already exists!",
                            column.name
                        )))
                    };
                }
                if column.name.is_empty() {
                    return Err(Error::Catalog("empty column name".into()));
                }
                after.columns.push(column.clone());
            }
            Self::DropColumn { column, if_exists } => {
                let index = match before.column_index(column) {
                    Ok(index) => index,
                    Err(_) if *if_exists => return Ok(None),
                    Err(error) => return Err(error),
                };
                if before.columns.len() == 1 {
                    return Err(Error::Catalog(
                        "Cannot drop column: table only has one column remaining!".into(),
                    ));
                }
                if before
                    .unique_keys
                    .iter()
                    .any(|key| key.columns.contains(&index))
                {
                    return Err(Error::Catalog(
                        "Cannot drop this column: an index depends on it!".into(),
                    ));
                }
                if before
                    .unique_keys
                    .iter()
                    .any(|key| key.columns.iter().any(|&i| i > index))
                {
                    return Err(Error::Catalog(
                        "Cannot drop this column: an index depends on a column after it!".into(),
                    ));
                }
                after.columns.remove(index);
            }
            Self::SetDefault { column, value } => {
                after.columns[before.column_index(column)?].default = value.clone();
            }
            Self::SetNullability { column, nullable } => {
                let index = before.column_index(column)?;
                if *nullable
                    && before
                        .unique_keys
                        .iter()
                        .any(|key| key.primary && key.columns.contains(&index))
                {
                    return Err(Error::Catalog(format!(
                        "Cannot drop NOT NULL constraint: column \"{column}\" is in a primary key"
                    )));
                }
                after.columns[index].nullable = *nullable;
            }
        }
        Ok(Some(after))
    }
}
