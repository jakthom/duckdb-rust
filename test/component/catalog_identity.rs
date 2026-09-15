use duckdb_rust::{
    DataType, Database, Error, Result, Value,
    catalog::{
        Catalog, CatalogMut, ColumnDefinition, CreateConflictPolicy, DropBehavior, ResolvedTable,
        TableAlteration, TableDefinition, TableName, TypeDefinition, TypeName,
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
        layout::CheckpointLayout,
        log::{Commit, TransactionChange},
        recovery::{RecoveredChange, RecoveryTarget},
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
fn enum_definition(schema: &str, name: &str, labels: &[&str]) -> Result<TypeDefinition> {
    TypeDefinition::enumeration(
        TypeName::new(schema, name),
        labels.iter().map(|label| (*label).into()).collect(),
    )
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
fn snapshot_named_types_preserve_identity_conflicts_and_concrete_table_enums() -> Result<()> {
    let mut snapshot = Snapshot::default();
    snapshot.create_schema("analytics", false)?;

    let empty = enum_definition("analytics", "empty", &[])?;
    assert!(snapshot.create_type(empty.clone(), CreateConflictPolicy::Error)?);
    assert_eq!(snapshot.named_type(&empty.name)?, empty);

    let original = enum_definition("analytics", "mood", &["sad"])?;
    assert!(snapshot.create_type(original.clone(), CreateConflictPolicy::Error)?);
    let original_entry = snapshot.type_entry(&original.name)?;
    let original_binding = original_entry.binding().clone();
    let before_ignore = snapshot.identity();
    assert!(!snapshot.create_type(
        enum_definition("analytics", "mood", &["ignored"])?,
        CreateConflictPolicy::Ignore,
    )?);
    assert_eq!(snapshot.identity(), before_ignore);
    assert_eq!(snapshot.named_type(&original.name)?, original);

    let table = TableDefinition {
        name: TableName::new("analytics", "mood"),
        columns: vec![ColumnDefinition::new(
            "value",
            original_entry.definition().data_type.clone(),
        )],
        unique_keys: Vec::new(),
    };
    snapshot.create_table(table.clone(), false)?;

    let replacement = enum_definition("analytics", "mood", &["happy"])?;
    let before_replace = snapshot.identity().expect("snapshot identity");
    assert!(snapshot.create_type(replacement.clone(), CreateConflictPolicy::Replace)?);
    assert_eq!(
        snapshot.identity().expect("snapshot identity").version,
        Some(
            before_replace
                .version
                .expect("versioned snapshot")
                .checked_next()?
        )
    );
    let replacement_entry = snapshot.type_entry(&replacement.name)?;
    assert_ne!(
        replacement_entry.binding().identity(),
        original_binding.identity()
    );
    assert_eq!(
        snapshot.table(&table.name)?.columns[0].data_type,
        original.data_type
    );
    assert_eq!(snapshot.named_type(&replacement.name)?, replacement);
    assert!(matches!(
        snapshot.drop_type_identified(&original_binding, false, DropBehavior::Restrict),
        Err(Error::Catalog(_))
    ));
    assert!(!snapshot.drop_type_identified(&original_binding, true, DropBehavior::Restrict,)?);
    assert_eq!(
        snapshot
            .type_entry(&TypeName::new("analytics", "mood"))?
            .binding()
            .identity(),
        replacement_entry.binding().identity()
    );

    assert!(matches!(
        snapshot.drop_schema("analytics", false),
        Err(Error::Catalog(_))
    ));
    assert!(snapshot.drop_type(
        &TypeName::new("analytics", "mood"),
        false,
        DropBehavior::Restrict,
    )?);
    assert_eq!(snapshot.table(&table.name)?, table);
    snapshot.drop_table(&table.name, false)?;
    assert!(matches!(
        snapshot.drop_schema("analytics", false),
        Err(Error::Catalog(_))
    ));
    assert!(snapshot.drop_type(&empty.name, false, DropBehavior::Restrict)?);
    snapshot.drop_schema("analytics", false)?;
    snapshot.validate()?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn named_type_recovery_is_atomic_and_obeys_schema_dependencies() -> Result<()> {
    let context = QueryContext::background();
    let mut snapshot = Snapshot::default();
    let definition = enum_definition("analytics", "mood", &["sad", "happy"])?;
    snapshot.apply_committed(
        &[
            RecoveredChange::CreateSchema("analytics".into()),
            RecoveredChange::CreateType {
                definition: definition.clone(),
                conflict: CreateConflictPolicy::Error,
            },
        ],
        &context,
    )?;
    assert_eq!(snapshot.named_type(&definition.name)?, definition);
    let before_failed_drop = snapshot.identity();
    assert!(matches!(
        snapshot.apply_committed(&[RecoveredChange::DropSchema("analytics".into())], &context,),
        Err(Error::Catalog(_))
    ));
    assert_eq!(snapshot.identity(), before_failed_drop);
    assert_eq!(
        snapshot.named_type(&TypeName::new("analytics", "mood"))?,
        definition
    );
    snapshot.apply_committed(
        &[
            RecoveredChange::DropType(TypeName::new("analytics", "mood")),
            RecoveredChange::DropSchema("analytics".into()),
        ],
        &context,
    )?;
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
fn transaction_named_types_publish_effective_changes_and_rollback() -> Result<()> {
    let durability = Arc::new(CapturingDurability::default());
    let manager = SnapshotTransactions::new(durability.clone())?;
    let old_reader = manager.begin()?;
    let mut transaction = manager.begin()?;
    transaction
        .catalog_mut()?
        .create_schema("analytics", false)?;
    let original = enum_definition("analytics", "mood", &["sad"])?;
    assert!(
        transaction
            .catalog_mut()?
            .create_type(original.clone(), CreateConflictPolicy::Error)?
    );
    let stale = transaction
        .catalog()
        .type_entry(&original.name)?
        .binding()
        .clone();
    let before_ignore = transaction.catalog().identity();
    assert!(!transaction.catalog_mut()?.create_type(
        enum_definition("analytics", "mood", &["ignored"])?,
        CreateConflictPolicy::Ignore,
    )?);
    assert_eq!(transaction.catalog().identity(), before_ignore);

    let replacement = enum_definition("analytics", "mood", &["happy"])?;
    assert!(
        transaction
            .catalog_mut()?
            .create_type(replacement.clone(), CreateConflictPolicy::Replace)?
    );
    assert!(!transaction.catalog_mut()?.drop_type_identified(
        &stale,
        true,
        DropBehavior::Restrict,
    )?);
    assert_eq!(
        transaction.catalog().named_type(&replacement.name)?,
        replacement
    );
    transaction.commit()?;

    assert!(old_reader.catalog().named_type(&original.name).is_err());
    let published = manager.begin()?;
    assert_eq!(
        published.catalog().named_type(&replacement.name)?,
        replacement
    );
    let published_identity = published.catalog().identity();

    let mut abandoned = manager.begin()?;
    assert!(abandoned.catalog_mut()?.create_type(
        enum_definition("analytics", "abandoned", &[])?,
        CreateConflictPolicy::Error,
    )?);
    drop(abandoned);
    let after_rollback = manager.begin()?;
    assert_eq!(after_rollback.catalog().identity(), published_identity);
    assert!(
        after_rollback
            .catalog()
            .named_type(&TypeName::new("analytics", "abandoned"))
            .is_err()
    );

    let publications = durability
        .publications
        .lock()
        .map_err(|_| Error::Internal("capturing journal mutex poisoned".into()))?;
    let [changes] = publications.as_slice() else {
        return Err(Error::Internal(
            "named type transaction must publish exactly once".into(),
        ));
    };
    let [
        TransactionChange::CreateSchema(schema),
        TransactionChange::CreateType {
            definition: created,
            conflict: CreateConflictPolicy::Error,
        },
        TransactionChange::CreateType {
            definition: replaced,
            conflict: CreateConflictPolicy::Replace,
        },
    ] = changes.as_slice()
    else {
        return Err(Error::Internal(
            "named type mutations produced an unexpected journal".into(),
        ));
    };
    assert_eq!(schema, "analytics");
    assert_eq!(created, &original);
    assert_eq!(replaced, &replacement);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn private_reopen_rebuilds_fresh_runtime_handles_without_wire_fields() -> Result<()> {
    let mut snapshot = Snapshot::default();
    snapshot.create_schema("analytics", false)?;
    snapshot.create_table(definition("analytics", "events"), false)?;
    let named_type = enum_definition("analytics", "mood", &[])?;
    snapshot.create_type(named_type.clone(), CreateConflictPolicy::Error)?;
    let before_catalog = snapshot.identity().expect("snapshot identity");
    let before_table = snapshot
        .table_entry(&TableName::new("analytics", "events"))?
        .binding()
        .identity();
    let before_type = snapshot.type_entry(&named_type.name)?.binding().identity();

    let bytes =
        serde_json::to_vec(&snapshot).map_err(|error| Error::Internal(error.to_string()))?;
    let wire: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| Error::Internal(error.to_string()))?;
    let fields = wire.as_object().expect("snapshot JSON object");
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["named_types", "schemas", "tables"]
    );
    let reopened: Snapshot =
        serde_json::from_slice(&bytes).map_err(|error| Error::Internal(error.to_string()))?;
    let after_catalog = reopened.identity().expect("snapshot identity");
    let after_table = reopened
        .table_entry(&TableName::new("analytics", "events"))?
        .binding()
        .identity();
    let after_type = reopened.type_entry(&named_type.name)?.binding().identity();
    assert_ne!(after_catalog.id, before_catalog.id);
    assert_eq!(after_catalog.version.map(|version| version.get()), Some(0));
    assert_ne!(after_table, before_table);
    assert_ne!(after_type, before_type);
    assert!(matches!(
        reopened.table_by_identity(&before_table.expect("old table identity")),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        reopened.type_by_identity(&before_type.expect("old named type identity")),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(reopened.named_type(&named_type.name)?, named_type);

    let mut legacy_wire = wire;
    legacy_wire
        .as_object_mut()
        .expect("snapshot JSON object")
        .remove("named_types");
    let legacy: Snapshot =
        serde_json::from_value(legacy_wire).map_err(|error| Error::Internal(error.to_string()))?;
    assert!(legacy.named_types()?.is_empty());
    legacy.table(&TableName::new("analytics", "events"))?;
    let layout = CheckpointLayout::identity(&snapshot)?;
    snapshot.validate_checkpoint_layout(&reopened, &layout, &QueryContext::background())?;
    assert!(
        snapshot
            .validate_checkpoint_layout(&legacy, &layout, &QueryContext::background())
            .is_err()
    );
    reopened.validate()?;
    legacy.validate()?;
    Ok(())
}
