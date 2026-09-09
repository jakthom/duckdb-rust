use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
};

mod layout;
mod recovery;

use serde::{Deserialize, Serialize};

use super::{RowId, StorageCapabilities, TableStorage, TableStorageMut};
use crate::{
    catalog::{Catalog, CatalogMut, TableDefinition, TableName},
    common::{Error, Result, Row},
    execution::index::{HashIndexFactory, IndexFactory, IndexSpec, KeyIndex},
    parallel::QueryContext,
};

#[derive(Clone, Serialize)]
pub struct Snapshot {
    schemas: BTreeSet<String>,
    tables: BTreeMap<String, Arc<TableData>>,
    #[serde(skip)]
    indexes: Arc<dyn IndexFactory>,
    #[serde(skip)]
    types: Arc<crate::common::type_registry::TypeRegistry>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self::new(crate::common::type_registry::builtin_types())
    }
}
impl Snapshot {
    pub fn new(types: Arc<crate::common::type_registry::TypeRegistry>) -> Self {
        Self {
            schemas: BTreeSet::from(["main".into()]),
            tables: BTreeMap::new(),
            indexes: Arc::new(HashIndexFactory),
            types,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TableData {
    definition: TableDefinition,
    rows: BTreeMap<RowId, Row>,
    next_id: RowId,
    #[serde(skip)]
    indexes: Vec<Arc<dyn KeyIndex>>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("schemas", &self.schemas)
            .field("tables", &self.tables)
            .field("indexes", &self.indexes.name())
            .finish()
    }
}

#[derive(Deserialize)]
struct SnapshotState {
    schemas: BTreeSet<String>,
    tables: BTreeMap<String, Arc<TableData>>,
}
impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let state = SnapshotState::deserialize(deserializer)?;
        Self::restore(state, crate::common::type_registry::builtin_types())
            .map_err(serde::de::Error::custom)
    }
}
impl Snapshot {
    pub(crate) fn decode_json(
        bytes: &[u8],
        types: Arc<crate::common::type_registry::TypeRegistry>,
    ) -> Result<Self> {
        let state = serde_json::from_slice(bytes).map_err(|e| Error::Corrupt(e.to_string()))?;
        Self::restore(state, types)
    }
    fn restore(
        state: SnapshotState,
        types: Arc<crate::common::type_registry::TypeRegistry>,
    ) -> Result<Self> {
        Self {
            schemas: state.schemas,
            tables: state.tables,
            indexes: Arc::new(HashIndexFactory),
            types: types.clone(),
        }
        .with_indexes(
            Arc::new(HashIndexFactory),
            &QueryContext::background().with_types(types),
        )
    }
}

impl Snapshot {
    /// Next unused physical row ID, including holes left by deleted rows.
    /// Log encoders use this to preserve append identity after a checkpoint.
    pub fn next_row_id(&self, table: &TableName) -> Result<RowId> {
        Ok(self.get(table)?.next_id)
    }
    /// Live row IDs in ascending order without copying row payloads.
    pub fn row_ids(&self, table: &TableName) -> Result<Vec<RowId>> {
        Ok(self.get(table)?.rows.keys().copied().collect())
    }
    /// Rebuilds derived index state before exposing a newly selected adapter.
    /// The returned snapshot owns its selection; old snapshots remain usable.
    pub fn with_indexes(
        mut self,
        factory: Arc<dyn IndexFactory>,
        context: &QueryContext,
    ) -> Result<Self> {
        let context = &context.clone().with_types(self.types.clone());
        self.validate_rows(context)?;
        for table in self.tables.values_mut() {
            Arc::make_mut(table).rebuild_indexes(factory.as_ref(), context)?;
        }
        self.indexes = factory;
        Ok(self)
    }
    fn get(&self, name: &TableName) -> Result<&TableData> {
        self.tables
            .get(&name.key())
            .map(Arc::as_ref)
            .ok_or_else(|| Error::Catalog(format!("table {name} does not exist")))
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_with_context(&QueryContext::background())
    }
    pub fn validate_with_context(&self, context: &QueryContext) -> Result<()> {
        let context = context.clone().with_types(self.types.clone());
        self.validate_rows(&context)?;
        for table in self.tables.values() {
            table.build_indexes(self.indexes.as_ref(), &context)?;
        }
        Ok(())
    }
    fn validate_rows(&self, context: &QueryContext) -> Result<()> {
        context.check()?;
        if !self.schemas.contains("main")
            || self
                .schemas
                .iter()
                .any(|s| s.is_empty() || *s != s.to_ascii_lowercase())
        {
            return Err(Error::Corrupt("invalid schema identity".into()));
        }
        for (key, table) in &self.tables {
            if *key != table.definition.name.key()
                || table.rows.keys().any(|&id| id >= table.next_id)
                || !self.schemas.contains(&table.definition.name.schema)
            {
                return Err(Error::Corrupt("invalid table or row identity".into()));
            }
            validate_definition(&table.definition, &self.types)?;
            table.validate_rows(context)?;
        }
        Ok(())
    }
}

impl Catalog for Snapshot {
    fn schemas(&self) -> Result<Vec<String>> {
        Ok(self.schemas.iter().cloned().collect())
    }
    fn table(&self, name: &TableName) -> Result<TableDefinition> {
        Ok(self.get(name)?.definition.clone())
    }
    fn tables(&self) -> Result<Vec<TableDefinition>> {
        Ok(self.tables.values().map(|t| t.definition.clone()).collect())
    }
}

fn validate_definition(
    definition: &TableDefinition,
    types: &Arc<crate::common::type_registry::TypeRegistry>,
) -> Result<()> {
    let mut names = HashSet::new();
    if definition.columns.is_empty() {
        return Err(Error::Catalog("a table needs at least one column".into()));
    }
    for column in &definition.columns {
        if !names.insert(column.name.to_ascii_lowercase()) {
            return Err(Error::Catalog(format!("duplicate column {}", column.name)));
        }
        types
            .bind(&column.data_type)?
            .validate(
                &column.default,
                &QueryContext::background().with_types(types.clone()),
            )
            .map_err(|error| match error {
                Error::Conversion(message) => {
                    Error::Catalog(format!("invalid default for {}: {message}", column.name))
                }
                other => other,
            })?;
    }
    for key in &definition.unique_keys {
        if key.columns.is_empty()
            || key.columns.iter().any(|&i| i >= definition.columns.len())
            || key.columns.iter().collect::<HashSet<_>>().len() != key.columns.len()
        {
            return Err(Error::Catalog("invalid unique key".into()));
        }
        if key.primary && key.columns.iter().any(|&i| definition.columns[i].nullable) {
            return Err(Error::Catalog(
                "primary key columns must be NOT NULL".into(),
            ));
        }
    }
    if definition.unique_keys.iter().filter(|k| k.primary).count() > 1 {
        return Err(Error::Catalog("multiple primary keys".into()));
    }
    Ok(())
}

impl CatalogMut for Snapshot {
    fn create_schema(&mut self, name: &str, if_not_exists: bool) -> Result<()> {
        if name.is_empty() {
            return Err(Error::Catalog("empty schema name".into()));
        }
        if !self.schemas.insert(name.to_ascii_lowercase()) && !if_not_exists {
            return Err(Error::Catalog(format!("schema {name} already exists")));
        }
        Ok(())
    }
    fn drop_schema(&mut self, name: &str, if_exists: bool) -> Result<()> {
        let name = name.to_ascii_lowercase();
        if name == "main" {
            return Err(Error::Catalog("cannot drop the main schema".into()));
        }
        if self
            .tables
            .values()
            .any(|t| t.definition.name.schema == name)
        {
            return Err(Error::Catalog(format!("schema {name} is not empty")));
        }
        if !self.schemas.remove(&name) && !if_exists {
            return Err(Error::Catalog(format!("schema {name} does not exist")));
        }
        Ok(())
    }
    fn create_table(&mut self, definition: TableDefinition, if_not_exists: bool) -> Result<()> {
        if !self.schemas.contains(&definition.name.schema) {
            return Err(Error::Catalog(format!(
                "schema {} does not exist",
                definition.name.schema
            )));
        }
        if self.tables.contains_key(&definition.name.key()) {
            return if if_not_exists {
                Ok(())
            } else {
                Err(Error::Catalog(format!(
                    "table {} already exists",
                    definition.name
                )))
            };
        }
        validate_definition(&definition, &self.types)?;
        let mut table = TableData {
            definition,
            rows: BTreeMap::new(),
            next_id: 0,
            indexes: Vec::new(),
        };
        table.rebuild_indexes(
            self.indexes.as_ref(),
            &QueryContext::background().with_types(self.types.clone()),
        )?;
        self.tables
            .insert(table.definition.name.key(), Arc::new(table));
        Ok(())
    }
    fn drop_table(&mut self, name: &TableName, if_exists: bool) -> Result<()> {
        if self.tables.remove(&name.key()).is_none() && !if_exists {
            return Err(Error::Catalog(format!("table {name} does not exist")));
        }
        Ok(())
    }
}

impl TableData {
    fn validate(&mut self, indexes: &dyn IndexFactory, context: &QueryContext) -> Result<()> {
        self.validate_rows(context)?;
        self.rebuild_indexes(indexes, context)
    }
    fn validate_rows(&self, context: &QueryContext) -> Result<()> {
        context.check_rows(self.rows.len())?;
        let types = self
            .definition
            .columns
            .iter()
            .map(|c| context.types().bind(&c.data_type))
            .collect::<Result<Vec<_>>>()?;
        for row in self.rows.values() {
            context.check()?;
            if row.len() != self.definition.columns.len() {
                return Err(Error::Constraint("row width differs from table".into()));
            }
            for ((value, column), data_type) in row.iter().zip(&self.definition.columns).zip(&types)
            {
                data_type
                    .validate(value, context)
                    .map_err(|error| match error {
                        Error::Conversion(message) => Error::Constraint(format!(
                            "invalid value for {}: {message}",
                            column.name
                        )),
                        other => other,
                    })?;
                if value.is_null() && !column.nullable {
                    return Err(Error::Constraint(format!(
                        "NOT NULL constraint failed: {}.{}",
                        self.definition.name, column.name
                    )));
                }
            }
        }
        Ok(())
    }
    fn rebuild_indexes(
        &mut self,
        factory: &dyn IndexFactory,
        context: &QueryContext,
    ) -> Result<()> {
        self.indexes = self.build_indexes(factory, context)?;
        Ok(())
    }
    fn build_indexes(
        &self,
        factory: &dyn IndexFactory,
        context: &QueryContext,
    ) -> Result<Vec<Arc<dyn KeyIndex>>> {
        let mut indexes = Vec::new();
        for key in &self.definition.unique_keys {
            let spec = IndexSpec {
                key_types: key
                    .columns
                    .iter()
                    .map(|&i| self.definition.columns[i].data_type.clone())
                    .collect(),
                unique: true,
            };
            let mut entries = self
                .rows
                .iter()
                .map(|(&id, row)| (id, key.columns.iter().map(|&i| row[i].clone()).collect()));
            indexes.push(factory.build(spec, &mut entries, context)?);
        }
        Ok(indexes)
    }
}

impl TableStorage for Snapshot {
    fn capabilities(&self) -> StorageCapabilities {
        StorageCapabilities {
            mutable: true,
            positional_fetch: true,
            key_lookup: true,
        }
    }
    fn key_columns(&self, table: &TableName) -> Result<Vec<Vec<usize>>> {
        Ok(self
            .get(table)?
            .definition
            .unique_keys
            .iter()
            .map(|k| k.columns.clone())
            .collect())
    }
    fn open_scan(&self, table: &TableName) -> Result<Box<dyn super::scan::TableScan + '_>> {
        Ok(Box::new(super::scan::SnapshotScan {
            rows: self.get(table)?.rows.iter(),
            finished: false,
        }))
    }
    fn fetch(
        &self,
        table: &TableName,
        ids: &[RowId],
        context: &QueryContext,
    ) -> Result<Vec<Option<Row>>> {
        context.check_rows(ids.len())?;
        let table = self.get(table)?;
        ids.iter()
            .map(|id| {
                context.check()?;
                Ok(table.rows.get(id).cloned())
            })
            .collect()
    }
    fn lookup(
        &self,
        table: &TableName,
        columns: &[usize],
        key: &Row,
        context: &QueryContext,
    ) -> Result<Vec<(RowId, Row)>> {
        context.check()?;
        let table = self.get(table)?;
        let position = table
            .definition
            .unique_keys
            .iter()
            .position(|k| k.columns == columns)
            .ok_or_else(|| Error::Unsupported("no index for the requested key columns".into()))?;
        let index = table
            .indexes
            .get(position)
            .ok_or_else(|| Error::Internal("missing table index".into()))?;
        index
            .lookup(key, context)?
            .into_iter()
            .map(|id| {
                context.check()?;
                table
                    .rows
                    .get(&id)
                    .cloned()
                    .map(|row| (id, row))
                    .ok_or_else(|| Error::Internal("index references an invisible row".into()))
            })
            .collect()
    }
}

impl TableStorageMut for Snapshot {
    fn insert(
        &mut self,
        table: &TableName,
        rows: Vec<Row>,
        context: &QueryContext,
    ) -> Result<usize> {
        context.check_rows(self.get(table)?.rows.len().saturating_add(rows.len()))?;
        let count = rows.len();
        let mut next = self.get(table)?.clone();
        for row in rows {
            context.check()?;
            let id = next.next_id;
            next.next_id = id
                .checked_add(1)
                .ok_or_else(|| Error::Resource("row identity exhausted".into()))?;
            next.rows.insert(id, row);
        }
        next.validate(
            self.indexes.as_ref(),
            &context.clone().with_types(self.types.clone()),
        )?;
        self.tables.insert(table.key(), Arc::new(next));
        Ok(count)
    }
    fn update(
        &mut self,
        table: &TableName,
        rows: Vec<(RowId, Row)>,
        context: &QueryContext,
    ) -> Result<usize> {
        context.check_rows(self.get(table)?.rows.len())?;
        let count = rows.len();
        let mut next = self.get(table)?.clone();
        for (id, row) in rows {
            context.check()?;
            if !next.rows.contains_key(&id) {
                return Err(Error::Transaction(format!("row {id} is not visible")));
            }
            next.rows.insert(id, row);
        }
        next.validate(
            self.indexes.as_ref(),
            &context.clone().with_types(self.types.clone()),
        )?;
        self.tables.insert(table.key(), Arc::new(next));
        Ok(count)
    }
    fn delete(
        &mut self,
        table: &TableName,
        ids: &[RowId],
        context: &QueryContext,
    ) -> Result<usize> {
        context.check_rows(self.get(table)?.rows.len())?;
        let mut next = self.get(table)?.clone();
        let mut count = 0;
        for id in ids {
            context.check()?;
            count += usize::from(next.rows.remove(id).is_some());
        }
        next.rebuild_indexes(
            self.indexes.as_ref(),
            &context.clone().with_types(self.types.clone()),
        )?;
        self.tables.insert(table.key(), Arc::new(next));
        Ok(count)
    }
}
