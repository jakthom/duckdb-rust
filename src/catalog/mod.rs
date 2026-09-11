use serde::{Deserialize, Serialize};

use crate::common::{DataType, Result, Value};
use expression::StoredExpression;

mod alter;
pub mod expression;
pub use alter::TableAlteration;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TableName {
    pub schema: String,
    pub name: String,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Display for TableName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.schema, self.name)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ColumnDefinition {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    /// The owned, unbound DEFAULT syntax. Absence is distinct from an explicit
    /// typed `DEFAULT NULL`; evaluation belongs to the operation that demands a
    /// value, never catalog copying, validation, or serialization.
    pub default: Option<StoredExpression>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ColumnDefaultWire {
    Expression(StoredExpression),
    /// Private snapshots written before retained defaults stored only the
    /// eagerly evaluated payload. Read them conservatively while new snapshots
    /// always write the owned expression form.
    Legacy(Value),
}

#[derive(Deserialize)]
struct ColumnDefinitionWire {
    name: String,
    data_type: DataType,
    nullable: bool,
    #[serde(default)]
    default: Option<ColumnDefaultWire>,
}

impl<'de> Deserialize<'de> for ColumnDefinition {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let wire = ColumnDefinitionWire::deserialize(deserializer)?;
        let default = match wire.default {
            Some(ColumnDefaultWire::Expression(expression)) => Some(expression),
            // The old representation could not distinguish no DEFAULT from
            // DEFAULT NULL. Preserve its historical no-default interpretation.
            Some(ColumnDefaultWire::Legacy(Value::Null)) | None => None,
            Some(ColumnDefaultWire::Legacy(value)) => {
                Some(StoredExpression::literal(wire.data_type.clone(), value))
            }
        };
        Ok(Self {
            name: wire.name,
            data_type: wire.data_type,
            nullable: wire.nullable,
            default,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ColumnDefinition {
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable: true,
            default: None,
        }
    }

    pub fn with_default(mut self, expression: StoredExpression) -> Self {
        self.default = Some(expression);
        self
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// All metadata is resolved in the caller's transaction snapshot.
pub trait Catalog: Send {
    fn schemas(&self) -> Result<Vec<String>>;
    fn table(&self, name: &TableName) -> Result<TableDefinition>;
    fn tables(&self) -> Result<Vec<TableDefinition>>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait CatalogMut: Catalog {
    fn create_schema(&mut self, name: &str, if_not_exists: bool) -> Result<()>;
    fn drop_schema(&mut self, name: &str, if_exists: bool) -> Result<()>;
    fn create_table(&mut self, definition: TableDefinition, if_not_exists: bool) -> Result<()>;
    fn drop_table(&mut self, name: &TableName, if_exists: bool) -> Result<()>;
    /// Atomically alter transaction-visible metadata and its rows. Preserve row
    /// identities, indexes, and older readers. Failure leaves both unchanged;
    /// false denotes an IF EXISTS/IF NOT EXISTS no-op. Unsupported adapters
    /// reject before effects. Successful changes follow ordinary commit/rollback.
    fn alter_table(
        &mut self,
        _name: &TableName,
        _alteration: &TableAlteration,
        _context: &crate::parallel::QueryContext,
    ) -> Result<bool> {
        Err(crate::Error::Unsupported(
            "table alteration on this catalog".into(),
        ))
    }
}
