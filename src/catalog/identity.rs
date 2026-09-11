use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::common::{Error, Result};

use super::{TableDefinition, TableName};

// DuckDB starts runtime OIDs above PostgreSQL's built-in object range. A
// single namespace prevents catalog and object handles from ever aliasing by
// accident when their raw values cross an API boundary.
const FIRST_RUNTIME_ID: u64 = 20_000;
static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(FIRST_RUNTIME_ID);

fn allocate_runtime_id(counter: &AtomicU64) -> Result<NonZeroU64> {
    let value = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| Error::Resource("runtime catalog identity space exhausted".into()))?;
    NonZeroU64::new(value)
        .ok_or_else(|| Error::Internal("runtime catalog identity allocator reached zero".into()))
}

macro_rules! runtime_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Allocates a process-unique runtime handle. Runtime handles are
            /// intentionally absent from snapshot and WAL serialization.
            pub fn allocate() -> Result<Self> {
                allocate_runtime_id(&NEXT_RUNTIME_ID).map(Self)
            }

            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

runtime_id!(CatalogId);
runtime_id!(ObjectId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogVersion(u64);

impl CatalogVersion {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_next(self) -> Result<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| Error::Resource("catalog version space exhausted".into()))
    }
}

impl fmt::Display for CatalogVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogIdentity {
    pub id: CatalogId,
    pub version: Option<CatalogVersion>,
}

impl CatalogIdentity {
    pub const fn new(id: CatalogId, version: Option<CatalogVersion>) -> Self {
        Self { id, version }
    }

    pub const fn unversioned(id: CatalogId) -> Self {
        Self::new(id, None)
    }
}

impl fmt::Display for CatalogIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.version {
            Some(version) => write!(f, "{}@{version}", self.id),
            None => write!(f, "{}@unversioned", self.id),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogObjectKind {
    Schema,
    Table,
}

impl fmt::Display for CatalogObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Schema => f.write_str("schema"),
            Self::Table => f.write_str("table"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectIdentity {
    pub catalog: CatalogId,
    pub object: ObjectId,
    pub kind: CatalogObjectKind,
}

impl ObjectIdentity {
    pub const fn new(catalog: CatalogId, object: ObjectId, kind: CatalogObjectKind) -> Self {
        Self {
            catalog,
            object,
            kind,
        }
    }
}

impl fmt::Display for ObjectIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.kind, self.catalog, self.object)
    }
}

/// A transaction-resolved table reference. Private fields keep a non-table
/// object identity from being smuggled into table APIs.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TableBinding {
    name: TableName,
    identity: Option<ObjectIdentity>,
    /// Version observed while resolving this binding. It invalidates cached
    /// resolution without changing the stable object key.
    catalog_version: Option<CatalogVersion>,
}

impl TableBinding {
    pub fn unversioned(name: TableName) -> Self {
        Self {
            name,
            identity: None,
            catalog_version: None,
        }
    }

    pub fn identified(
        name: TableName,
        identity: ObjectIdentity,
        catalog: CatalogIdentity,
    ) -> Result<Self> {
        if identity.kind != CatalogObjectKind::Table {
            return Err(Error::InvalidInput(format!(
                "table binding requires a table identity, got {}",
                identity.kind
            )));
        }
        if identity.catalog != catalog.id {
            return Err(Error::InvalidInput(
                "table identity and resolved catalog identity do not match".into(),
            ));
        }
        Ok(Self {
            name,
            identity: Some(identity),
            catalog_version: catalog.version,
        })
    }

    pub const fn name(&self) -> &TableName {
        &self.name
    }

    pub const fn identity(&self) -> Option<ObjectIdentity> {
        self.identity
    }

    pub const fn catalog_version(&self) -> Option<CatalogVersion> {
        self.catalog_version
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Debug for TableBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.name, f)
    }
}

impl fmt::Display for TableBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(f)
    }
}

/// A definition paired with the exact binding used to resolve it.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTable {
    binding: TableBinding,
    definition: TableDefinition,
}

impl ResolvedTable {
    pub fn unversioned(definition: TableDefinition) -> Self {
        Self {
            binding: TableBinding::unversioned(definition.name.clone()),
            definition,
        }
    }

    pub fn identified(
        identity: ObjectIdentity,
        catalog: CatalogIdentity,
        definition: TableDefinition,
    ) -> Result<Self> {
        Ok(Self {
            binding: TableBinding::identified(definition.name.clone(), identity, catalog)?,
            definition,
        })
    }

    pub const fn binding(&self) -> &TableBinding {
        &self.binding
    }

    pub const fn definition(&self) -> &TableDefinition {
        &self.definition
    }

    pub fn into_parts(self) -> (TableBinding, TableDefinition) {
        (self.binding, self.definition)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DropBehavior {
    Restrict,
    Cascade,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::thread;

    use super::*;
    use crate::catalog::{Catalog, CatalogMut, ColumnDefinition, TableAlteration, UniqueKey};
    use crate::common::DataType;

    fn definition(name: &str) -> TableDefinition {
        TableDefinition {
            name: TableName::main(name),
            columns: vec![ColumnDefinition::new("i", DataType::Integer)],
            unique_keys: vec![UniqueKey {
                columns: vec![0],
                primary: true,
            }],
        }
    }

    fn table_identity(kind: CatalogObjectKind) -> ObjectIdentity {
        let catalog =
            CatalogIdentity::new(CatalogId::allocate().unwrap(), Some(CatalogVersion::new(7)));
        ObjectIdentity::new(catalog.id, ObjectId::allocate().unwrap(), kind)
    }

    #[test]
    fn runtime_ids_are_nonzero_unique_and_share_one_namespace() {
        let threads = (0..4)
            .map(|_| {
                thread::spawn(|| {
                    (0..128)
                        .flat_map(|_| {
                            [
                                CatalogId::allocate().unwrap().get(),
                                ObjectId::allocate().unwrap().get(),
                            ]
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let ids = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert!(ids.iter().all(|id| *id != 0));
        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len());
    }

    #[test]
    fn allocator_and_catalog_versions_reject_overflow() {
        let exhausted = AtomicU64::new(u64::MAX);
        assert!(matches!(
            allocate_runtime_id(&exhausted),
            Err(Error::Resource(_))
        ));
        assert_eq!(exhausted.load(Ordering::Relaxed), u64::MAX);
        assert!(matches!(
            CatalogVersion::new(u64::MAX).checked_next(),
            Err(Error::Resource(_))
        ));
    }

    #[test]
    fn handles_validate_kind_and_have_stable_diagnostics() {
        let name = TableName::new("Analytics", "Events");
        let catalog =
            CatalogIdentity::new(CatalogId::allocate().unwrap(), Some(CatalogVersion::new(7)));
        let identity = ObjectIdentity::new(
            catalog.id,
            ObjectId::allocate().unwrap(),
            CatalogObjectKind::Table,
        );
        let binding = TableBinding::identified(name.clone(), identity, catalog).unwrap();
        assert_eq!(binding, binding.clone());
        assert_eq!(binding.name(), &name);
        assert_eq!(binding.identity(), Some(identity));
        assert_eq!(binding.catalog_version(), catalog.version);
        assert_eq!(binding.to_string(), "analytics.events");
        assert_eq!(
            identity.to_string(),
            format!("table:{}:{}", identity.catalog, identity.object)
        );

        let schema = table_identity(CatalogObjectKind::Schema);
        assert!(matches!(
            TableBinding::identified(name, schema, catalog),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            ResolvedTable::identified(schema, catalog, definition("events")),
            Err(Error::InvalidInput(_))
        ));

        let other_catalog = CatalogIdentity::unversioned(CatalogId::allocate().unwrap());
        assert!(matches!(
            TableBinding::identified(TableName::main("events"), identity, other_catalog),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn object_identity_survives_catalog_version_changes() {
        let catalog_id = CatalogId::allocate().unwrap();
        let object_id = ObjectId::allocate().unwrap();
        let old_catalog = CatalogIdentity::new(catalog_id, Some(CatalogVersion::new(7)));
        let new_catalog = CatalogIdentity::new(catalog_id, Some(CatalogVersion::new(8)));
        let old_identity = ObjectIdentity::new(old_catalog.id, object_id, CatalogObjectKind::Table);
        let new_identity = ObjectIdentity::new(new_catalog.id, object_id, CatalogObjectKind::Table);
        let name = TableName::main("items");
        let old = TableBinding::identified(name.clone(), old_identity, old_catalog).unwrap();
        let new = TableBinding::identified(name, new_identity, new_catalog).unwrap();

        assert_eq!(old_identity, new_identity);
        assert_eq!(old.identity(), new.identity());
        assert_ne!(old.catalog_version(), new.catalog_version());
        assert_ne!(old, new);
    }

    #[derive(Clone)]
    struct LegacyCatalog {
        table: TableDefinition,
        identity: Option<CatalogIdentity>,
        drops: usize,
        alters: usize,
    }

    impl Catalog for LegacyCatalog {
        fn identity(&self) -> Option<CatalogIdentity> {
            self.identity
        }

        fn schemas(&self) -> Result<Vec<String>> {
            Ok(vec!["main".into()])
        }

        fn table(&self, name: &TableName) -> Result<TableDefinition> {
            assert_eq!(name, &self.table.name);
            Ok(self.table.clone())
        }

        fn tables(&self) -> Result<Vec<TableDefinition>> {
            Ok(vec![self.table.clone()])
        }
    }

    impl CatalogMut for LegacyCatalog {
        fn create_schema(&mut self, _name: &str, _if_not_exists: bool) -> Result<()> {
            Ok(())
        }

        fn drop_schema(&mut self, _name: &str, _if_exists: bool) -> Result<()> {
            Ok(())
        }

        fn create_table(
            &mut self,
            _definition: TableDefinition,
            _if_not_exists: bool,
        ) -> Result<()> {
            Ok(())
        }

        fn drop_table(&mut self, name: &TableName, _if_exists: bool) -> Result<()> {
            assert_eq!(name, &self.table.name);
            self.drops += 1;
            Ok(())
        }

        fn alter_table(
            &mut self,
            name: &TableName,
            _alteration: &TableAlteration,
            _context: &crate::parallel::QueryContext,
        ) -> Result<bool> {
            assert_eq!(name, &self.table.name);
            self.alters += 1;
            Ok(true)
        }
    }

    #[test]
    fn legacy_catalog_defaults_are_explicitly_unversioned() {
        let mut catalog = LegacyCatalog {
            table: definition("items"),
            identity: None,
            drops: 0,
            alters: 0,
        };
        assert_eq!(catalog.identity(), None);

        let resolved = catalog.table_entry(&TableName::main("items")).unwrap();
        assert_eq!(resolved.definition(), &catalog.table);
        assert_eq!(resolved.binding().identity(), None);
        assert_eq!(
            catalog
                .table_entry_if_exists(&TableName::main("items"))
                .unwrap(),
            Some(resolved.clone())
        );
        assert_eq!(
            catalog.resolve_table_binding(resolved.binding()).unwrap(),
            resolved
        );
        catalog.current_table_binding(resolved.binding()).unwrap();
        catalog
            .drop_table_identified(resolved.binding(), false)
            .unwrap();
        assert_eq!(catalog.drops, 1);

        let identity = table_identity(CatalogObjectKind::Table);
        assert!(matches!(
            catalog.table_by_identity(&identity),
            Err(Error::Unsupported(_))
        ));
        let identified = TableBinding::identified(
            TableName::main("items"),
            identity,
            CatalogIdentity::unversioned(identity.catalog),
        )
        .unwrap();
        assert!(matches!(
            catalog.drop_table_identified(&identified, false),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(catalog.drops, 1);
    }

    #[test]
    fn identity_advertising_adapter_must_override_entry_and_mutation_paths() {
        let identity =
            CatalogIdentity::new(CatalogId::allocate().unwrap(), Some(CatalogVersion::new(3)));
        let mut catalog = LegacyCatalog {
            table: definition("items"),
            identity: Some(identity),
            drops: 0,
            alters: 0,
        };
        let binding = TableBinding::unversioned(TableName::main("items"));

        assert!(matches!(
            catalog.table_entry(binding.name()),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            catalog.table_entry_if_exists(binding.name()),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            catalog.resolve_table_binding(&binding),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            catalog.current_table_binding(&binding),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            catalog.drop_table_identified(&binding, false),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            catalog.alter_table_identified(
                &binding,
                &TableAlteration::RenameTable("renamed".into()),
                &crate::parallel::QueryContext::background()
            ),
            Err(Error::Unsupported(_))
        ));
        assert_eq!((catalog.drops, catalog.alters), (0, 0));
    }

    #[test]
    fn runtime_identity_does_not_change_table_definition_wire_shape() {
        let value = serde_json::to_value(definition("items")).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(
            object.keys().map(String::as_str).collect::<HashSet<_>>(),
            HashSet::from(["name", "columns", "unique_keys"])
        );
        assert!(!value.to_string().contains("identity"));
    }

    // Compile-time evidence that handles remain usable across threads without
    // gaining a persistence contract.
    #[test]
    fn handles_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<TableBinding>>();
    }
}
