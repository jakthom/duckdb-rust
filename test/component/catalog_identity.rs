use duckdb_rust::{
    DataType, Database, Error, Result, Value,
    catalog::{
        Catalog, CatalogMut, ColumnDefinition, ResolvedTable, TableAlteration, TableDefinition,
        TableName,
    },
    common::cast::CastRegistry,
    execution::{
        ExecutionContext,
        expression_executor::ScalarEvaluator,
        physical_plan::{NativePhysicalPlanner, PhysicalOperator, PhysicalPlanner},
        subquery::{PreparedSubqueries, StreamingSubqueries},
    },
    function::{FunctionRegistry, operator::OperatorRegistry},
    parallel::QueryContext,
    parser::{DuckDbParser, Parser},
    planner::{
        BindContext, Binder, BoundExpr, BoundStatement, Field, LogicalPlan, PlanNode, SqlBinder,
    },
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
fn bind_sql(catalog: &Snapshot, sql: &str) -> Result<BoundStatement> {
    let query = QueryContext::background().with_types(catalog.type_registry());
    let casts = CastRegistry::builtins();
    let functions = FunctionRegistry::builtins();
    let operators = OperatorRegistry::builtins();
    let mut statements = DuckDbParser.parse(sql)?;
    if statements.len() != 1 {
        return Err(Error::Internal(
            "identity test expected one statement".into(),
        ));
    }
    SqlBinder.bind(
        &statements.remove(0),
        &BindContext {
            catalog,
            casts: &casts,
            operators: &operators,
            query: &query,
            functions: &functions,
            expressions: &ScalarEvaluator,
            parameters: &[],
        },
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn scan_binding(plan: &LogicalPlan) -> Option<&duckdb_rust::catalog::TableBinding> {
    match &plan.node {
        PlanNode::Scan(table) => Some(table),
        PlanNode::Filter { input, .. }
        | PlanNode::Projection { input, .. }
        | PlanNode::Aggregate { input, .. }
        | PlanNode::Window { input, .. }
        | PlanNode::Sort { input, .. }
        | PlanNode::Limit { input, .. }
        | PlanNode::Distinct(input) => scan_binding(input),
        _ => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn physical_table_plans(
    planner: &dyn PhysicalPlanner,
    resolved: ResolvedTable,
) -> Result<Vec<Arc<dyn PhysicalOperator>>> {
    let (binding, definition) = resolved.into_parts();
    let schema = definition
        .columns
        .iter()
        .map(|column| Field::new(&column.name, column.data_type.clone()))
        .collect::<Vec<_>>();
    let scan = LogicalPlan {
        schema: schema.clone(),
        node: PlanNode::Scan(binding.clone()),
    };
    let filtered = LogicalPlan {
        schema: schema.clone(),
        node: PlanNode::Filter {
            input: Box::new(scan.clone()),
            predicate: BoundExpr::literal(Value::Boolean(true)),
        },
    };
    let lookup = LogicalPlan {
        schema,
        node: PlanNode::KeyLookup {
            table: binding,
            columns: vec![0],
            key: vec![Value::Integer(0)],
        },
    };
    [scan, filtered, lookup]
        .into_iter()
        .map(|plan| planner.plan(&plan))
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn assert_physical_plans_reject(
    plans: &[Arc<dyn PhysicalOperator>],
    transaction: &dyn duckdb_rust::transaction::Transaction,
    query: &QueryContext,
    planner: &dyn PhysicalPlanner,
    expected: fn(&Error) -> bool,
) {
    for plan in plans {
        let subquery_plans = PreparedSubqueries::new(planner);
        let context = ExecutionContext {
            transaction,
            expressions: &ScalarEvaluator,
            query,
            subquery_plans: &subquery_plans,
            subqueries: &StreamingSubqueries,
            outer: None,
            recursive: None,
        };
        let Err(error) = plan.open(&context) else {
            panic!("stale physical table plan was accepted")
        };
        assert!(expected(&error), "unexpected physical plan error: {error}");
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
    let mut foreign = Snapshot::default();
    foreign.create_table(definition("main", "items"), false)?;
    let foreign_binding = foreign
        .table_entry(&TableName::main("items"))?
        .binding()
        .clone();
    assert!(matches!(
        snapshot.resolve_table_binding(&foreign_binding),
        Err(Error::InvalidInput(_))
    ));
    snapshot.validate()?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bound_plans_distinguish_current_schema_from_stable_ddl_identity() -> Result<()> {
    let context = QueryContext::background();
    let mut snapshot = Snapshot::default();
    snapshot.create_table(definition("main", "items"), false)?;

    let query = bind_sql(&snapshot, "SELECT i FROM items")?;
    let BoundStatement::Query(plan) = &query else {
        return Err(Error::Internal("expected a query plan".into()));
    };
    let query_binding = scan_binding(plan)
        .ok_or_else(|| Error::Internal("query plan did not retain a scan binding".into()))?
        .clone();
    assert!(query_binding.identity().is_some());
    snapshot.current_table_binding(&query_binding)?;

    let drop = bind_sql(&snapshot, "DROP TABLE items")?;
    let BoundStatement::DropTable { tables, .. } = &drop else {
        return Err(Error::Internal("expected a drop-table plan".into()));
    };
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].identity(), query_binding.identity());

    snapshot.create_schema("unrelated", false)?;
    assert!(matches!(
        query.validate(&snapshot, &context),
        Err(Error::Bind(_))
    ));
    drop.validate(&snapshot, &context)?;
    assert_eq!(
        snapshot
            .resolve_table_binding(&query_binding)?
            .binding()
            .identity(),
        query_binding.identity()
    );

    snapshot.alter_table(
        &TableName::main("items"),
        &TableAlteration::RenameTable("renamed".into()),
        &context,
    )?;
    assert_eq!(
        snapshot
            .resolve_table_binding(&query_binding)?
            .definition()
            .name,
        TableName::main("renamed")
    );
    drop.validate(&snapshot, &context)?;
    assert!(matches!(
        snapshot.current_table_binding(&query_binding),
        Err(Error::Bind(_))
    ));

    snapshot.drop_table(&TableName::main("renamed"), false)?;
    snapshot.create_table(definition("main", "items"), false)?;
    assert!(matches!(
        snapshot.resolve_table_binding(&query_binding),
        Err(Error::Catalog(_))
    ));
    assert!(matches!(
        drop.validate(&snapshot, &context),
        Err(Error::Catalog(_))
    ));
    let replacement = snapshot.resolve_table_binding(
        &duckdb_rust::catalog::TableBinding::unversioned(TableName::main("items")),
    )?;
    assert_ne!(replacement.binding().identity(), query_binding.identity());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn physical_scan_boundaries_reject_stale_catalog_bindings() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let query = QueryContext::background().with_types(manager.types());
    let mut transaction = manager.begin()?;
    transaction
        .catalog_mut()?
        .create_table(definition("main", "items"), false)?;
    let planner = NativePhysicalPlanner::default();

    let stale_version = physical_table_plans(
        &planner,
        transaction
            .catalog()
            .table_entry(&TableName::main("items"))?,
    )?;
    transaction
        .catalog_mut()?
        .create_schema("unrelated", false)?;
    assert_physical_plans_reject(
        &stale_version,
        transaction.as_ref(),
        &query,
        &planner,
        |error| matches!(error, Error::Bind(_)),
    );

    let stale_name = physical_table_plans(
        &planner,
        transaction
            .catalog()
            .table_entry(&TableName::main("items"))?,
    )?;
    transaction.catalog_mut()?.alter_table(
        &TableName::main("items"),
        &TableAlteration::RenameTable("renamed".into()),
        &query,
    )?;
    assert_physical_plans_reject(
        &stale_name,
        transaction.as_ref(),
        &query,
        &planner,
        |error| matches!(error, Error::Bind(_)),
    );

    let stale_object = physical_table_plans(
        &planner,
        transaction
            .catalog()
            .table_entry(&TableName::main("renamed"))?,
    )?;
    transaction
        .catalog_mut()?
        .drop_table(&TableName::main("renamed"), false)?;
    transaction
        .catalog_mut()?
        .create_table(definition("main", "renamed"), false)?;
    assert_physical_plans_reject(
        &stale_object,
        transaction.as_ref(),
        &query,
        &planner,
        |error| matches!(error, Error::Catalog(_)),
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn identified_drop_if_exists_ignores_only_absence_and_preserves_replacements() -> Result<()> {
    let context = QueryContext::background();
    let mut snapshot = Snapshot::default();
    snapshot.create_table(definition("main", "items"), false)?;
    let statement = bind_sql(&snapshot, "DROP TABLE IF EXISTS items")?;
    let BoundStatement::DropTable { tables, .. } = &statement else {
        return Err(Error::Internal("expected a drop-table plan".into()));
    };
    let binding = tables[0].clone();
    snapshot.drop_table(&TableName::main("items"), false)?;
    statement.validate(&snapshot, &context)?;
    snapshot.drop_table_identified(&binding, true)?;

    snapshot.create_table(definition("main", "items"), false)?;
    statement.validate(&snapshot, &context)?;
    snapshot.drop_table_identified(&binding, true)?;
    let replacement = snapshot.table_entry(&TableName::main("items"))?;
    assert_ne!(replacement.binding().identity(), binding.identity());

    let unversioned = duckdb_rust::catalog::TableBinding::unversioned(TableName::main("items"));
    assert_eq!(
        snapshot
            .resolve_table_binding_if_exists(&unversioned)?
            .expect("name-bound replacement")
            .binding()
            .identity(),
        replacement.binding().identity()
    );
    assert!(matches!(
        snapshot.drop_table_identified(&unversioned, true),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(
        snapshot
            .table_entry(&TableName::main("items"))?
            .binding()
            .identity(),
        replacement.binding().identity()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn alternative_frontends_can_resolve_safe_ddl_bindings() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute("CREATE TABLE items(i INTEGER)")?;
    let original = connection
        .resolve_table(&TableName::main("items"))?
        .binding()
        .clone();
    connection.execute_plan(BoundStatement::AlterTable {
        table: original.clone(),
        alteration: TableAlteration::RenameTable("renamed".into()),
    })?;
    connection.query("SELECT * FROM renamed")?;

    connection.execute("DROP TABLE renamed; CREATE TABLE items(i INTEGER)")?;
    connection.execute_plan(BoundStatement::DropTable {
        tables: vec![original],
        if_exists: true,
    })?;
    connection.query("SELECT * FROM items")?;

    let replacement = connection
        .resolve_table(&TableName::main("items"))?
        .binding()
        .clone();
    connection.execute_plan(BoundStatement::DropTable {
        tables: vec![replacement],
        if_exists: false,
    })?;
    assert!(matches!(
        connection.query("SELECT * FROM items"),
        Err(Error::Catalog(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sql_name_provenance_and_prepared_syntax_rebind_across_catalog_changes() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute("CREATE TABLE items(i INTEGER); INSERT INTO items VALUES (7)")?;
    assert_eq!(
        connection
            .query("WITH items AS (SELECT 9 AS i) SELECT i FROM items")?
            .rows,
        vec![vec![Value::Integer(9)]]
    );
    assert_eq!(
        connection
            .query("WITH items AS (SELECT 9 AS i) SELECT i FROM main.items")?
            .rows,
        vec![vec![Value::Integer(7)]]
    );
    let explain = connection.query("EXPLAIN SELECT i FROM items")?.rows[0][0].to_string();
    assert!(!explain.contains("catalog_version"));
    assert!(!explain.contains("identity"));

    let old_name = connection.prepare("SELECT i FROM items")?;
    connection.execute("ALTER TABLE items RENAME TO renamed")?;
    assert!(matches!(
        connection.execute_prepared(&old_name, &[]),
        Err(Error::Catalog(_))
    ));

    let current = connection.prepare("SELECT i FROM renamed")?;
    connection.execute("CREATE SCHEMA unrelated")?;
    assert_eq!(
        connection.execute_prepared(&current, &[])?.rows,
        vec![vec![Value::Integer(7)]]
    );

    connection.execute(
        "DROP TABLE renamed; CREATE TABLE items(i INTEGER); INSERT INTO items VALUES (8)",
    )?;
    assert_eq!(
        connection.execute_prepared(&old_name, &[])?.rows,
        vec![vec![Value::Integer(8)]]
    );
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
