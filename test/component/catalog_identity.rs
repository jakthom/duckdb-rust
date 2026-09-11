use duckdb_rust::{
    DataType, Error, Result,
    catalog::{Catalog, CatalogMut, ColumnDefinition, TableAlteration, TableDefinition, TableName},
    parallel::QueryContext,
    storage::{
        checkpoint::{Durability, MemoryDurability, PublishOutcome},
        log::{Commit, TransactionChange},
        table::Snapshot,
    },
    transaction::{SnapshotTransactions, TransactionManager},
};
use std::sync::{Arc, Mutex};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn definition(schema: &str, name: &str) -> TableDefinition {
    TableDefinition {
        name: TableName::new(schema, name),
        columns: vec![ColumnDefinition::new("i", DataType::Integer)],
        unique_keys: Vec::new(),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn snapshot_bindings_survive_rename_but_not_drop_and_replacement() -> Result<()> {
    let context = QueryContext::background();
    let mut snapshot = Snapshot::default();
    let initial = snapshot.identity().expect("snapshot identity");
    snapshot.create_table(definition("main", "items"), false)?;
    let created = snapshot.identity().expect("snapshot identity");
    assert_eq!(created.id, initial.id);
    assert!(created.version > initial.version);

    let binding = snapshot
        .table_entry(&TableName::main("items"))?
        .binding()
        .clone();
    let object = binding.identity().expect("table identity");
    assert_eq!(object.catalog, created.id);
    snapshot.alter_table(
        &TableName::main("items"),
        &TableAlteration::RenameTable("renamed".into()),
        &context,
    )?;
    assert!(snapshot.table(&TableName::main("items")).is_err());
    assert_eq!(
        snapshot.table_by_identity(&object)?.definition().name,
        TableName::main("renamed")
    );

    snapshot.drop_table_identified(&binding, false)?;
    assert!(snapshot.table(&TableName::main("renamed")).is_err());
    snapshot.create_table(definition("main", "items"), false)?;
    let replacement = snapshot.table_entry(&TableName::main("items"))?;
    assert_ne!(replacement.binding().identity(), Some(object));
    assert!(matches!(
        snapshot.drop_table_identified(&binding, false),
        Err(Error::Catalog(_))
    ));
    assert_eq!(
        snapshot.table(&TableName::main("items"))?,
        definition("main", "items")
    );
    snapshot.validate()?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn schema_dependencies_make_restrict_atomic() -> Result<()> {
    let mut snapshot = Snapshot::default();
    snapshot.create_schema("analytics", false)?;
    snapshot.create_table(definition("analytics", "events"), false)?;
    let before = snapshot.identity();
    assert!(matches!(
        snapshot.drop_schema("analytics", false),
        Err(Error::Catalog(_))
    ));
    assert_eq!(snapshot.identity(), before);
    assert_eq!(
        snapshot.table(&TableName::new("analytics", "events"))?,
        definition("analytics", "events")
    );
    snapshot.drop_table(&TableName::new("analytics", "events"), false)?;
    snapshot.drop_schema("analytics", false)?;
    assert!(!snapshot.schemas()?.contains(&"analytics".into()));
    snapshot.validate()?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn transaction_snapshots_publish_and_rollback_catalog_versions() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let old_reader = manager.begin()?;
    let initial = old_reader.catalog().identity().expect("snapshot identity");

    let mut writer = manager.begin()?;
    writer
        .catalog_mut()?
        .create_table(definition("main", "published"), false)?;
    let uncommitted = writer.catalog().identity().expect("snapshot identity");
    assert_eq!(uncommitted.id, initial.id);
    assert!(uncommitted.version > initial.version);
    writer.commit()?;

    assert_eq!(old_reader.catalog().identity(), Some(initial));
    assert!(
        old_reader
            .catalog()
            .table(&TableName::main("published"))
            .is_err()
    );
    let published = manager.begin()?;
    assert_eq!(published.catalog().identity(), Some(uncommitted));
    published.catalog().table(&TableName::main("published"))?;

    let mut abandoned = manager.begin()?;
    abandoned.catalog_mut()?.create_schema("abandoned", false)?;
    assert!(abandoned.catalog().identity() > published.catalog().identity());
    drop(abandoned);
    let after_rollback = manager.begin()?;
    assert_eq!(after_rollback.catalog().identity(), Some(uncommitted));
    assert!(
        !after_rollback
            .catalog()
            .schemas()?
            .contains(&"abandoned".into())
    );
    Ok(())
}

#[derive(Default)]
struct CapturingDurability {
    publications: Mutex<Vec<Vec<TransactionChange>>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Durability for CapturingDurability {
    fn name(&self) -> &'static str {
        "capturing-catalog-journal"
    }

    fn load(
        &self,
        types: Arc<duckdb_rust::common::type_registry::TypeRegistry>,
    ) -> Result<Snapshot> {
        Snapshot::try_new(types)
    }

    fn requires_journal(&self) -> bool {
        true
    }

    fn publish(&self, commit: Commit<'_>) -> Result<PublishOutcome> {
        let changes = commit.changes.ok_or_else(|| {
            Error::Internal("capturing durability requires a transaction journal".into())
        })?;
        self.publications
            .lock()
            .map_err(|_| Error::Internal("capturing journal mutex poisoned".into()))?
            .push(changes.to_vec());
        Ok(PublishOutcome::Published)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn transaction_bindings_mutate_renamed_objects_and_reject_replacements() -> Result<()> {
    let durability = Arc::new(CapturingDurability::default());
    let manager = SnapshotTransactions::new(durability.clone())?;
    let context = QueryContext::background();
    let mut transaction = manager.begin()?;
    transaction
        .catalog_mut()?
        .create_table(definition("main", "items"), false)?;
    let binding = transaction
        .catalog()
        .table_entry(&TableName::main("items"))?
        .binding()
        .clone();
    let original = binding.identity().expect("identified transaction table");

    assert!(transaction.catalog_mut()?.alter_table_identified(
        &binding,
        &TableAlteration::RenameTable("renamed".into()),
        &context,
    )?);
    assert_eq!(
        transaction
            .catalog()
            .table_by_identity(&original)?
            .definition()
            .name,
        TableName::main("renamed")
    );
    transaction
        .catalog_mut()?
        .drop_table_identified(&binding, false)?;

    transaction
        .catalog_mut()?
        .create_table(definition("main", "items"), false)?;
    let replacement = transaction
        .catalog()
        .table_entry(&TableName::main("items"))?;
    assert_ne!(replacement.binding().identity(), Some(original));
    assert!(matches!(
        transaction.catalog_mut()?.alter_table_identified(
            &binding,
            &TableAlteration::RenameTable("wrong".into()),
            &context,
        ),
        Err(Error::Catalog(_))
    ));
    assert!(matches!(
        transaction
            .catalog_mut()?
            .drop_table_identified(&binding, false),
        Err(Error::Catalog(_))
    ));
    transaction
        .catalog_mut()?
        .drop_table_identified(&binding, true)?;
    assert_eq!(
        transaction.catalog().table(&TableName::main("items"))?,
        definition("main", "items")
    );
    transaction.commit()?;

    let publications = durability
        .publications
        .lock()
        .map_err(|_| Error::Internal("capturing journal mutex poisoned".into()))?;
    let [changes] = publications.as_slice() else {
        return Err(Error::Internal(
            "identified transaction must publish exactly once".into(),
        ));
    };
    let [
        TransactionChange::CreateTable(created),
        TransactionChange::AlterTable {
            table: renamed_from,
            alteration: TableAlteration::RenameTable(renamed_to),
            materialized_rows: None,
        },
        TransactionChange::DropTable(dropped),
        TransactionChange::CreateTable(recreated),
    ] = changes.as_slice()
    else {
        return Err(Error::Internal(
            "identified mutations produced an unexpected journal".into(),
        ));
    };
    assert_eq!(created.name, TableName::main("items"));
    assert_eq!(renamed_from, &TableName::main("items"));
    assert_eq!(renamed_to, "renamed");
    assert_eq!(dropped, &TableName::main("renamed"));
    assert_eq!(recreated.name, TableName::main("items"));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn private_reopen_rebuilds_fresh_runtime_handles_without_wire_fields() -> Result<()> {
    let mut snapshot = Snapshot::default();
    snapshot.create_schema("analytics", false)?;
    snapshot.create_table(definition("analytics", "events"), false)?;
    let before_catalog = snapshot.identity().expect("snapshot identity");
    let before_table = snapshot
        .table_entry(&TableName::new("analytics", "events"))?
        .binding()
        .identity();

    let bytes =
        serde_json::to_vec(&snapshot).map_err(|error| Error::Internal(error.to_string()))?;
    let wire: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| Error::Internal(error.to_string()))?;
    let fields = wire.as_object().expect("snapshot JSON object");
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["schemas", "tables"]
    );
    let reopened: Snapshot =
        serde_json::from_slice(&bytes).map_err(|error| Error::Internal(error.to_string()))?;
    let after_catalog = reopened.identity().expect("snapshot identity");
    let after_table = reopened
        .table_entry(&TableName::new("analytics", "events"))?
        .binding()
        .identity();
    assert_ne!(after_catalog.id, before_catalog.id);
    assert_eq!(after_catalog.version.map(|version| version.get()), Some(0));
    assert_ne!(after_table, before_table);
    assert!(matches!(
        reopened.table_by_identity(&before_table.expect("old table identity")),
        Err(Error::InvalidInput(_))
    ));
    reopened.validate()?;
    Ok(())
}
