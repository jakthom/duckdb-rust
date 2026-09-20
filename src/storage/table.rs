use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
};

mod alter;
mod layout;
mod recovery;
pub(crate) use recovery::RestoredSlot;
mod rows;
pub(crate) use rows::PackedBigInts;
use rows::Rows;

use serde::{Deserialize, Serialize};

use super::{
    RowId, StorageCapabilities, TableStorage, TableStorageMut, UpdateMetadata, UpdateMode,
};
use crate::{
    catalog::{
        Catalog, CatalogIdentity, CatalogMut, CatalogObjectKind, CatalogObjectName,
        CatalogRegistry, CreateConflictPolicy, DropBehavior, ObjectIdentity, PreparedCatalogCreate,
        PreparedCatalogInsert, ResolvedTable, ResolvedType, ResolvedView, TableBinding,
        TableDefinition, TableName, TypeBinding, TypeDefinition, TypeName, ViewBinding,
        ViewDefinition,
        macro_definition::ScalarMacroDefinition,
    },
    common::{DataType, Error, Result, Row, Value, vector::DataChunk},
    execution::index::{HashIndexFactory, IndexFactory, IndexSpec, KeyIndex},
    parallel::QueryContext,
};

#[derive(Clone, Serialize)]
pub struct Snapshot {
    schemas: BTreeSet<String>,
    tables: BTreeMap<String, Arc<TableData>>,
    views: BTreeMap<String, Arc<ViewDefinition>>,
    named_types: BTreeMap<String, TypeDefinition>,
    #[serde(default)]
    pub(crate) scalar_macros: BTreeMap<String, ScalarMacroDefinition>,
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
            views: BTreeMap::new(),
            named_types: BTreeMap::new(),
            scalar_macros: BTreeMap::new(),
            registry: CatalogRegistry::rebuild_with_types(["main".into()], [], [])?,
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
    /// Append-only tables imply their physical stream from `next_id`; a slot
    /// vector is allocated only once a delete, relocation, or legacy restore
    /// makes that stream non-contiguous.  This deliberately avoids retaining a
    /// second million-entry RowId allocation next to published CTAS columns.
    #[serde(default, alias = "physical_slots")]
    physical_order: PhysicalOrder,
    #[serde(skip)]
    indexes: Vec<Arc<dyn KeyIndex>>,
}

/// A physical row identity plus its visibility bit.  This deliberately uses
/// one machine word: a table already retains the live identity column, and a
/// second 16-byte enum for every physical row made append-heavy CTAS tables
/// retain substantially more provenance than the actual values require.
///
/// Row identities reserve their high bit while they are retained as physical
/// slots.  The public identity space is still enormous; attempting to enter
/// the reserved half reports a resource error rather than aliasing a deleted
/// slot.  Native layouts and WAL entries continue to carry the original ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PhysicalSlot(RowId);

#[derive(Serialize, Deserialize)]
enum SerializedPhysicalSlot {
    Live(RowId),
    Deleted(RowId),
}

const DELETED_SLOT_BIT: RowId = RowId::MAX ^ (RowId::MAX >> 1);
const PHYSICAL_ROW_ID_MAX: RowId = DELETED_SLOT_BIT - 1;

#[derive(Clone, Debug, Default)]
enum PhysicalOrder {
    #[default]
    ImplicitAppend,
    Explicit(Arc<Vec<PhysicalSlot>>),
}

#[derive(Serialize, Deserialize)]
enum PhysicalOrderWire {
    ImplicitAppend,
    Explicit(Vec<PhysicalSlot>),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Serialize for PhysicalOrder {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::ImplicitAppend => PhysicalOrderWire::ImplicitAppend.serialize(serializer),
            Self::Explicit(slots) => {
                PhysicalOrderWire::Explicit((**slots).clone()).serialize(serializer)
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'de> Deserialize<'de> for PhysicalOrder {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Current(PhysicalOrderWire),
            Legacy(Vec<PhysicalSlot>),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Current(PhysicalOrderWire::ImplicitAppend) => Self::ImplicitAppend,
            Wire::Current(PhysicalOrderWire::Explicit(slots)) | Wire::Legacy(slots) => {
                Self::Explicit(Arc::new(slots))
            }
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PhysicalSlot {
    fn present(id: RowId) -> Result<Self> {
        if id > PHYSICAL_ROW_ID_MAX {
            return Err(Error::Resource("physical row identity exhausted".into()));
        }
        Ok(Self(id))
    }
    fn deleted(id: RowId) -> Result<Self> {
        if id > PHYSICAL_ROW_ID_MAX {
            return Err(Error::Resource("physical row identity exhausted".into()));
        }
        Ok(Self(id | DELETED_SLOT_BIT))
    }
    fn row_id(self) -> RowId {
        self.0 & PHYSICAL_ROW_ID_MAX
    }
    fn live(self) -> Option<RowId> {
        (self.0 & DELETED_SLOT_BIT == 0).then_some(self.0)
    }
    fn is_deleted(self) -> bool {
        self.0 & DELETED_SLOT_BIT != 0
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Serialize for PhysicalSlot {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let state = if self.is_deleted() {
            SerializedPhysicalSlot::Deleted(self.row_id())
        } else {
            SerializedPhysicalSlot::Live(self.row_id())
        };
        state.serialize(serializer)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'de> Deserialize<'de> for PhysicalSlot {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        match SerializedPhysicalSlot::deserialize(deserializer)? {
            SerializedPhysicalSlot::Live(id) => Self::present(id),
            SerializedPhysicalSlot::Deleted(id) => Self::deleted(id),
        }
        .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod physical_slot_tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn compact_physical_slots_retain_visibility_identity_and_json_contract() -> Result<()> {
        assert_eq!(
            std::mem::size_of::<PhysicalSlot>(),
            std::mem::size_of::<RowId>()
        );
        let live = PhysicalSlot::present(42)?;
        let deleted = PhysicalSlot::deleted(42)?;
        assert_eq!(live.live(), Some(42));
        assert!(!live.is_deleted());
        assert_eq!(deleted.live(), None);
        assert!(deleted.is_deleted());
        assert_eq!(deleted.row_id(), 42);
        // Keep the snapshot's previous tagged JSON representation readable.
        let restored: PhysicalSlot = serde_json::from_str(r#"{"Deleted":42}"#).unwrap();
        assert_eq!(restored, deleted);
        assert_eq!(serde_json::to_string(&live).unwrap(), r#"{"Live":42}"#);
        assert!(PhysicalSlot::present(PHYSICAL_ROW_ID_MAX).is_ok());
        assert!(PhysicalSlot::present(DELETED_SLOT_BIT).is_err());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn implicit_append_stream_is_range_backed_and_materializes_only_for_holes() -> Result<()> {
        let mut order = PhysicalOrder::ImplicitAppend;
        let legacy: PhysicalOrder = serde_json::from_str(r#"[{"Live":0},{"Deleted":1}]"#).unwrap();
        assert_eq!(
            legacy
                .slots(2)
                .filter_map(|slot| slot.live())
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            serde_json::to_string(&order).unwrap(),
            r#""ImplicitAppend""#
        );
        assert_eq!(
            order
                .slots(3)
                .filter_map(|slot| slot.live())
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(matches!(order, PhysicalOrder::ImplicitAppend));
        order.materialize(3)[1] = PhysicalSlot::deleted(1)?;
        assert!(matches!(order, PhysicalOrder::Explicit(_)));
        assert_eq!(
            order
                .slots(3)
                .filter_map(|slot| slot.live())
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PhysicalOrder {
    fn slots(&self, next_id: RowId) -> Box<dyn Iterator<Item = PhysicalSlot> + '_> {
        match self {
            Self::ImplicitAppend => Box::new(
                (0..next_id).map(|id| PhysicalSlot::present(id).expect("next ID is representable")),
            ),
            Self::Explicit(slots) => Box::new(slots.iter().copied()),
        }
    }
    fn materialize(&mut self, next_id: RowId) -> &mut Vec<PhysicalSlot> {
        if matches!(self, Self::ImplicitAppend) {
            *self = Self::Explicit(Arc::new(
                (0..next_id)
                    .map(|id| PhysicalSlot::present(id).expect("next ID is representable"))
                    .collect(),
            ));
        }
        match self {
            Self::Explicit(slots) => Arc::make_mut(slots),
            Self::ImplicitAppend => unreachable!(),
        }
    }
    fn append(&mut self, next_id_before: RowId, id: RowId) -> Result<()> {
        debug_assert_eq!(id, next_id_before);
        if let Self::Explicit(slots) = self {
            Arc::make_mut(slots).push(PhysicalSlot::present(id)?);
        }
        Ok(())
    }
    fn explicit(slots: Vec<PhysicalSlot>, next_id: RowId) -> Self {
        if slots.iter().filter_map(|slot| slot.live()).eq(0..next_id)
            && slots.len() == usize::try_from(next_id).unwrap_or(usize::MAX)
        {
            Self::ImplicitAppend
        } else {
            Self::Explicit(Arc::new(slots))
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("schemas", &self.schemas)
            .field("tables", &self.tables)
            .field("views", &self.views)
            .field("named_types", &self.named_types)
            .field("catalog", &self.registry.identity())
            .field("indexes", &self.indexes.name())
            .finish()
    }
}

#[derive(Deserialize)]
struct SnapshotState {
    schemas: BTreeSet<String>,
    tables: BTreeMap<String, Arc<TableData>>,
    #[serde(default)]
    views: BTreeMap<String, ViewDefinition>,
    #[serde(default)]
    named_types: BTreeMap<String, TypeDefinition>,
    #[serde(default)]
    scalar_macros: BTreeMap<String, ScalarMacroDefinition>,
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
            // Older snapshots omit the field entirely.  Their row map is the
            // only available provenance, so reconstruct the legacy stream.
            if matches!(table.physical_order, PhysicalOrder::ImplicitAppend)
                && !table.rows.keys().copied().eq(0..table.next_id)
            {
                table.physical_order = PhysicalOrder::Explicit(Arc::new(
                    (0..table.next_id)
                        .map(|id| {
                            if table.rows.contains_key(&id) {
                                PhysicalSlot::present(id).expect("restored identity was validated")
                            } else {
                                PhysicalSlot::deleted(id).expect("restored identity was validated")
                            }
                        })
                        .collect(),
                ));
            }
        }
        let registry = CatalogRegistry::rebuild_with_views_types(
            state.schemas.iter().cloned(),
            state
                .tables
                .values()
                .map(|table| table.definition.name.clone()),
            state
                .views
                .values()
                .map(|definition| definition.name.clone()),
            state
                .named_types
                .values()
                .map(|definition| definition.name.clone()),
        )?;
        Self {
            schemas: state.schemas,
            tables: state.tables,
            views: state
                .views
                .into_iter()
                .map(|(key, definition)| (key, Arc::new(definition)))
                .collect(),
            named_types: state.named_types,
            scalar_macros: state.scalar_macros,
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
            .physical_order
            .slots(self.get(table)?.next_id)
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
    /// Private checkpoint-only fast-path probe. It borrows an already
    /// validated selected builtin lane and otherwise deliberately falls back
    /// to the generic physical row scan.
    pub(crate) fn implicit_append_bigints(
        &self,
        table: &TableDefinition,
        context: &QueryContext,
    ) -> Result<Option<PackedBigInts<'_>>> {
        if table.columns.len() != 1
            || table.columns[0].data_type != DataType::BigInt
            || !table.unique_keys.is_empty()
        {
            return Ok(None);
        }
        if context
            .bind_type(&DataType::BigInt)?
            .requires_logical_validation()
        {
            return Ok(None);
        }
        let data = self.get(&table.name)?;
        if data.definition != *table {
            return Ok(None);
        }
        if !matches!(data.physical_order, PhysicalOrder::ImplicitAppend) {
            return Ok(None);
        }
        data.rows.implicit_append_bigints(data.next_id, context)
    }
    /// Return a snapshot with checkpoint-reclaimed physical slots. Existing
    /// clones remain unchanged and continue to represent their older demand.
    pub(crate) fn reclaim_for_checkpoint(&self) -> Self {
        let mut compacted = self.clone();
        for table in compacted.tables.values_mut() {
            let table = Arc::make_mut(table);
            table.physical_order = PhysicalOrder::explicit(
                table
                    .physical_order
                    .slots(table.next_id)
                    .filter_map(|slot| slot.live().map(PhysicalSlot::present))
                    .collect::<Result<Vec<_>>>()
                    .expect("existing physical identities remain representable"),
                table.next_id,
            );
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
                    .physical_order
                    .slots(table.next_id)
                    .filter_map(|slot| slot.is_deleted().then_some(slot.row_id()))
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
                let table = Arc::make_mut(table);
                table
                    .physical_order
                    .materialize(table.next_id)
                    .retain(|slot| !(slot.is_deleted() && deleted.contains(&slot.row_id())));
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
            .and_then(|count| count.checked_add(self.views.len()))
            .and_then(|count| count.checked_add(self.named_types.len()))
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
        for (key, definition) in &self.named_types {
            if *key != type_key(&definition.name)
                || !self.schemas.contains(&definition.name.schema)
                || definition.name != TypeName::new(&definition.name.schema, &definition.name.name)
            {
                return Err(Error::Corrupt("invalid named type identity".into()));
            }
            definition.validate().map_err(|error| match error {
                Error::Resource(_) => error,
                other => Error::Corrupt(other.to_string()),
            })?;
            if self.registry.lookup_type(&definition.name)?.is_none() {
                return Err(Error::Corrupt(
                    "runtime registry omits a snapshot named type".into(),
                ));
            }
        }
        for (key, table) in &self.tables {
            if *key != table.definition.name.key()
                || table.rows.keys().any(|&id| id >= table.next_id)
                || table.next_id > DELETED_SLOT_BIT
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
                .physical_order
                .slots(table.next_id)
                .filter_map(|slot| slot.live())
                .collect::<Vec<_>>();
            if live_slots.len() != table.rows.len()
                || live_slots.iter().copied().collect::<HashSet<_>>().len() != live_slots.len()
                || live_slots.iter().any(|id| !table.rows.contains_key(id))
                || table
                    .physical_order
                    .slots(table.next_id)
                    .any(|slot| slot.row_id() >= table.next_id)
            {
                return Err(Error::Corrupt("invalid physical row slots".into()));
            }
            if matches!(table.physical_order, PhysicalOrder::ImplicitAppend)
                && !table.rows.keys().copied().eq(0..table.next_id)
            {
                return Err(Error::Corrupt(
                    "table physical-order metadata differs from rows".into(),
                ));
            }
            table.validate_rows(context)?;
        }
        for (key, view) in &self.views {
            if *key != view.name.key()
                || !self.schemas.contains(&view.name.schema)
                || view.name != TableName::new(&view.name.schema, &view.name.name)
                || view.names.len() != view.types.len()
                || view.aliases.len() > view.types.len()
            {
                return Err(Error::Corrupt("invalid view definition".into()));
            }
            if self.registry.lookup_view(&view.name)?.is_none() {
                return Err(Error::Corrupt(
                    "runtime registry omits a snapshot view".into(),
                ));
            }
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
    fn scalar_macro(&self, name: &TableName) -> Result<ScalarMacroDefinition> {
        self.scalar_macros.get(&name.key()).cloned().ok_or_else(|| Error::Catalog(format!("scalar macro {name} does not exist")))
    }
    fn scalar_macros(&self) -> Result<Vec<ScalarMacroDefinition>> {
        Ok(self.scalar_macros.values().cloned().collect())
    }
    fn view(&self, name: &TableName) -> Result<ViewDefinition> {
        self.views
            .get(&name.key())
            .map(|definition| definition.as_ref().clone())
            .ok_or_else(|| Error::Catalog(format!("view {name} does not exist")))
    }
    fn views(&self) -> Result<Vec<ViewDefinition>> {
        Ok(self
            .views
            .values()
            .map(|definition| definition.as_ref().clone())
            .collect())
    }
    fn view_entry(&self, name: &TableName) -> Result<ResolvedView> {
        let definition = self.view(name)?;
        let binding = self.registry.bind_view(name)?;
        ResolvedView::identified(
            binding
                .identity()
                .ok_or_else(|| Error::Internal("runtime view binding has no identity".into()))?,
            self.registry.identity(),
            definition,
        )
    }
    fn view_entry_if_exists(&self, name: &TableName) -> Result<Option<ResolvedView>> {
        if self.registry.lookup_view(name)?.is_none() {
            return Ok(None);
        }
        self.view_entry(name).map(Some)
    }
    fn view_by_identity(&self, identity: &ObjectIdentity) -> Result<ResolvedView> {
        let name = self.registry.name(*identity)?.view_name()?;
        ResolvedView::identified(*identity, self.registry.identity(), self.view(&name)?)
    }
    fn view_by_identity_if_exists(
        &self,
        identity: &ObjectIdentity,
    ) -> Result<Option<ResolvedView>> {
        let Some(name) = self.registry.name_if_exists(*identity)? else {
            return Ok(None);
        };
        let name = name.view_name()?;
        ResolvedView::identified(*identity, self.registry.identity(), self.view(&name)?).map(Some)
    }
    fn named_type(&self, name: &TypeName) -> Result<TypeDefinition> {
        self.named_types
            .get(&type_key(name))
            .cloned()
            .ok_or_else(|| Error::Catalog(format!("type {name} does not exist")))
    }
    fn named_types(&self) -> Result<Vec<TypeDefinition>> {
        Ok(self.named_types.values().cloned().collect())
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
    fn type_entry(&self, name: &TypeName) -> Result<ResolvedType> {
        let definition = self.named_type(name)?;
        let binding = self.registry.bind_type(name)?;
        ResolvedType::identified(
            binding
                .identity()
                .ok_or_else(|| Error::Internal("runtime type binding has no identity".into()))?,
            self.registry.identity(),
            definition,
        )
    }
    fn type_entry_if_exists(&self, name: &TypeName) -> Result<Option<ResolvedType>> {
        if self.registry.lookup_type(name)?.is_none() {
            return Ok(None);
        }
        self.type_entry(name).map(Some)
    }
    fn type_by_identity(&self, identity: &ObjectIdentity) -> Result<ResolvedType> {
        let name = self.registry.name(*identity)?.type_name()?;
        ResolvedType::identified(*identity, self.registry.identity(), self.named_type(&name)?)
    }
    fn type_by_identity_if_exists(
        &self,
        identity: &ObjectIdentity,
    ) -> Result<Option<ResolvedType>> {
        let Some(name) = self.registry.name_if_exists(*identity)? else {
            return Ok(None);
        };
        let name = name.type_name()?;
        ResolvedType::identified(*identity, self.registry.identity(), self.named_type(&name)?)
            .map(Some)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn type_key(name: &TypeName) -> String {
    format!("{}:{}{}", name.schema.len(), name.schema, name.name)
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
    fn create_scalar_macro(
        &mut self,
        definition: ScalarMacroDefinition,
        conflict: CreateConflictPolicy,
    ) -> Result<()> {
        definition.validate()?;
        if !self.schemas.contains(&definition.name.schema) {
            return Err(Error::Catalog(format!("schema {} does not exist", definition.name.schema)));
        }
        let key = definition.name.key();
        if self.scalar_macros.contains_key(&key) && !matches!(conflict, CreateConflictPolicy::Replace) {
            return Err(Error::Catalog(format!("scalar macro {} already exists", definition.name)));
        }
        self.scalar_macros.insert(key, definition);
        self.registry.touch()?;
        Ok(())
    }
    fn drop_scalar_macro(&mut self, name: &TableName, if_exists: bool) -> Result<()> {
        if self.scalar_macros.remove(&name.key()).is_none() {
            return if if_exists { Ok(()) } else { Err(Error::Catalog(format!("scalar macro {name} does not exist"))) };
        }
        self.registry.touch()?;
        Ok(())
    }
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
    fn create_view(
        &mut self,
        definition: ViewDefinition,
        conflict: CreateConflictPolicy,
    ) -> Result<bool> {
        let prepared = self.prepare_view_creation(&definition, conflict)?;
        self.apply_view_creation(Arc::new(definition), &prepared)
    }
    fn drop_view(
        &mut self,
        name: &TableName,
        if_exists: bool,
        behavior: DropBehavior,
    ) -> Result<bool> {
        let Some(identity) = self.registry.lookup_view(name)? else {
            return if if_exists {
                Ok(false)
            } else {
                Err(Error::Catalog(format!("view {name} does not exist")))
            };
        };
        let mut registry = self.registry.clone();
        let removed = registry.drop_object(identity, behavior)?;
        for record in &removed {
            if record.name().kind() == CatalogObjectKind::View {
                self.views.remove(&record.name().view_name()?.key());
            }
        }
        self.registry = registry;
        Ok(true)
    }
    fn drop_view_identified(
        &mut self,
        view: &ViewBinding,
        if_exists: bool,
        behavior: DropBehavior,
    ) -> Result<bool> {
        let Some(resolved) = self.resolve_view_binding_if_exists(view)? else {
            return if if_exists {
                Ok(false)
            } else {
                Err(Error::Catalog(format!("view {view} does not exist")))
            };
        };
        self.drop_view(&resolved.definition().name, if_exists, behavior)
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

    fn create_type(
        &mut self,
        definition: TypeDefinition,
        conflict: CreateConflictPolicy,
    ) -> Result<bool> {
        let prepared = self.prepare_type_creation(&definition, conflict)?;
        self.apply_type_creation(definition, &prepared)
    }

    fn drop_type(
        &mut self,
        name: &TypeName,
        if_exists: bool,
        behavior: DropBehavior,
    ) -> Result<bool> {
        let key = type_key(name);
        if !self.named_types.contains_key(&key) {
            return if if_exists {
                Ok(false)
            } else {
                Err(Error::Catalog(format!("type {name} does not exist")))
            };
        }
        let identity = self
            .registry
            .lookup_type(name)?
            .ok_or_else(|| Error::Internal("runtime registry lost named type".into()))?;
        let mut registry = self.registry.clone();
        let removed = registry.drop_object(identity, behavior)?;
        if removed.len() != 1
            || removed[0].identity() != identity
            || removed[0].name().kind() != CatalogObjectKind::Type
        {
            return Err(Error::Internal(
                "named type drop produced an invalid plan".into(),
            ));
        }
        self.named_types.remove(&key);
        self.registry = registry;
        Ok(true)
    }

    fn drop_type_identified(
        &mut self,
        type_: &TypeBinding,
        if_exists: bool,
        behavior: DropBehavior,
    ) -> Result<bool> {
        if type_.identity().is_none() {
            return Err(Error::InvalidInput(
                "runtime snapshot requires an identified type binding".into(),
            ));
        }
        let name = match self.resolve_type_binding_if_exists(type_)? {
            Some(resolved) => resolved.definition().name.clone(),
            None if if_exists => return Ok(false),
            None => return Err(Error::Catalog(format!("type {type_} does not exist"))),
        };
        self.drop_type(&name, if_exists, behavior)
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
    pub(crate) fn prepare_view_creation(
        &self,
        definition: &ViewDefinition,
        conflict: CreateConflictPolicy,
    ) -> Result<PreparedCatalogCreate> {
        if !self.schemas.contains(&definition.name.schema) {
            return Err(Error::Catalog(format!(
                "schema {} does not exist",
                definition.name.schema
            )));
        }
        if definition.names.len() != definition.types.len()
            || definition.aliases.len() > definition.types.len()
        {
            return Err(Error::Catalog("invalid view output metadata".into()));
        }
        self.registry
            .prepare_view_create(&definition.name, conflict)
    }

    pub(crate) fn apply_view_creation(
        &mut self,
        definition: Arc<ViewDefinition>,
        prepared: &PreparedCatalogCreate,
    ) -> Result<bool> {
        if prepared.name() != &CatalogObjectName::view(&definition.name)? {
            return Err(Error::InvalidInput(
                "prepared view creation has a different name".into(),
            ));
        }
        let mut registry = self.registry.clone();
        let changed = registry.apply_prepared_create(prepared)?;
        if !changed {
            return Ok(false);
        }
        self.views.insert(definition.name.key(), definition);
        self.registry = registry;
        Ok(true)
    }

    /// Publish one prepared CREATE VIEW successor into the transaction's two
    /// independent catalog payloads. Equal runtime registries make the
    /// prepared successor deterministic, so deriving it once avoids repeating
    /// registry validation and dependency-graph construction for the basis.
    pub(crate) fn apply_view_creation_pair(
        snapshot: &mut Self,
        basis: &mut Self,
        definition: Arc<ViewDefinition>,
        prepared: &PreparedCatalogCreate,
    ) -> Result<bool> {
        if !snapshot.has_same_runtime_catalog(basis) {
            return Err(Error::Internal(
                "transaction catalog views have different runtime identities".into(),
            ));
        }
        let changed = snapshot.apply_view_creation(definition.clone(), prepared)?;
        if changed {
            basis.views.insert(definition.name.key(), definition);
            basis.registry = snapshot.registry.clone();
        }
        Ok(changed)
    }

    pub(crate) fn prepare_type_creation(
        &self,
        definition: &TypeDefinition,
        conflict: CreateConflictPolicy,
    ) -> Result<PreparedCatalogCreate> {
        definition.validate()?;
        if definition.name != TypeName::new(&definition.name.schema, &definition.name.name) {
            return Err(Error::Catalog("invalid named type identity".into()));
        }
        if !self.schemas.contains(&definition.name.schema) {
            return Err(Error::Catalog(format!(
                "schema {} does not exist",
                definition.name.schema
            )));
        }
        self.registry
            .prepare_type_create(&definition.name, conflict)
    }

    pub(crate) fn apply_type_creation(
        &mut self,
        definition: TypeDefinition,
        prepared: &PreparedCatalogCreate,
    ) -> Result<bool> {
        definition.validate()?;
        if definition.name != TypeName::new(&definition.name.schema, &definition.name.name) {
            return Err(Error::Catalog("invalid named type identity".into()));
        }
        if !self.schemas.contains(&definition.name.schema) {
            return Err(Error::Catalog(format!(
                "schema {} does not exist",
                definition.name.schema
            )));
        }
        if prepared.name() != &CatalogObjectName::named_type(&definition.name)? {
            return Err(Error::InvalidInput(
                "prepared named type creation has a different name".into(),
            ));
        }
        let key = type_key(&definition.name);
        let existed = self.named_types.contains_key(&key);
        let should_exist = !prepared.changes_catalog() || prepared.replaced_identity().is_some();
        if existed != should_exist {
            return Err(Error::Internal(
                "named type payload and runtime registry disagree".into(),
            ));
        }
        let mut registry = self.registry.clone();
        let changed = registry.apply_prepared_create(prepared)?;
        if !changed {
            return Ok(false);
        }
        self.named_types.insert(key, definition);
        self.registry = registry;
        Ok(true)
    }

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
        if self.tables.contains_key(&definition.name.key())
            || self.views.contains_key(&definition.name.key())
        {
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
        if self.tables.contains_key(&definition.name.key())
            || self.views.contains_key(&definition.name.key())
        {
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
            physical_order: PhysicalOrder::ImplicitAppend,
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
        if let Some(data) = self.rows.published_data() {
            if data.columns().len() != self.definition.columns.len() {
                return Err(Error::Constraint("row width differs from table".into()));
            }
            for (column, (definition, data_type)) in data
                .columns()
                .iter()
                .zip(self.definition.columns.iter().zip(&types))
            {
                data_type
                    .validate_vector(column, context)
                    .map_err(|error| match error {
                        Error::Conversion(message) => Error::Constraint(format!(
                            "invalid value for {}: {message}",
                            definition.name
                        )),
                        other => other,
                    })?;
                if !definition.nullable && column.values().any(|value| value.is_null()) {
                    return Err(Error::Constraint(format!(
                        "NOT NULL constraint failed: {}.{}",
                        self.definition.name, definition.name
                    )));
                }
            }
            return Ok(());
        }
        let mut validated = (0..types.len()).map(|_| HashSet::new()).collect::<Vec<_>>();
        for row in self.rows.values() {
            context.check()?;
            if row.len() != self.definition.columns.len() {
                return Err(Error::Constraint("row width differs from table".into()));
            }
            for (index, ((value, column), data_type)) in row
                .iter()
                .zip(&self.definition.columns)
                .zip(&types)
                .enumerate()
            {
                if shared_value_identity(&value)
                    .is_none_or(|identity| validated[index].insert(identity))
                {
                    data_type
                        .validate(&value, context)
                        .map_err(|error| match error {
                            Error::Conversion(message) => Error::Constraint(format!(
                                "invalid value for {}: {message}",
                                column.name
                            )),
                            other => other,
                        })?;
                }
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
            let mut entries = self.rows.iter().map(|(&id, row)| {
                (
                    id,
                    key.columns
                        .iter()
                        .map(|&i| row.get(i).expect("validated row key"))
                        .collect(),
                )
            });
            indexes.push(factory.build(spec, &mut entries, context)?);
        }
        Ok(indexes)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Immutable Arc-backed payloads can share one logical-validation result while
/// every row still receives its own NULL/constraint checks.
fn shared_value_identity(value: &Value) -> Option<(u8, usize)> {
    match value {
        Value::Bit(value) => Some((0, Arc::as_ptr(value) as usize)),
        Value::Bignum(value) => Some((1, Arc::as_ptr(value) as usize)),
        Value::Enum(value) => Some((2, Arc::as_ptr(value) as usize)),
        Value::Nested(value) => Some((3, Arc::as_ptr(value) as usize)),
        Value::Extension(value) => Some((4, Arc::as_ptr(value) as usize)),
        _ => None,
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
        match &table.physical_order {
            PhysicalOrder::ImplicitAppend => {
                Ok(Box::new(table.rows.scan_implicit_append(table.next_id)?))
            }
            PhysicalOrder::Explicit(slots) => {
                let order = slots
                    .iter()
                    .filter_map(|slot| slot.live())
                    .collect::<Vec<_>>();
                Ok(Box::new(table.rows.scan_ordered(&order)?))
            }
        }
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
            if id > PHYSICAL_ROW_ID_MAX {
                return Err(Error::Resource("physical row identity exhausted".into()));
            }
            next.next_id = id
                .checked_add(1)
                .ok_or_else(|| Error::Resource("row identity exhausted".into()))?;
            next.rows.insert(id, row);
            next.physical_order.append(id, id)?;
        }
        next.validate(
            self.indexes.as_ref(),
            &context.clone().with_types(self.types.clone()),
        )?;
        self.tables.insert(table.key(), Arc::new(next));
        Ok(count)
    }
    fn insert_chunks(
        &mut self,
        table: &TableName,
        chunks: Vec<DataChunk>,
        context: &QueryContext,
    ) -> Result<usize> {
        let count = chunks.iter().try_fold(0usize, |count, chunk| {
            count
                .checked_add(chunk.len())
                .ok_or_else(|| Error::Resource("table row count overflow".into()))
        })?;
        context.check_rows(self.get(table)?.rows.len().saturating_add(count))?;
        if count == 0 {
            return Ok(0);
        }
        if !self.get(table)?.rows.is_empty() {
            let mut rows = Vec::with_capacity(count);
            for chunk in chunks {
                rows.extend(chunk.rows());
            }
            return self.insert(table, rows, context);
        }
        let mut next = self.get(table)?.clone();
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            context.check()?;
            let id = next.next_id;
            if id > PHYSICAL_ROW_ID_MAX {
                return Err(Error::Resource("physical row identity exhausted".into()));
            }
            next.next_id = id
                .checked_add(1)
                .ok_or_else(|| Error::Resource("row identity exhausted".into()))?;
            ids.push(id);
            next.physical_order.append(id, id)?;
        }
        let types = next
            .definition
            .columns
            .iter()
            .map(|column| column.data_type.clone())
            .collect::<Arc<[_]>>();
        next.rows = Rows::from_chunks(ids, types, chunks, context)?;
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
                    .physical_order
                    .materialize(next.next_id)
                    .iter_mut()
                    .find(|slot| slot.live() == Some(*id))
                    .ok_or_else(|| {
                        Error::Internal("updated row has no live physical slot".into())
                    })?;
                *slot = PhysicalSlot::deleted(*id)?;
                relocated.push(PhysicalSlot::present(*id)?);
            }
            next.physical_order
                .materialize(next.next_id)
                .extend(relocated);
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
                    .physical_order
                    .materialize(next.next_id)
                    .iter_mut()
                    .find(|slot| slot.live() == Some(*id))
                    .ok_or_else(|| Error::Internal("live row has no physical slot".into()))?;
                *slot = PhysicalSlot::deleted(*id)?;
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

#[cfg(test)]
mod paired_view_creation_tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn definition(name: &str, query: &str) -> ViewDefinition {
        ViewDefinition {
            name: TableName::new("main", name),
            query: query.into(),
            aliases: vec![],
            names: vec![],
            types: vec![],
            query_shape: None,
            dependencies: vec![],
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn apply_pair(
        snapshot: &mut Snapshot,
        basis: &mut Snapshot,
        definition: Arc<ViewDefinition>,
        conflict: CreateConflictPolicy,
    ) -> Result<bool> {
        let prepared = snapshot.prepare_view_creation(&definition, conflict)?;
        Snapshot::apply_view_creation_pair(snapshot, basis, definition, &prepared)
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn paired_view_creation_shares_one_successor_and_replacement_identity() -> Result<()> {
        let mut snapshot = Snapshot::default();
        let mut basis = snapshot.clone();
        let first = Arc::new(definition("v", "SELECT 1"));
        assert!(apply_pair(
            &mut snapshot,
            &mut basis,
            first.clone(),
            CreateConflictPolicy::Error,
        )?);
        let name = TableName::new("main", "v");
        let first_identity = snapshot.registry.lookup_view(&name)?.unwrap();
        assert_eq!(Some(first_identity), basis.registry.lookup_view(&name)?);
        assert!(Arc::ptr_eq(
            snapshot.views.get(&name.key()).unwrap(),
            basis.views.get(&name.key()).unwrap(),
        ));

        let replacement = Arc::new(definition("v", "SELECT 2"));
        assert!(apply_pair(
            &mut snapshot,
            &mut basis,
            replacement.clone(),
            CreateConflictPolicy::Replace,
        )?);
        let replacement_identity = snapshot.registry.lookup_view(&name)?.unwrap();
        assert_ne!(first_identity, replacement_identity);
        assert_eq!(
            Some(replacement_identity),
            basis.registry.lookup_view(&name)?
        );
        assert!(Arc::ptr_eq(
            snapshot.views.get(&name.key()).unwrap(),
            basis.views.get(&name.key()).unwrap(),
        ));
        assert_eq!(snapshot.registry, basis.registry);
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn paired_view_creation_ignore_and_failed_preparations_leave_candidates_unchanged() -> Result<()>
    {
        let mut snapshot = Snapshot::default();
        let mut basis = snapshot.clone();
        let initial = Arc::new(definition("v", "SELECT 1"));
        assert!(apply_pair(
            &mut snapshot,
            &mut basis,
            initial,
            CreateConflictPolicy::Error,
        )?);
        let before_snapshot = snapshot.clone();
        let before_basis = basis.clone();
        assert!(!apply_pair(
            &mut snapshot,
            &mut basis,
            Arc::new(definition("v", "SELECT 2")),
            CreateConflictPolicy::Ignore,
        )?);
        assert_eq!(snapshot.registry, before_snapshot.registry);
        assert_eq!(basis.registry, before_basis.registry);
        assert_eq!(snapshot.views, before_snapshot.views);
        assert_eq!(basis.views, before_basis.views);

        let stale = snapshot.prepare_view_creation(
            &definition("stale", "SELECT 3"),
            CreateConflictPolicy::Error,
        )?;
        assert!(apply_pair(
            &mut snapshot,
            &mut basis,
            Arc::new(definition("next", "SELECT 4")),
            CreateConflictPolicy::Error,
        )?);
        let after_success_snapshot = snapshot.clone();
        let after_success_basis = basis.clone();
        assert!(
            Snapshot::apply_view_creation_pair(
                &mut snapshot,
                &mut basis,
                Arc::new(definition("stale", "SELECT 3")),
                &stale,
            )
            .is_err()
        );
        assert_eq!(snapshot.registry, after_success_snapshot.registry);
        assert_eq!(basis.registry, after_success_basis.registry);
        assert_eq!(snapshot.views, after_success_snapshot.views);
        assert_eq!(basis.views, after_success_basis.views);

        let foreign = Snapshot::default();
        let foreign_definition = definition("foreign", "SELECT 5");
        let foreign_prepared =
            foreign.prepare_view_creation(&foreign_definition, CreateConflictPolicy::Error)?;
        assert!(
            Snapshot::apply_view_creation_pair(
                &mut snapshot,
                &mut basis,
                Arc::new(foreign_definition),
                &foreign_prepared,
            )
            .is_err()
        );
        assert_eq!(snapshot.registry, after_success_snapshot.registry);
        assert_eq!(basis.registry, after_success_basis.registry);
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn paired_view_creation_rejects_unequal_runtime_catalogs_before_mutation() -> Result<()> {
        let mut snapshot = Snapshot::default();
        let mut basis = snapshot.clone();
        let isolated = definition("only_snapshot", "SELECT 1");
        let isolated_prepared =
            snapshot.prepare_view_creation(&isolated, CreateConflictPolicy::Error)?;
        assert!(snapshot.apply_view_creation(Arc::new(isolated), &isolated_prepared)?);
        let prepared = snapshot.prepare_view_creation(
            &definition("should_not_publish", "SELECT 2"),
            CreateConflictPolicy::Error,
        )?;
        let before_snapshot = snapshot.clone();
        let before_basis = basis.clone();
        assert!(
            Snapshot::apply_view_creation_pair(
                &mut snapshot,
                &mut basis,
                Arc::new(definition("should_not_publish", "SELECT 2")),
                &prepared,
            )
            .is_err()
        );
        assert_eq!(snapshot.registry, before_snapshot.registry);
        assert_eq!(basis.registry, before_basis.registry);
        assert_eq!(snapshot.views, before_snapshot.views);
        assert_eq!(basis.views, before_basis.views);
        Ok(())
    }
}
