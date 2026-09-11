use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
};

mod alter;
mod layout;
mod recovery;
pub(crate) use recovery::RestoredSlot;
mod rows;
use rows::Rows;

use serde::{Deserialize, Serialize};

use super::{
    RowId, StorageCapabilities, TableStorage, TableStorageMut, UpdateMetadata, UpdateMode,
};
use crate::{
    catalog::{
        Catalog, CatalogIdentity, CatalogMut, CatalogObjectName, CatalogRegistry, DropBehavior,
        ObjectIdentity, PreparedCatalogInsert, ResolvedTable, TableBinding, TableDefinition,
        TableName,
    },
    common::{Error, Result, Row},
    execution::index::{HashIndexFactory, IndexFactory, IndexSpec, KeyIndex},
    parallel::QueryContext,
};

#[derive(Clone, Serialize)]
pub struct Snapshot {
    schemas: BTreeSet<String>,
    tables: BTreeMap<String, Arc<TableData>>,
    #[serde(skip)]
    registry: CatalogRegistry,
    #[serde(skip)]
    indexes: Arc<dyn IndexFactory>,
    #[serde(skip)]
    types: Arc<crate::common::type_registry::TypeRegistry>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for Snapshot {
    fn default() -> Self {
        Self::new(crate::common::type_registry::builtin_types())
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    /// Retain this snapshot's selected type services for native serialization.
    /// A codec must not substitute the ambient or builtin registry.
    pub fn type_registry(&self) -> Arc<crate::common::type_registry::TypeRegistry> {
        self.types.clone()
    }
    pub fn new(types: Arc<crate::common::type_registry::TypeRegistry>) -> Self {
        Self::try_new(types).expect("runtime catalog identity space exhausted")
    }
    /// Construct a fresh in-memory snapshot without hiding runtime identity
    /// allocation failure from fallible database startup paths.
    pub fn try_new(types: Arc<crate::common::type_registry::TypeRegistry>) -> Result<Self> {
        Ok(Self {
            schemas: BTreeSet::from(["main".into()]),
            tables: BTreeMap::new(),
            registry: CatalogRegistry::rebuild(["main".into()], [])?,
            indexes: Arc::new(HashIndexFactory),
            types,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TableData {
    definition: TableDefinition,
    rows: Rows,
    next_id: RowId,
    /// Physical evaluation order retained between checkpoints. Deleted slots
    /// retain their old identity until checkpoint reclamation; live slots
    /// identify the logical row that receives an ADD COLUMN default result.
    #[serde(default = "missing_physical_slots")]
    physical_slots: Vec<PhysicalSlot>,
    #[serde(skip)]
    indexes: Vec<Arc<dyn KeyIndex>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum PhysicalSlot {
    Live(RowId),
    Deleted(RowId),
}

fn missing_physical_slots() -> Vec<PhysicalSlot> {
    vec![PhysicalSlot::Deleted(RowId::MAX)]
}

impl PhysicalSlot {
    fn row_id(self) -> RowId {
        match self {
            Self::Live(id) | Self::Deleted(id) => id,
        }
    }
    fn live(self) -> Option<RowId> {
        match self {
            Self::Live(id) => Some(id),
            Self::Deleted(_) => None,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("schemas", &self.schemas)
            .field("tables", &self.tables)
            .field("catalog", &self.registry.identity())
            .field("indexes", &self.indexes.name())
            .finish()
    }
}

#[derive(Deserialize)]
struct SnapshotState {
    schemas: BTreeSet<String>,
    tables: BTreeMap<String, Arc<TableData>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let state = SnapshotState::deserialize(deserializer)?;
        Self::restore(state, crate::common::type_registry::builtin_types())
            .map_err(serde::de::Error::custom)
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    pub(crate) fn decode_json(
        bytes: &[u8],
        types: Arc<crate::common::type_registry::TypeRegistry>,
    ) -> Result<Self> {
        let state = serde_json::from_slice(bytes).map_err(|e| Error::Corrupt(e.to_string()))?;
        Self::restore(state, types)
    }
    fn restore(
        mut state: SnapshotState,
        types: Arc<crate::common::type_registry::TypeRegistry>,
    ) -> Result<Self> {
        for table in state.tables.values_mut() {
            let table = Arc::make_mut(table);
            if table.physical_slots == missing_physical_slots() {
                table.physical_slots = (0..table.next_id)
                    .map(|id| {
                        if table.rows.contains_key(&id) {
                            PhysicalSlot::Live(id)
                        } else {
                            PhysicalSlot::Deleted(id)
                        }
                    })
                    .collect();
            }
        }
        let registry = CatalogRegistry::rebuild(
            state.schemas.iter().cloned(),
            state
                .tables
                .values()
                .map(|table| table.definition.name.clone()),
        )?;
        Self {
            schemas: state.schemas,
            tables: state.tables,
            registry,
            indexes: Arc::new(HashIndexFactory),
            types: types.clone(),
        }
        .with_indexes(
            Arc::new(HashIndexFactory),
            &QueryContext::background().with_types(types),
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    /// Transaction payload views may differ in rows, but their runtime catalog
    /// metadata must remain byte-for-byte equivalent.
    pub(crate) fn has_same_runtime_catalog(&self, other: &Self) -> bool {
        self.registry == other.registry
    }

    /// Next unused physical row ID, including holes left by deleted rows.
    /// Log encoders use this to preserve append identity after a checkpoint.
    pub fn next_row_id(&self, table: &TableName) -> Result<RowId> {
        Ok(self.get(table)?.next_id)
    }
    /// Live row IDs in ascending order without copying row payloads.
    pub fn row_ids(&self, table: &TableName) -> Result<Vec<RowId>> {
        Ok(self.get(table)?.rows.keys().copied().collect())
    }
    /// Live logical IDs in retained physical scan order. Native checkpoint
    /// encoding uses this stream so delete-plus-insert updates do not revert to
    /// logical-ID order when the file is compacted.
    pub(crate) fn physical_row_ids(&self, table: &TableName) -> Result<Vec<RowId>> {
        Ok(self
            .get(table)?
            .physical_slots
            .iter()
            .filter_map(|slot| slot.live())
            .collect())
    }
    pub(crate) fn scan_physical(
        &self,
        table: &TableName,
        context: &QueryContext,
    ) -> Result<Vec<(RowId, Row)>> {
        let ids = self.physical_row_ids(table)?;
        context.check_rows(ids.len())?;
        let table = self.get(table)?;
        ids.into_iter()
            .map(|id| {
                context.check()?;
                let row = table.rows.get(&id).ok_or_else(|| {
                    Error::Internal("physical slot references an invisible row".into())
                })?;
                Ok((id, row.to_owned()))
            })
            .collect()
    }
    /// Return a snapshot with checkpoint-reclaimed physical slots. Existing
    /// clones remain unchanged and continue to represent their older demand.
    pub(crate) fn reclaim_for_checkpoint(&self) -> Self {
        let mut compacted = self.clone();
        for table in compacted.tables.values_mut() {
            let table = Arc::make_mut(table);
            table.physical_slots = table
                .physical_slots
                .iter()
                .filter_map(|slot| slot.live().map(PhysicalSlot::Live))
                .collect();
        }
        compacted
    }
    /// Remove only deleted physical slots already present in the acknowledged
    /// snapshot checkpointed immediately before this transaction. Journaled
    /// renames keep the old table lineage; transaction-local deletes remain.
    pub(crate) fn reclaim_checkpointed_basis(
        &self,
        basis: &Self,
        changes: Option<&[super::log::TransactionChange]>,
    ) -> Self {
        let mut lineages = basis
            .tables
            .values()
            .map(|table| {
                let name = table.definition.name.clone();
                let deleted = table
                    .physical_slots
                    .iter()
                    .filter_map(|slot| match slot {
                        PhysicalSlot::Deleted(id) => Some(*id),
                        PhysicalSlot::Live(_) => None,
                    })
                    .collect::<HashSet<_>>();
                (name, deleted)
            })
            .collect::<BTreeMap<_, _>>();
        for change in changes.unwrap_or_default() {
            match change {
                super::log::TransactionChange::DropTable(name) => {
                    lineages.remove(name);
                }
                super::log::TransactionChange::AlterTable {
                    table,
                    alteration: crate::catalog::TableAlteration::RenameTable(name),
                    ..
                } => {
                    if let Some(deleted) = lineages.remove(table) {
                        let renamed = TableName::new(&table.schema, name);
                        lineages.insert(renamed, deleted);
                    }
                }
                _ => {}
            }
        }
        let mut result = self.clone();
        for (name, deleted) in lineages {
            if deleted.is_empty() {
                continue;
            }
            if let Some(table) = result.tables.get_mut(&name.key()) {
                Arc::make_mut(table).physical_slots.retain(
                    |slot| !matches!(slot, PhysicalSlot::Deleted(id) if deleted.contains(id)),
                );
            }
        }
        result
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
        self.registry.validate()?;
        let catalog_objects = self
            .schemas
            .len()
            .checked_add(self.tables.len())
            .ok_or_else(|| Error::Resource("snapshot catalog object count overflow".into()))?;
        if self.registry.len() != catalog_objects {
            return Err(Error::Corrupt(
                "runtime catalog registry differs from snapshot objects".into(),
            ));
        }
        if !self.schemas.contains("main")
            || self
                .schemas
                .iter()
                .any(|s| s.is_empty() || *s != s.to_ascii_lowercase())
        {
            return Err(Error::Corrupt("invalid schema identity".into()));
        }
        for schema in &self.schemas {
            if self.registry.lookup_schema(schema)?.is_none() {
                return Err(Error::Corrupt(
                    "runtime registry omits a snapshot schema".into(),
                ));
            }
        }
        for (key, table) in &self.tables {
            if *key != table.definition.name.key()
                || table.rows.keys().any(|&id| id >= table.next_id)
                || !self.schemas.contains(&table.definition.name.schema)
            {
                return Err(Error::Corrupt("invalid table or row identity".into()));
            }
            if self
                .registry
                .lookup_table(&table.definition.name)?
                .is_none()
            {
                return Err(Error::Corrupt(
                    "runtime registry omits a snapshot table".into(),
                ));
            }
            validate_definition(&table.definition, &self.types)?;
            let live_slots = table
                .physical_slots
                .iter()
                .filter_map(|slot| slot.live())
                .collect::<Vec<_>>();
            if live_slots.len() != table.rows.len()
                || live_slots.iter().copied().collect::<HashSet<_>>().len() != live_slots.len()
                || live_slots.iter().any(|id| !table.rows.contains_key(id))
                || table
                    .physical_slots
                    .iter()
                    .any(|slot| slot.row_id() >= table.next_id)
            {
                return Err(Error::Corrupt("invalid physical row slots".into()));
            }
            table.validate_rows(context)?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Catalog for Snapshot {
    fn identity(&self) -> Option<CatalogIdentity> {
        Some(self.registry.identity())
    }
    fn schemas(&self) -> Result<Vec<String>> {
        Ok(self.schemas.iter().cloned().collect())
    }
    fn table(&self, name: &TableName) -> Result<TableDefinition> {
        Ok(self.get(name)?.definition.clone())
    }
    fn tables(&self) -> Result<Vec<TableDefinition>> {
        Ok(self.tables.values().map(|t| t.definition.clone()).collect())
    }
    fn table_entry(&self, name: &TableName) -> Result<ResolvedTable> {
        let definition = self.table(name)?;
        let binding = self.registry.bind_table(name)?;
        ResolvedTable::identified(
            binding
                .identity()
                .ok_or_else(|| Error::Internal("runtime table binding has no identity".into()))?,
            self.registry.identity(),
            definition,
        )
    }
    fn table_entry_if_exists(&self, name: &TableName) -> Result<Option<ResolvedTable>> {
        if self.registry.lookup_table(name)?.is_none() {
            return Ok(None);
        }
        self.table_entry(name).map(Some)
    }
    fn table_by_identity(&self, identity: &ObjectIdentity) -> Result<ResolvedTable> {
        let name = self.registry.name(*identity)?.table_name()?;
        ResolvedTable::identified(*identity, self.registry.identity(), self.table(&name)?)
    }
    fn table_by_identity_if_exists(
        &self,
        identity: &ObjectIdentity,
    ) -> Result<Option<ResolvedTable>> {
        let Some(name) = self.registry.name_if_exists(*identity)? else {
            return Ok(None);
        };
        let name = name.table_name()?;
        ResolvedTable::identified(*identity, self.registry.identity(), self.table(&name)?).map(Some)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
        types.bind(&column.data_type)?;
        if let Some(default) = &column.default {
            default
                .validate(&QueryContext::background().with_types(types.clone()))
                .map_err(|error| match error {
                    Error::Conversion(message) => {
                        Error::Catalog(format!("invalid default for {}: {message}", column.name))
                    }
                    other => other,
                })?;
        }
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
        for &index in &key.columns {
            let data_type = &definition.columns[index].data_type;
            if !types.bind(data_type)?.supports_index() {
                return Err(Error::InvalidType(format!(
                    "Invalid Type [{data_type}]: Invalid type for index key."
                )));
            }
        }
    }
    if definition.unique_keys.iter().filter(|k| k.primary).count() > 1 {
        return Err(Error::Catalog("multiple primary keys".into()));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CatalogMut for Snapshot {
    fn alter_table(
        &mut self,
        name: &TableName,
        alteration: &crate::catalog::TableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        self.alter(name, alteration, context)
    }
    fn create_schema(&mut self, name: &str, if_not_exists: bool) -> Result<()> {
        let Some(prepared) = self.prepare_schema_creation(name, if_not_exists)? else {
            return Ok(());
        };
        self.apply_schema_creation(name, &prepared)
    }
    fn drop_schema(&mut self, name: &str, if_exists: bool) -> Result<()> {
        let name = name.to_ascii_lowercase();
        if name == "main" {
            return Err(Error::Catalog("cannot drop the main schema".into()));
        }
        if !self.schemas.contains(&name) {
            return if if_exists {
                Ok(())
            } else {
                Err(Error::Catalog(format!("schema {name} does not exist")))
            };
        }
        let identity = self
            .registry
            .lookup_schema(&name)?
            .ok_or_else(|| Error::Internal("runtime registry lost schema".into()))?;
        let mut registry = self.registry.clone();
        let removed = registry.drop_object(identity, DropBehavior::Restrict)?;
        if removed.len() != 1
            || removed[0].identity() != identity
            || removed[0].name().kind() != crate::catalog::CatalogObjectKind::Schema
        {
            return Err(Error::Internal(
                "restricted schema drop produced an invalid plan".into(),
            ));
        }
        self.schemas.remove(&name);
        self.registry = registry;
        Ok(())
    }
    fn create_table(&mut self, definition: TableDefinition, if_not_exists: bool) -> Result<()> {
        let Some(prepared) = self.prepare_table_creation(&definition, if_not_exists)? else {
            return Ok(());
        };
        self.apply_table_creation(definition, &prepared)
    }
    fn drop_table(&mut self, name: &TableName, if_exists: bool) -> Result<()> {
        if !self.tables.contains_key(&name.key()) {
            return if if_exists {
                Ok(())
            } else {
                Err(Error::Catalog(format!("table {name} does not exist")))
            };
        }
        let identity = self
            .registry
            .lookup_table(name)?
            .ok_or_else(|| Error::Internal("runtime registry lost table".into()))?;
        let mut registry = self.registry.clone();
        let removed = registry.drop_object(identity, DropBehavior::Restrict)?;
        if removed.len() != 1 || removed[0].identity() != identity {
            return Err(Error::Internal(
                "restricted table drop produced an invalid plan".into(),
            ));
        }
        self.tables.remove(&name.key());
        self.registry = registry;
        Ok(())
    }

    fn drop_table_identified(&mut self, table: &TableBinding, if_exists: bool) -> Result<()> {
        if table.identity().is_none() {
            return Err(Error::InvalidInput(
                "runtime snapshot requires an identified table binding".into(),
            ));
        }
        let name = match self.resolve_table_binding_if_exists(table)? {
            Some(resolved) => resolved.definition().name.clone(),
            None if if_exists => return Ok(()),
            None => return Err(Error::Catalog(format!("table {table} does not exist"))),
        };
        self.drop_table(&name, if_exists)
    }

    fn alter_table_identified(
        &mut self,
        table: &TableBinding,
        alteration: &crate::catalog::TableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        let name = self.registry.table_name_for_binding(table)?;
        self.alter_table(&name, alteration, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    pub(crate) fn prepare_schema_creation(
        &self,
        name: &str,
        if_not_exists: bool,
    ) -> Result<Option<PreparedCatalogInsert>> {
        if name.is_empty() {
            return Err(Error::Catalog("empty schema name".into()));
        }
        let name = name.to_ascii_lowercase();
        if self.schemas.contains(&name) {
            return if if_not_exists {
                Ok(None)
            } else {
                Err(Error::Catalog(format!("schema {name} already exists")))
            };
        }
        self.registry
            .prepare_insert(CatalogObjectName::schema(name)?)
            .map(Some)
    }

    pub(crate) fn apply_schema_creation(
        &mut self,
        name: &str,
        prepared: &PreparedCatalogInsert,
    ) -> Result<()> {
        if name.is_empty() {
            return Err(Error::Catalog("empty schema name".into()));
        }
        let name = name.to_ascii_lowercase();
        if self.schemas.contains(&name) {
            return Err(Error::Catalog(format!("schema {name} already exists")));
        }
        if prepared.name() != &CatalogObjectName::schema(&name)? {
            return Err(Error::InvalidInput(
                "prepared schema insertion has a different name".into(),
            ));
        }
        let mut registry = self.registry.clone();
        registry.insert_prepared(prepared)?;
        self.schemas.insert(name);
        self.registry = registry;
        Ok(())
    }

    pub(crate) fn prepare_table_creation(
        &self,
        definition: &TableDefinition,
        if_not_exists: bool,
    ) -> Result<Option<PreparedCatalogInsert>> {
        if !self.schemas.contains(&definition.name.schema) {
            return Err(Error::Catalog(format!(
                "schema {} does not exist",
                definition.name.schema
            )));
        }
        if self.tables.contains_key(&definition.name.key()) {
            return if if_not_exists {
                Ok(None)
            } else {
                Err(Error::Catalog(format!(
                    "table {} already exists",
                    definition.name
                )))
            };
        }
        validate_definition(definition, &self.types)?;
        self.registry
            .prepare_insert(CatalogObjectName::table(&definition.name)?)
            .map(Some)
    }

    pub(crate) fn apply_table_creation(
        &mut self,
        definition: TableDefinition,
        prepared: &PreparedCatalogInsert,
    ) -> Result<()> {
        if !self.schemas.contains(&definition.name.schema) {
            return Err(Error::Catalog(format!(
                "schema {} does not exist",
                definition.name.schema
            )));
        }
        if self.tables.contains_key(&definition.name.key()) {
            return Err(Error::Catalog(format!(
                "table {} already exists",
                definition.name
            )));
        }
        validate_definition(&definition, &self.types)?;
        if prepared.name() != &CatalogObjectName::table(&definition.name)? {
            return Err(Error::InvalidInput(
                "prepared table insertion has a different name".into(),
            ));
        }
        let mut table = TableData {
            definition,
            rows: Rows::default(),
            next_id: 0,
            physical_slots: Vec::new(),
            indexes: Vec::new(),
        };
        table.rebuild_indexes(
            self.indexes.as_ref(),
            &QueryContext::background().with_types(self.types.clone()),
        )?;
        let mut registry = self.registry.clone();
        registry.insert_prepared(prepared)?;
        self.tables
            .insert(table.definition.name.key(), Arc::new(table));
        self.registry = registry;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
        self.rows.seal(
            self.definition
                .columns
                .iter()
                .map(|column| column.data_type.clone())
                .collect(),
            context,
        )
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableStorage for Snapshot {
    fn row_count(&self, table: &TableName) -> Result<Option<usize>> {
        Ok(Some(self.get(table)?.rows.len()))
    }
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
        let table = self.get(table)?;
        let order = table
            .physical_slots
            .iter()
            .filter_map(|slot| slot.live())
            .collect::<Vec<_>>();
        Ok(Box::new(table.rows.scan_ordered(&order)?))
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
                Ok(table.rows.get(id).map(rows::RowView::to_owned))
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
                    .map(rows::RowView::to_owned)
                    .map(|row| (id, row))
                    .ok_or_else(|| Error::Internal("index references an invisible row".into()))
            })
            .collect()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
            next.physical_slots.push(PhysicalSlot::Live(id));
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
        metadata: &UpdateMetadata,
        rows: Vec<(RowId, Row)>,
        context: &QueryContext,
    ) -> Result<usize> {
        context.check_rows(self.get(table)?.rows.len())?;
        let mut next = self.get(table)?.clone();
        metadata.validate_for(&next.definition)?;
        // Statement validation observes the final replacement for each logical
        // row. Relocating a duplicate more than once would manufacture phantom
        // physical slots, so normalize before changing either representation.
        let rows = super::normalize_update_rows(rows);
        let count = rows.len();
        for (id, row) in &rows {
            context.check()?;
            if !next.rows.contains_key(id) {
                return Err(Error::Transaction(format!("row {id} is not visible")));
            }
            next.rows.insert(*id, row.clone());
        }
        if metadata.mode == UpdateMode::DeleteInsert {
            let mut relocated = Vec::with_capacity(count);
            for (id, _) in &rows {
                let slot = next
                    .physical_slots
                    .iter_mut()
                    .find(|slot| matches!(slot, PhysicalSlot::Live(row_id) if row_id == id))
                    .ok_or_else(|| {
                        Error::Internal("updated row has no live physical slot".into())
                    })?;
                *slot = PhysicalSlot::Deleted(*id);
                relocated.push(PhysicalSlot::Live(*id));
            }
            next.physical_slots.extend(relocated);
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
            if next.rows.remove(id).is_some() {
                let slot = next
                    .physical_slots
                    .iter_mut()
                    .find(|slot| matches!(slot, PhysicalSlot::Live(row_id) if row_id == id))
                    .ok_or_else(|| Error::Internal("live row has no physical slot".into()))?;
                *slot = PhysicalSlot::Deleted(*id);
                count += 1;
            }
        }
        next.rebuild_indexes(
            self.indexes.as_ref(),
            &context.clone().with_types(self.types.clone()),
        )?;
        self.tables.insert(table.key(), Arc::new(next));
        Ok(count)
    }
}
