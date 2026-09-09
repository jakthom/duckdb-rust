use serde::{Deserialize, Serialize};

use crate::common::{DataType, Result, Value};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TableName {
    pub schema: String,
    pub name: String,
}

impl TableName {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into().to_ascii_lowercase(),
            name: name.into().to_ascii_lowercase(),
        }
    }
    pub fn main(name: impl Into<String>) -> Self {
        Self::new("main", name)
    }
    pub(crate) fn key(&self) -> String {
        format!("{}:{}{}", self.schema.len(), self.schema, self.name)
    }
}

impl std::fmt::Display for TableName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.schema, self.name)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnDefinition {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub default: Value,
}

impl ColumnDefinition {
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable: true,
            default: Value::Null,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UniqueKey {
    pub columns: Vec<usize>,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableDefinition {
    pub name: TableName,
    pub columns: Vec<ColumnDefinition>,
    /// Each key is a group of column ordinals. NULL keys do not conflict.
    pub unique_keys: Vec<UniqueKey>,
}

/// All metadata is resolved in the caller's transaction snapshot.
pub trait Catalog: Send {
    fn schemas(&self) -> Result<Vec<String>>;
    fn table(&self, name: &TableName) -> Result<TableDefinition>;
    fn tables(&self) -> Result<Vec<TableDefinition>>;
}

pub trait CatalogMut: Catalog {
    fn create_schema(&mut self, name: &str, if_not_exists: bool) -> Result<()>;
    fn drop_schema(&mut self, name: &str, if_exists: bool) -> Result<()>;
    fn create_table(&mut self, definition: TableDefinition, if_not_exists: bool) -> Result<()>;
    fn drop_table(&mut self, name: &TableName, if_exists: bool) -> Result<()>;
}
