//! Runtime catalog name, identity, version and dependency coordination.
//!
//! This registry is deliberately not serializable. Durable definitions own
//! names and payloads; reopening rebuilds fresh process-local identities.

use std::collections::{BTreeMap, BTreeSet};

use crate::common::{Error, Result};

use super::{
    CatalogId, CatalogIdentity, CatalogObjectKind, CatalogVersion, DependencyGraph, DependentFlags,
    DropBehavior, ObjectId, ObjectIdentity, SubjectFlags, TableBinding, TableName,
};

const MAX_CATALOG_OBJECTS: usize = 1_000_000;
const MAX_IDENTIFIER_BYTES: usize = 4_096;

/// Canonical name inside one catalog. Schemas occupy the catalog namespace;
/// tables occupy a schema namespace. Future object families must explicitly
/// choose their reference-compatible namespace rather than aliasing this key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogObjectName {
    kind: CatalogObjectKind,
    schema: Option<String>,
    name: String,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CatalogObjectName {
    pub fn schema(name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            kind: CatalogObjectKind::Schema,
            schema: None,
            name: canonical_identifier(name.into())?,
        })
    }

    pub fn table(name: &TableName) -> Result<Self> {
        Ok(Self {
            kind: CatalogObjectKind::Table,
            schema: Some(canonical_identifier(name.schema.clone())?),
            name: canonical_identifier(name.name.clone())?,
        })
    }

    pub const fn kind(&self) -> CatalogObjectKind {
        self.kind
    }

    pub fn schema_name(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn table_name(&self) -> Result<TableName> {
        if self.kind != CatalogObjectKind::Table {
            return Err(Error::InvalidInput(format!(
                "{} is not a table catalog name",
                self.kind
            )));
        }
        Ok(TableName::new(
            self.schema
                .as_deref()
                .ok_or_else(|| Error::Internal("table registry key has no schema".into()))?,
            &self.name,
        ))
    }
}

/// An entry returned by a checked registry mutation. Payload owners use this
/// record to apply the same identity-ordered plan to their own state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogObjectRecord {
    identity: ObjectIdentity,
    name: CatalogObjectName,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CatalogObjectRecord {
    pub const fn identity(&self) -> ObjectIdentity {
        self.identity
    }

    pub const fn name(&self) -> &CatalogObjectName {
        &self.name
    }
}

/// One-shot insertion plan tied to the exact catalog version that allocated
/// it. Cloned transaction views at that version may both apply the plan; a
/// replay or application after any divergent catalog change is rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedCatalogInsert {
    record: CatalogObjectRecord,
    basis: CatalogVersion,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PreparedCatalogInsert {
    pub const fn identity(&self) -> ObjectIdentity {
        self.record.identity
    }

    pub const fn name(&self) -> &CatalogObjectName {
        &self.record.name
    }
}

/// Cloneable transaction-local runtime catalog metadata. Every successful
/// mutation advances the catalog version once. Failed mutations leave all
/// name, identity, dependency and version state unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogRegistry {
    catalog: CatalogId,
    version: CatalogVersion,
    identities_by_name: BTreeMap<CatalogObjectName, ObjectIdentity>,
    names_by_identity: BTreeMap<ObjectIdentity, CatalogObjectName>,
    dependencies: DependencyGraph,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CatalogRegistry {
    pub fn new() -> Result<Self> {
        Ok(Self {
            catalog: CatalogId::allocate()?,
            version: CatalogVersion::new(0),
            identities_by_name: BTreeMap::new(),
            names_by_identity: BTreeMap::new(),
            dependencies: DependencyGraph::new(),
        })
    }

    /// Rebuild fresh runtime identities from durable names without pretending
    /// those process-local handles were persisted. Input is fully validated
    /// before the new registry is returned.
    pub fn rebuild(
        schemas: impl IntoIterator<Item = String>,
        tables: impl IntoIterator<Item = TableName>,
    ) -> Result<Self> {
        let mut registry = Self::new()?;
        let mut input_count = 0usize;
        let mut schema_names = BTreeSet::new();
        for schema in schemas {
            input_count = input_count
                .checked_add(1)
                .ok_or_else(|| Error::Resource("catalog object count overflow".into()))?;
            if input_count > MAX_CATALOG_OBJECTS {
                return Err(Error::Resource("catalog object limit exceeded".into()));
            }
            let name = CatalogObjectName::schema(schema)?;
            if !schema_names.insert(name) {
                return Err(Error::Corrupt("duplicate durable schema name".into()));
            }
        }
        let mut table_names = BTreeSet::new();
        for table in tables {
            input_count = input_count
                .checked_add(1)
                .ok_or_else(|| Error::Resource("catalog object count overflow".into()))?;
            if input_count > MAX_CATALOG_OBJECTS {
                return Err(Error::Resource("catalog object limit exceeded".into()));
            }
            let name = CatalogObjectName::table(&table)?;
            if !table_names.insert(name) {
                return Err(Error::Corrupt("duplicate durable table name".into()));
            }
        }
        let count = schema_names
            .len()
            .checked_add(table_names.len())
            .ok_or_else(|| Error::Resource("catalog object count overflow".into()))?;
        if count > MAX_CATALOG_OBJECTS {
            return Err(Error::Resource("catalog object limit exceeded".into()));
        }
        for name in schema_names.into_iter().chain(table_names) {
            registry.insert_without_version(name)?;
        }
        registry.validate()?;
        Ok(registry)
    }

    pub const fn identity(&self) -> CatalogIdentity {
        CatalogIdentity::new(self.catalog, Some(self.version))
    }

    pub fn len(&self) -> usize {
        self.identities_by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.identities_by_name.is_empty()
    }

    pub fn lookup(&self, name: &CatalogObjectName) -> Option<ObjectIdentity> {
        self.identities_by_name.get(name).copied()
    }

    pub fn lookup_schema(&self, name: &str) -> Result<Option<ObjectIdentity>> {
        Ok(self.lookup(&CatalogObjectName::schema(name)?))
    }

    pub fn lookup_table(&self, name: &TableName) -> Result<Option<ObjectIdentity>> {
        Ok(self.lookup(&CatalogObjectName::table(name)?))
    }

    pub fn name(&self, identity: ObjectIdentity) -> Result<&CatalogObjectName> {
        self.ensure_local(identity)?;
        self.names_by_identity
            .get(&identity)
            .ok_or_else(|| Error::Catalog(format!("catalog object {identity} no longer exists")))
    }

    pub fn name_if_exists(&self, identity: ObjectIdentity) -> Result<Option<&CatalogObjectName>> {
        self.ensure_local(identity)?;
        Ok(self.names_by_identity.get(&identity))
    }

    pub fn bind_table(&self, name: &TableName) -> Result<TableBinding> {
        let name = CatalogObjectName::table(name)?;
        let identity = self
            .lookup(&name)
            .ok_or_else(|| Error::Catalog(format!("table {} does not exist", name.name)))?;
        TableBinding::identified(name.table_name()?, identity, self.identity())
    }

    /// True when a cached binding observed the current complete catalog
    /// version. False requires re-resolution, but does not imply the stable
    /// object itself was dropped or replaced.
    pub fn binding_is_current(&self, binding: &super::TableBinding) -> bool {
        let Some(identity) = binding.identity() else {
            return false;
        };
        identity.catalog == self.catalog
            && binding.catalog_version() == Some(self.version)
            && self
                .names_by_identity
                .get(&identity)
                .and_then(|name| name.table_name().ok())
                .is_some_and(|name| &name == binding.name())
    }

    /// Resolve a stable binding to its current name. Rename preserves identity;
    /// drop/recreate under the old name does not.
    pub fn table_name_for_binding(&self, binding: &super::TableBinding) -> Result<TableName> {
        let identity = binding.identity().ok_or_else(|| {
            Error::InvalidInput("runtime registry requires an identified table binding".into())
        })?;
        self.name(identity)?.table_name()
    }

    pub fn insert(&mut self, name: CatalogObjectName) -> Result<ObjectIdentity> {
        let prepared = self.prepare_insert(name)?;
        let identity = prepared.identity();
        self.insert_prepared(&prepared)?;
        Ok(identity)
    }

    /// Allocate one identity for a catalog insertion without mutating state.
    /// Multiple transaction-local payload views can apply this same record and
    /// therefore cannot assign different identities to the same object.
    pub fn prepare_insert(&self, name: CatalogObjectName) -> Result<PreparedCatalogInsert> {
        self.validate()?;
        self.insert_schema(&name)?;
        if self.identities_by_name.contains_key(&name) {
            return Err(Error::Catalog(format!(
                "{} {} already exists",
                name.kind, name.name
            )));
        }
        Ok(PreparedCatalogInsert {
            record: CatalogObjectRecord {
                identity: ObjectIdentity::new(self.catalog, ObjectId::allocate()?, name.kind),
                name,
            },
            basis: self.version,
        })
    }

    /// Apply a previously allocated insertion to this catalog lineage.
    /// Failure, including a name or identity collision, is atomic.
    pub fn insert_prepared(&mut self, prepared: &PreparedCatalogInsert) -> Result<()> {
        self.validate()?;
        self.ensure_local(prepared.record.identity)?;
        if prepared.basis != self.version {
            return Err(Error::Catalog(
                "prepared catalog insertion observed a stale catalog version".into(),
            ));
        }
        if prepared.record.identity.kind != prepared.record.name.kind {
            return Err(Error::InvalidInput(
                "prepared catalog identity kind differs from its name".into(),
            ));
        }
        let mut candidate = self.clone();
        candidate.insert_identity_without_version(prepared.record.clone())?;
        candidate.advance_version()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn rename(
        &mut self,
        identity: ObjectIdentity,
        replacement: CatalogObjectName,
    ) -> Result<()> {
        self.validate()?;
        self.ensure_local(identity)?;
        if identity.kind != replacement.kind {
            return Err(Error::InvalidInput(format!(
                "cannot rename {} identity as {}",
                identity.kind, replacement.kind
            )));
        }
        let current = self.name(identity)?;
        if current == &replacement {
            return Ok(());
        }
        if identity.kind == CatalogObjectKind::Table && current.schema != replacement.schema {
            return Err(Error::InvalidInput(
                "table rename cannot move an object between schemas".into(),
            ));
        }
        self.dependencies.ensure_can_alter(identity)?;
        if self.identities_by_name.contains_key(&replacement) {
            return Err(Error::Catalog(format!(
                "{} {} already exists",
                replacement.kind, replacement.name
            )));
        }
        let mut candidate = self.clone();
        let old = candidate.replace_name(identity, replacement.clone())?;
        if identity.kind == CatalogObjectKind::Schema {
            let children = candidate
                .names_by_identity
                .iter()
                .filter_map(|(child, name)| {
                    (name.kind == CatalogObjectKind::Table
                        && name.schema.as_deref() == Some(old.name.as_str()))
                    .then_some((*child, name.clone()))
                })
                .collect::<Vec<_>>();
            for (child, mut name) in children {
                name.schema = Some(replacement.name.clone());
                candidate.replace_name(child, name)?;
            }
        }
        candidate.advance_version()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Record a metadata alteration that leaves the object's name and stable
    /// identity intact. Dependency blockers are checked before the version is
    /// advanced.
    pub fn alter(&mut self, identity: ObjectIdentity) -> Result<()> {
        self.validate()?;
        self.name(identity)?;
        self.dependencies.ensure_can_alter(identity)?;
        let mut candidate = self.clone();
        candidate.advance_version()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn add_dependency(
        &mut self,
        dependent: ObjectIdentity,
        subject: ObjectIdentity,
        dependent_flags: DependentFlags,
        subject_flags: SubjectFlags,
    ) -> Result<()> {
        self.validate()?;
        self.name(dependent)?;
        self.name(subject)?;
        let mut candidate = self.clone();
        let before = candidate.dependencies.clone();
        candidate.dependencies.add_dependency(
            dependent,
            subject,
            dependent_flags,
            subject_flags,
        )?;
        if candidate.dependencies != before {
            candidate.advance_version()?;
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Atomically remove a checked dependency closure and return its
    /// dependent-before-subject records. A blocked or cyclic plan changes
    /// nothing.
    pub fn drop_object(
        &mut self,
        root: ObjectIdentity,
        behavior: DropBehavior,
    ) -> Result<Vec<CatalogObjectRecord>> {
        self.validate()?;
        self.name(root)?;
        let plan = self.dependencies.plan_drop(root, behavior)?;
        let mut candidate = self.clone();
        let mut records = Vec::new();
        records
            .try_reserve_exact(plan.len())
            .map_err(|_| Error::Resource("catalog drop plan allocation".into()))?;
        for identity in plan {
            let name = candidate
                .names_by_identity
                .remove(&identity)
                .ok_or_else(|| Error::Catalog(format!("catalog object {identity} is missing")))?;
            if candidate.identities_by_name.remove(&name) != Some(identity) {
                return Err(Error::Internal(
                    "catalog name indexes disagree during drop".into(),
                ));
            }
            candidate.dependencies.remove_object(identity)?;
            records.push(CatalogObjectRecord { identity, name });
        }
        candidate.advance_version()?;
        candidate.validate()?;
        *self = candidate;
        Ok(records)
    }

    pub fn validate(&self) -> Result<()> {
        if self.identities_by_name.len() != self.names_by_identity.len() {
            return Err(Error::Internal(
                "catalog name indexes have different entry counts".into(),
            ));
        }
        if self.identities_by_name.len() > MAX_CATALOG_OBJECTS {
            return Err(Error::Resource("catalog object limit exceeded".into()));
        }
        for (name, identity) in &self.identities_by_name {
            if identity.catalog != self.catalog || identity.kind != name.kind {
                return Err(Error::Internal(
                    "catalog name index has invalid identity".into(),
                ));
            }
            if self.names_by_identity.get(identity) != Some(name) {
                return Err(Error::Internal(
                    "catalog reverse name index disagrees".into(),
                ));
            }
            if name.kind == CatalogObjectKind::Table {
                let schema = name
                    .schema
                    .as_deref()
                    .ok_or_else(|| Error::Internal("table registry name has no schema".into()))?;
                let schema = self
                    .lookup(&CatalogObjectName::schema(schema)?)
                    .ok_or_else(|| Error::Internal("table registry schema is missing".into()))?;
                if self.dependencies.dependency_flags(*identity, schema)?
                    != Some((DependentFlags::blocking(), SubjectFlags::ordinary()))
                {
                    return Err(Error::Internal(
                        "table registry schema dependency is missing or invalid".into(),
                    ));
                }
            }
        }
        for (identity, name) in &self.names_by_identity {
            if self.identities_by_name.get(name) != Some(identity) {
                return Err(Error::Internal(
                    "catalog forward name index disagrees".into(),
                ));
            }
        }
        self.dependencies
            .validate_object_set(|identity| self.names_by_identity.contains_key(&identity))
    }

    fn insert_without_version(&mut self, name: CatalogObjectName) -> Result<ObjectIdentity> {
        let identity = ObjectIdentity::new(self.catalog, ObjectId::allocate()?, name.kind);
        self.insert_identity_without_version(CatalogObjectRecord { identity, name })?;
        Ok(identity)
    }

    fn insert_identity_without_version(&mut self, prepared: CatalogObjectRecord) -> Result<()> {
        if self.identities_by_name.len() >= MAX_CATALOG_OBJECTS {
            return Err(Error::Resource("catalog object limit exceeded".into()));
        }
        let CatalogObjectRecord { identity, name } = prepared;
        if self.identities_by_name.contains_key(&name) {
            return Err(Error::Catalog(format!(
                "{} {} already exists",
                name.kind, name.name
            )));
        }
        self.ensure_local(identity)?;
        if identity.kind != name.kind {
            return Err(Error::InvalidInput(
                "catalog identity kind differs from its name".into(),
            ));
        }
        let schema = self.insert_schema(&name)?;
        self.identities_by_name.insert(name.clone(), identity);
        if self.names_by_identity.insert(identity, name).is_some() {
            return Err(Error::Internal("runtime object identity collision".into()));
        }
        if let Some(schema) = schema {
            self.dependencies.add_dependency(
                identity,
                schema,
                DependentFlags::blocking(),
                SubjectFlags::ordinary(),
            )?;
        }
        Ok(())
    }

    fn insert_schema(&self, name: &CatalogObjectName) -> Result<Option<ObjectIdentity>> {
        if name.kind != CatalogObjectKind::Table {
            return Ok(None);
        }
        let schema = name
            .schema
            .as_deref()
            .ok_or_else(|| Error::Internal("table registry name has no schema".into()))?;
        self.lookup(&CatalogObjectName::schema(schema)?)
            .map(Some)
            .ok_or_else(|| Error::Catalog(format!("schema {schema} does not exist")))
    }

    fn replace_name(
        &mut self,
        identity: ObjectIdentity,
        replacement: CatalogObjectName,
    ) -> Result<CatalogObjectName> {
        if self
            .identities_by_name
            .get(&replacement)
            .is_some_and(|existing| *existing != identity)
        {
            return Err(Error::Catalog(format!(
                "{} {} already exists",
                replacement.kind, replacement.name
            )));
        }
        let old = self
            .names_by_identity
            .insert(identity, replacement.clone())
            .ok_or_else(|| Error::Internal("catalog reverse name index lost identity".into()))?;
        if self.identities_by_name.remove(&old) != Some(identity) {
            return Err(Error::Internal(
                "catalog name indexes disagree during rename".into(),
            ));
        }
        self.identities_by_name.insert(replacement, identity);
        Ok(old)
    }

    fn ensure_local(&self, identity: ObjectIdentity) -> Result<()> {
        if identity.catalog != self.catalog {
            return Err(Error::InvalidInput(format!(
                "catalog object {identity} belongs to a different catalog"
            )));
        }
        Ok(())
    }

    fn advance_version(&mut self) -> Result<()> {
        self.version = self.version.checked_next()?;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn canonical_identifier(value: String) -> Result<String> {
    if value.is_empty() {
        return Err(Error::InvalidInput(
            "catalog identifier cannot be empty".into(),
        ));
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(Error::Resource(
            "catalog identifier exceeds size limit".into(),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn rebuild_allocates_fresh_runtime_identity_without_versioning_initial_state() {
        let first = CatalogRegistry::rebuild(
            ["main".into(), "analytics".into()],
            [TableName::new("analytics", "events")],
        )
        .unwrap();
        let second = CatalogRegistry::rebuild(
            ["main".into(), "analytics".into()],
            [TableName::new("analytics", "events")],
        )
        .unwrap();
        assert_eq!(first.identity().version, Some(CatalogVersion::new(0)));
        assert_eq!(first.len(), 3);
        assert_ne!(first.identity().id, second.identity().id);
        assert_ne!(
            first
                .lookup_table(&TableName::new("ANALYTICS", "EVENTS"))
                .unwrap(),
            second
                .lookup_table(&TableName::new("analytics", "events"))
                .unwrap()
        );
        assert!(matches!(
            CatalogRegistry::rebuild(["main".into(), "MAIN".into()], []),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            CatalogRegistry::rebuild(["main".into()], [TableName::new("missing", "events")]),
            Err(Error::Catalog(_))
        ));
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn insert_resolve_rename_and_replacement_keep_stable_object_semantics() {
        let mut registry = CatalogRegistry::rebuild(["main".into()], []).unwrap();
        let table = CatalogObjectName::table(&TableName::main("items")).unwrap();
        let identity = registry.insert(table).unwrap();
        assert_eq!(registry.identity().version, Some(CatalogVersion::new(1)));
        let binding = registry.bind_table(&TableName::main("items")).unwrap();
        assert_eq!(binding.identity(), Some(identity));
        assert!(registry.binding_is_current(&binding));

        registry
            .rename(
                identity,
                CatalogObjectName::table(&TableName::main("renamed")).unwrap(),
            )
            .unwrap();
        assert_eq!(
            registry.table_name_for_binding(&binding).unwrap(),
            TableName::main("renamed")
        );
        assert!(!registry.binding_is_current(&binding));
        assert_eq!(
            registry.lookup_table(&TableName::main("renamed")).unwrap(),
            Some(identity)
        );
        let forged =
            TableBinding::identified(TableName::main("wrong"), identity, registry.identity())
                .unwrap();
        assert!(!registry.binding_is_current(&forged));

        let version = registry.identity().version.unwrap();
        registry.alter(identity).unwrap();
        assert_eq!(
            registry.identity().version,
            Some(version.checked_next().unwrap())
        );

        registry
            .drop_object(identity, DropBehavior::Restrict)
            .unwrap();
        let replacement = registry
            .insert(CatalogObjectName::table(&TableName::main("items")).unwrap())
            .unwrap();
        assert_ne!(replacement, identity);
        assert!(registry.table_name_for_binding(&binding).is_err());
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn prepared_insert_reuses_one_identity_across_transaction_views() {
        let registry = CatalogRegistry::rebuild(["main".into()], []).unwrap();
        let prepared = registry
            .prepare_insert(CatalogObjectName::table(&TableName::main("items")).unwrap())
            .unwrap();
        let mut current = registry.clone();
        let mut basis = registry;

        current.insert_prepared(&prepared).unwrap();
        basis.insert_prepared(&prepared).unwrap();

        assert_eq!(
            current.lookup_table(&TableName::main("items")).unwrap(),
            Some(prepared.identity())
        );
        assert_eq!(current, basis);
        let before = current.clone();
        assert!(matches!(
            current.insert_prepared(&prepared),
            Err(Error::Catalog(_))
        ));
        assert_eq!(current, before);
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn cross_schema_table_rename_is_rejected_atomically() {
        let mut registry = CatalogRegistry::rebuild(["main".into(), "other".into()], []).unwrap();
        let table = registry
            .insert(CatalogObjectName::table(&TableName::main("items")).unwrap())
            .unwrap();
        let before = registry.clone();

        assert!(matches!(
            registry.rename(
                table,
                CatalogObjectName::table(&TableName::new("other", "items")).unwrap()
            ),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(registry, before);
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn dependency_drop_is_ordered_atomic_and_advances_once() {
        let mut registry = CatalogRegistry::rebuild(["main".into()], []).unwrap();
        let subject = registry
            .insert(CatalogObjectName::table(&TableName::main("subject")).unwrap())
            .unwrap();
        let dependent = registry
            .insert(CatalogObjectName::table(&TableName::main("dependent")).unwrap())
            .unwrap();
        registry
            .add_dependency(
                dependent,
                subject,
                DependentFlags::blocking(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        let before = registry.clone();
        assert!(matches!(
            registry.drop_object(subject, DropBehavior::Restrict),
            Err(Error::Catalog(_))
        ));
        assert_eq!(registry, before);

        let version = registry.identity().version.unwrap();
        let plan = registry
            .drop_object(subject, DropBehavior::Cascade)
            .unwrap();
        assert_eq!(
            plan.iter()
                .map(CatalogObjectRecord::identity)
                .collect::<Vec<_>>(),
            vec![dependent, subject]
        );
        assert_eq!(
            registry.identity().version,
            Some(version.checked_next().unwrap())
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn schema_dependencies_block_restrict_and_schema_rename_moves_table_names() {
        let mut registry = CatalogRegistry::rebuild(
            ["main".into(), "old".into()],
            [
                TableName::new("old", "first"),
                TableName::new("old", "second"),
            ],
        )
        .unwrap();
        let schema = registry.lookup_schema("old").unwrap().unwrap();
        let first = registry
            .lookup_table(&TableName::new("old", "first"))
            .unwrap()
            .unwrap();
        let before = registry.clone();
        assert!(matches!(
            registry.drop_object(schema, DropBehavior::Restrict),
            Err(Error::Catalog(_))
        ));
        assert_eq!(registry, before);

        registry
            .rename(schema, CatalogObjectName::schema("new").unwrap())
            .unwrap();
        assert_eq!(registry.lookup_schema("old").unwrap(), None);
        assert_eq!(registry.lookup_schema("new").unwrap(), Some(schema));
        assert_eq!(
            registry
                .lookup_table(&TableName::new("new", "first"))
                .unwrap(),
            Some(first)
        );
        assert_eq!(
            registry
                .lookup_table(&TableName::new("old", "first"))
                .unwrap(),
            None
        );

        let dropped = registry.drop_object(schema, DropBehavior::Cascade).unwrap();
        assert_eq!(dropped.last().unwrap().identity(), schema);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn namespaces_collisions_and_foreign_handles_fail_closed() {
        let mut first = CatalogRegistry::rebuild(["main".into()], []).unwrap();
        let second = CatalogRegistry::rebuild(["main".into()], []).unwrap();
        let schema = first.lookup_schema("MAIN").unwrap().unwrap();
        let table = first
            .insert(CatalogObjectName::table(&TableName::main("main")).unwrap())
            .unwrap();
        assert_ne!(schema, table);
        let before = first.clone();
        assert!(
            first
                .insert(CatalogObjectName::schema("MAIN").unwrap())
                .is_err()
        );
        assert_eq!(first, before);
        let foreign = second.lookup_schema("main").unwrap().unwrap();
        assert!(matches!(first.name(foreign), Err(Error::InvalidInput(_))));
        assert!(matches!(
            first.rename(table, CatalogObjectName::schema("not_a_table").unwrap()),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(first, before);
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn duplicate_dependency_is_idempotent_and_invalid_inputs_are_atomic() {
        let mut registry = CatalogRegistry::rebuild(["main".into()], []).unwrap();
        let first = registry
            .insert(CatalogObjectName::table(&TableName::main("first")).unwrap())
            .unwrap();
        let second = registry
            .insert(CatalogObjectName::table(&TableName::main("second")).unwrap())
            .unwrap();
        registry
            .add_dependency(
                first,
                second,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        let version = registry.identity().version;
        registry
            .add_dependency(
                first,
                second,
                DependentFlags::automatic(),
                SubjectFlags::ordinary(),
            )
            .unwrap();
        assert_eq!(registry.identity().version, version);
        let before = registry.clone();
        let foreign = CatalogRegistry::rebuild(["main".into()], [])
            .unwrap()
            .lookup_schema("main")
            .unwrap()
            .unwrap();
        assert!(
            registry
                .add_dependency(
                    first,
                    foreign,
                    DependentFlags::automatic(),
                    SubjectFlags::ordinary()
                )
                .is_err()
        );
        assert_eq!(registry, before);

        let mut blocked = registry.clone();
        blocked
            .add_dependency(
                first,
                second,
                DependentFlags {
                    alter_blocking: true,
                    ..DependentFlags::automatic()
                },
                SubjectFlags::ordinary(),
            )
            .unwrap();
        let before = blocked.clone();
        assert!(matches!(blocked.alter(second), Err(Error::Catalog(_))));
        assert_eq!(blocked, before);
    }
}
