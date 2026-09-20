use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[path = "component/argument_combination.rs"]
mod argument_combination;
#[path = "component/argument_provenance.rs"]
mod argument_provenance;
#[path = "component/catalog_identity.rs"]
mod catalog_identity;
#[path = "component/closed_argument.rs"]
mod closed_argument;
#[path = "component/comparison_nulls.rs"]
mod comparison_nulls;
#[path = "component/default_lifecycle.rs"]
mod default_lifecycle;
#[path = "component/default_syntax.rs"]
mod default_syntax;
#[path = "component/native_decode_context.rs"]
mod native_decode_context;
#[path = "component/scalar_expansion.rs"]
mod scalar_expansion;
#[path = "component/scalar_overload.rs"]
mod scalar_overload;
#[path = "component/stored_expression.rs"]
mod stored_expression;
#[path = "component/typed_constant.rs"]
mod typed_constant;
#[path = "component/value_binding.rs"]
mod value_binding;

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    catalog::{Catalog, CatalogMut, ColumnDefinition, TableBinding, TableDefinition, TableName},
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec},
        vector::{DataChunk, Vector},
    },
    execution::{
        operator::join::{HashJoin, NestedLoopJoin},
        physical_plan::NativePhysicalPlanner,
    },
    function::{FunctionEffects, FunctionRegistry, ScalarFunction},
    optimizer::IdentityOptimizer,
    parallel::{InterruptHandle, QueryContext},
    storage::{
        TableStorage, TableStorageMut, UpdateMetadata,
        checkpoint::{Durability, FileCheckpoint, MemoryDurability},
        filesystem::OpenMode,
        format::JsonSnapshotFormat,
        table::Snapshot,
    },
    transaction::{SnapshotTransactions, TransactionManager},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn integers(values: &[i128]) -> Vec<Value> {
    values.iter().copied().map(Value::Integer).collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn scalar_catalog_resolution_precedes_unsupported_argument_binding() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    for sql in [
        "SELECT missing_scalar(NULL::ANY)",
        "SELECT missing_scalar(*, 'sum')",
        "SELECT missing_scalar(ARRAY[(1,2), (3,4)])",
        "SELECT missing_scalar(1/0)",
    ] {
        assert!(
            matches!(connection.query(sql), Err(Error::Catalog(message)) if message.contains("missing_scalar")),
            "{sql}"
        );
    }
    // Existing functions still validate their own arguments rather than being
    // converted into missing-function errors by the shared binding path.
    assert!(matches!(
        connection.query("SELECT length(1)"),
        Err(Error::Bind(_))
    ));
    assert_eq!(
        connection.query("SELECT length('abc')")?.rows,
        vec![vec![Value::Integer(3)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn column_projection_preserves_shared_values_shape_and_lifetimes() -> Result<()> {
    let source = DataChunk::new(
        vec![
            Vector::flat(
                DataType::Integer,
                vec![Value::Integer(1), Value::Null, Value::Integer(3)],
            )?,
            Vector::constant(DataType::Varchar, Value::Varchar("retained".into()), 3)?,
        ],
        3,
    )?;
    let selected = source.select(&[2, 0, 2])?;
    let projected = selected.project(&[1, 0, 0])?;
    assert_eq!(projected.columns()[1].get(0), source.columns()[0].get(2));
    assert_eq!(projected.columns()[1].get(1), projected.columns()[2].get(1));
    let empty_columns = selected.project(&[])?;
    assert_eq!(empty_columns.len(), 3);
    assert_eq!(
        empty_columns.rows().collect::<Vec<_>>(),
        vec![vec![], vec![], vec![]]
    );
    assert!(matches!(selected.project(&[0, 2]), Err(Error::Internal(_))));
    let empty_rows = source.select(&[])?.project(&[1, 0])?;
    assert!(empty_rows.is_empty());
    assert_eq!(empty_rows.columns().len(), 2);
    drop(source);
    drop(selected);
    assert_eq!(
        projected.rows().collect::<Vec<_>>(),
        [3, 1, 3]
            .into_iter()
            .map(|value| vec![
                Value::Varchar("retained".into()),
                Value::Integer(value),
                Value::Integer(value)
            ])
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn relational_contract_across_compositions() -> Result<()> {
    for batch_size in [1, 3, 2048] {
        for hash_join in [false, true] {
            let joins: Vec<Arc<dyn duckdb_rust::execution::operator::join::JoinAlgorithm>> =
                if hash_join {
                    vec![Arc::new(HashJoin), Arc::new(NestedLoopJoin)]
                } else {
                    vec![Arc::new(NestedLoopJoin)]
                };
            let db = DatabaseBuilder::new()
                .batch_size(batch_size)
                .optimizer(Arc::new(IdentityOptimizer))
                .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(joins)))
                .build()?;
            let mut c = db.connect();
            c.execute("CREATE TABLE a(k INTEGER, v VARCHAR); CREATE TABLE b(k INTEGER, n INTEGER); INSERT INTO a VALUES (1,'a'),(1,'b'),(2,'c'),(NULL,'n'); INSERT INTO b VALUES (1,10),(1,20),(3,30),(NULL,40)")?;
            assert_eq!(
                c.query("SELECT count(*), sum(n) FROM a JOIN b ON a.k=b.k")?
                    .rows,
                vec![integers(&[4, 60])]
            );
            assert_eq!(
                c.query("SELECT count(*) FROM a LEFT JOIN b ON a.k=b.k")?
                    .rows,
                vec![integers(&[6])]
            );
            assert_eq!(
                c.query("SELECT count(*) FROM a FULL JOIN b ON a.k=b.k")?
                    .rows,
                vec![integers(&[8])]
            );
            assert_eq!(
                c.query("SELECT count(*) FROM a SEMI JOIN b ON a.k=b.k")?
                    .rows,
                vec![integers(&[2])]
            );
            assert_eq!(
                c.query("SELECT count(*) FROM a ANTI JOIN b ON a.k=b.k")?
                    .rows,
                vec![integers(&[2])]
            );
            assert_eq!(
                c.query("SELECT k, count(*) FROM a GROUP BY k ORDER BY k")?
                    .rows,
                vec![
                    integers(&[1, 2]),
                    integers(&[2, 1]),
                    vec![Value::Null, Value::Integer(1)]
                ]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn expressions_preserve_nulls_errors_and_short_circuiting() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(
        c.query("SELECT NULL AND false, NULL OR true, NULL AND true, NULL OR false, NOT NULL")?
            .rows,
        vec![vec![
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Null,
            Value::Null,
            Value::Null
        ]]
    );
    assert_eq!(c.query("SELECT coalesce(1, CAST('bad' AS INTEGER)), CASE WHEN true THEN 2 ELSE CAST('bad' AS INTEGER) END")?.rows, vec![integers(&[1,2])]);
    assert_eq!(
        c.query("SELECT 2 IN (1,NULL), 1 NOT IN (1,NULL), NULL IS NULL, NULL IS NOT NULL")?
            .rows,
        vec![vec![
            Value::Null,
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false)
        ]]
    );
    assert!(c.query("SELECT 2147483647::INTEGER + 1::INTEGER").is_err());
    assert_eq!(
        c.query("SELECT try_cast('bad' AS INTEGER)")?.rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        c.query("SELECT 'a🦆' LIKE 'a_', length('a🦆')")?.rows,
        vec![vec![Value::Boolean(true), Value::Integer(2)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn snapshot_isolation_and_write_conflicts() -> Result<()> {
    let db = Database::memory()?;
    let mut a = db.connect();
    let mut b = db.connect();
    a.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (1); BEGIN")?;
    b.execute("UPDATE t SET i=2")?;
    assert_eq!(a.query("SELECT * FROM t")?.rows, vec![integers(&[1])]);
    a.execute("UPDATE t SET i=3")?;
    assert!(matches!(a.query("COMMIT"), Err(Error::Conflict)));
    assert_eq!(a.query("SELECT * FROM t")?.rows, vec![integers(&[2])]);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_row_domains_retain_readers_and_commit_disjoint_writers() -> Result<()> {
    let db = Database::memory()?;
    let mut reader = db.connect();
    let mut left = db.connect();
    let mut right = db.connect();
    left.execute("CREATE TABLE d1_rows(i INTEGER PRIMARY KEY, v INTEGER); INSERT INTO d1_rows VALUES (1,10),(2,20)")?;
    reader.execute("BEGIN")?;
    left.execute("BEGIN; UPDATE d1_rows SET v=11 WHERE i=1")?;
    right.execute("BEGIN; UPDATE d1_rows SET v=21 WHERE i=2")?;
    left.execute("COMMIT")?;
    assert_eq!(
        reader.query("SELECT v FROM d1_rows WHERE i=1")?.rows,
        vec![integers(&[10])]
    );
    right.execute("COMMIT")?;
    let mut fresh = db.connect();
    assert_eq!(
        fresh.query("SELECT v FROM d1_rows ORDER BY i")?.rows,
        vec![integers(&[11]), integers(&[21])]
    );
    reader.execute("ROLLBACK")?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_same_row_winner_is_atomic_and_loser_does_not_publish() -> Result<()> {
    let db = Database::memory()?;
    let mut first = db.connect();
    let mut second = db.connect();
    first.execute("CREATE TABLE d1_conflict(i INTEGER PRIMARY KEY, v INTEGER); INSERT INTO d1_conflict VALUES (1,10)")?;
    first.execute("BEGIN; UPDATE d1_conflict SET v=11 WHERE i=1")?;
    second.execute("BEGIN; DELETE FROM d1_conflict WHERE i=1")?;
    first.execute("COMMIT")?;
    assert!(matches!(second.query("COMMIT"), Err(Error::Conflict)));
    let mut fresh = db.connect();
    assert_eq!(
        fresh.query("SELECT v FROM d1_conflict")?.rows,
        vec![integers(&[11])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_catalog_domains_allow_unrelated_create_and_reject_same_name() -> Result<()> {
    let db = Database::memory()?;
    let mut left = db.connect();
    let mut right = db.connect();
    left.execute("BEGIN; CREATE TABLE d1_left(i INTEGER)")?;
    right.execute("BEGIN; CREATE TABLE d1_right(i INTEGER)")?;
    left.execute("COMMIT")?;
    right.execute("COMMIT")?;
    let mut one = db.connect();
    let mut two = db.connect();
    one.execute("BEGIN; CREATE TABLE d1_same(i INTEGER)")?;
    two.execute("BEGIN; CREATE TABLE d1_same(i INTEGER)")?;
    one.execute("COMMIT")?;
    assert!(matches!(two.query("COMMIT"), Err(Error::Conflict)));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_concurrent_appends_survive() -> Result<()> {
    let db = Database::memory()?;
    let mut first = db.connect();
    let mut second = db.connect();
    first.execute(
        "CREATE TABLE d1_append(i INTEGER PRIMARY KEY); INSERT INTO d1_append VALUES (1)",
    )?;
    first.execute("BEGIN; INSERT INTO d1_append VALUES (2)")?;
    second.execute("BEGIN; INSERT INTO d1_append VALUES (3)")?;
    first.execute("COMMIT")?;
    second.execute("COMMIT")?;
    let mut verify = db.connect();
    assert_eq!(
        verify.query("SELECT i FROM d1_append ORDER BY i")?.rows,
        vec![integers(&[1]), integers(&[2]), integers(&[3])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_read_only_transaction_never_conflicts_with_newer_publication() -> Result<()> {
    let db = Database::memory()?;
    let mut reader = db.connect();
    let mut writer = db.connect();
    writer.execute("CREATE TABLE d1_reader(i INTEGER); INSERT INTO d1_reader VALUES (1)")?;
    reader.execute("BEGIN")?;
    writer.execute("INSERT INTO d1_reader VALUES (2)")?;
    assert_eq!(
        reader.query("SELECT i FROM d1_reader ORDER BY i")?.rows,
        vec![integers(&[1])]
    );
    reader.execute("COMMIT")?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_stale_catalog_binding_cannot_target_recreated_name() -> Result<()> {
    let db = Database::memory()?;
    let mut stale = db.connect();
    let mut winner = db.connect();
    winner.execute("CREATE TABLE d1_recreated(i INTEGER); INSERT INTO d1_recreated VALUES (1)")?;
    stale.execute("BEGIN; INSERT INTO d1_recreated VALUES (2)")?;
    winner.execute("DROP TABLE d1_recreated; CREATE TABLE d1_recreated(i INTEGER); INSERT INTO d1_recreated VALUES (9)")?;
    assert!(matches!(stale.query("COMMIT"), Err(Error::Conflict)));
    assert_eq!(
        winner.query("SELECT i FROM d1_recreated")?.rows,
        vec![integers(&[9])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_schema_dependency_race_never_leaves_orphan_table() -> Result<()> {
    let db = Database::memory()?;
    let mut dropper = db.connect();
    let mut creator = db.connect();
    dropper.execute("CREATE SCHEMA d1_schema")?;
    dropper.execute("BEGIN; DROP SCHEMA d1_schema")?;
    creator.execute("BEGIN; CREATE TABLE d1_schema.t(i INTEGER)")?;
    dropper.execute("COMMIT")?;
    assert!(creator.query("COMMIT").is_err());
    let mut verify = db.connect();
    assert!(verify.query("SELECT * FROM d1_schema.t").is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_rebase_preserves_primary_key_constraint_consumer() -> Result<()> {
    let db = Database::memory()?;
    let mut left = db.connect();
    let mut right = db.connect();
    left.execute("CREATE TABLE d1_key(i INTEGER PRIMARY KEY, v INTEGER); INSERT INTO d1_key VALUES (1,0),(2,0)")?;
    left.execute("BEGIN; UPDATE d1_key SET v=1 WHERE i=1")?;
    right.execute("BEGIN; UPDATE d1_key SET v=2 WHERE i=2")?;
    left.execute("COMMIT")?;
    right.execute("COMMIT")?;
    assert!(left.execute("INSERT INTO d1_key VALUES (1,99)").is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_checkpoint_keeps_retained_reader_snapshot() -> Result<()> {
    let db = Database::memory()?;
    let mut reader = db.connect();
    let mut writer = db.connect();
    writer
        .execute("CREATE TABLE d1_checkpoint(i INTEGER); INSERT INTO d1_checkpoint VALUES (1)")?;
    reader.execute("BEGIN")?;
    writer.execute("INSERT INTO d1_checkpoint VALUES (2)")?;
    writer.checkpoint()?;
    assert_eq!(
        reader.query("SELECT i FROM d1_checkpoint ORDER BY i")?.rows,
        vec![integers(&[1])]
    );
    reader.execute("ROLLBACK")?;
    assert_eq!(
        writer.query("SELECT i FROM d1_checkpoint ORDER BY i")?.rows,
        vec![integers(&[1]), integers(&[2])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_uncertain_publication_poison_blocks_stale_retry() -> Result<()> {
    let db = DatabaseBuilder::new()
        .durability(Arc::new(FailingDurability {
            publications: AtomicUsize::new(0),
            uncertain: true,
        }))
        .build()?;
    let mut writer = db.connect();
    writer.execute("CREATE TABLE d1_poison(i INTEGER)")?;
    assert!(matches!(
        writer.execute("INSERT INTO d1_poison VALUES (1)"),
        Err(Error::CommitUnknown(_))
    ));
    assert!(matches!(
        writer.execute("INSERT INTO d1_poison VALUES (2)"),
        Err(Error::CommitUnknown(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_file_backed_winner_journal_and_reopen_exclude_loser() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("d1-winners.json");
    let open = || {
        DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()
    };
    let db = open()?;
    let mut left = db.connect();
    let mut right = db.connect();
    left.execute("CREATE TABLE d1_winner(i INTEGER PRIMARY KEY, v INTEGER); INSERT INTO d1_winner VALUES (1,0),(2,0)")?;
    left.execute("BEGIN; UPDATE d1_winner SET v=1 WHERE i=1")?;
    right.execute("BEGIN; UPDATE d1_winner SET v=2 WHERE i=2")?;
    left.execute("COMMIT")?;
    right.execute("COMMIT")?;
    let mut stale = db.connect();
    let mut winner = db.connect();
    stale.execute("BEGIN; UPDATE d1_winner SET v=3 WHERE i=1")?;
    winner.execute("BEGIN; DELETE FROM d1_winner WHERE i=1")?;
    winner.execute("COMMIT")?;
    let winner_bytes = std::fs::read(&path)?;
    assert!(matches!(stale.query("COMMIT"), Err(Error::Conflict)));
    assert_eq!(
        std::fs::read(&path)?,
        winner_bytes,
        "loser must not publish a journal/checkpoint successor"
    );
    drop(stale);
    drop(winner);
    drop(left);
    drop(right);
    drop(db);
    let reopened = open()?;
    let mut verify = reopened.connect();
    assert_eq!(
        verify.query("SELECT i,v FROM d1_winner ORDER BY i")?.rows,
        vec![integers(&[2, 2])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_rebase_remaps_inserted_row_updates_away_from_rival_append() -> Result<()> {
    let db = Database::memory()?;
    let mut local = db.connect();
    let mut rival = db.connect();
    local.execute("CREATE TABLE d1_remap(i INTEGER PRIMARY KEY, v INTEGER); INSERT INTO d1_remap VALUES (1,0)")?;
    local
        .execute("BEGIN; INSERT INTO d1_remap VALUES (2,0); UPDATE d1_remap SET v=20 WHERE i=2")?;
    rival.execute("INSERT INTO d1_remap VALUES (3,0)")?;
    local.execute("COMMIT")?;
    assert_eq!(
        rival.query("SELECT i,v FROM d1_remap ORDER BY i")?.rows,
        vec![integers(&[1, 0]), integers(&[2, 20]), integers(&[3, 0])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_rebase_remaps_inserted_row_delete_away_from_rival_append() -> Result<()> {
    let db = Database::memory()?;
    let mut local = db.connect();
    let mut rival = db.connect();
    local.execute("CREATE TABLE d1_delete_remap(i INTEGER PRIMARY KEY); INSERT INTO d1_delete_remap VALUES (1)")?;
    // Both transactions initially assign their append the same physical row
    // ID. Rebase must delete local's shifted row, never the winner's append.
    local.execute(
        "BEGIN; INSERT INTO d1_delete_remap VALUES (2); DELETE FROM d1_delete_remap WHERE i=2",
    )?;
    rival.execute("INSERT INTO d1_delete_remap VALUES (3)")?;
    local.execute("COMMIT")?;
    assert_eq!(
        rival
            .query("SELECT i FROM d1_delete_remap ORDER BY i")?
            .rows,
        vec![integers(&[1]), integers(&[3])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_rebase_preserves_mutation_context_cancellation() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let table = TableName::main("d1_context");
    let mut seed = manager.begin()?;
    seed.catalog_mut()?.create_table(
        TableDefinition {
            name: table.clone(),
            columns: vec![ColumnDefinition::new("i", DataType::Integer)],
            unique_keys: vec![],
        },
        false,
    )?;
    seed.commit()?;

    let interrupt = InterruptHandle::default();
    let context = QueryContext::new(interrupt.clone(), None, 64, 64)?;
    let mut local = manager.begin()?;
    local
        .storage_mut()?
        .insert(&table, vec![integers(&[2])], &context)?;
    let mut rival = manager.begin()?;
    rival
        .storage_mut()?
        .insert(&table, vec![integers(&[3])], &QueryContext::background())?;
    rival.commit()?;
    interrupt.interrupt();
    assert!(matches!(local.commit(), Err(Error::Interrupted)));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_relation_ddl_rejects_stale_append_across_drop_and_recreate() -> Result<()> {
    let db = Database::memory()?;
    let mut stale = db.connect();
    let mut ddl = db.connect();
    stale.execute(
        "CREATE TABLE d1_relation(i INTEGER); BEGIN; INSERT INTO d1_relation VALUES (1)",
    )?;
    ddl.execute("DROP TABLE d1_relation; CREATE TABLE d1_relation(i INTEGER)")?;
    assert!(matches!(stale.execute("COMMIT"), Err(Error::Conflict)));
    assert_eq!(
        ddl.query("SELECT * FROM d1_relation")?.rows,
        Vec::<Vec<Value>>::new()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_rename_claims_both_old_and_new_relation_names() -> Result<()> {
    let db = Database::memory()?;
    let mut stale = db.connect();
    let mut rename = db.connect();
    stale.execute("CREATE TABLE d1_rename(i INTEGER); BEGIN; INSERT INTO d1_rename VALUES (1)")?;
    rename.execute("ALTER TABLE d1_rename RENAME TO d1_renamed")?;
    assert!(matches!(stale.execute("COMMIT"), Err(Error::Conflict)));
    assert_eq!(
        rename.query("SELECT * FROM d1_renamed")?.rows,
        Vec::<Vec<Value>>::new()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_view_dependency_rejects_concurrent_source_relation_change() -> Result<()> {
    let db = Database::memory()?;
    let mut view_tx = db.connect();
    let mut ddl = db.connect();
    view_tx.execute("CREATE TABLE d1_dependency(i INTEGER); BEGIN; CREATE VIEW d1_dependency_view AS SELECT i FROM d1_dependency")?;
    ddl.execute("ALTER TABLE d1_dependency ADD COLUMN j INTEGER")?;
    assert!(matches!(view_tx.execute("COMMIT"), Err(Error::Conflict)));
    assert!(ddl.query("SELECT * FROM d1_dependency_view").is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn transactional_ddl_constraints_and_abandonment() -> Result<()> {
    let db = Database::memory()?;
    let mut c = db.connect();
    c.execute("CREATE TABLE t(i INTEGER PRIMARY KEY, v INTEGER NOT NULL DEFAULT 7); INSERT INTO t(i) VALUES (1)")?;
    assert_eq!(c.query("SELECT * FROM t")?.rows, vec![integers(&[1, 7])]);
    assert!(c.execute("INSERT INTO t VALUES (2,3),(1,4)").is_err());
    assert_eq!(
        c.query("SELECT count(*) FROM t")?.rows,
        vec![integers(&[1])]
    );
    c.execute("BEGIN; CREATE TABLE temporary_work(i INTEGER); INSERT INTO t VALUES (2,3)")?;
    assert!(c.execute("INSERT INTO t VALUES (2,4)").is_err());
    assert!(c.query("SELECT * FROM t").is_err());
    c.execute("ROLLBACK")?;
    assert!(c.query("SELECT * FROM temporary_work").is_err());
    assert_eq!(
        c.query("SELECT count(*) FROM t")?.rows,
        vec![integers(&[1])]
    );
    {
        let mut abandoned = db.connect();
        abandoned.execute("BEGIN; DELETE FROM t")?;
    }
    assert_eq!(
        c.query("SELECT count(*) FROM t")?.rows,
        vec![integers(&[1])]
    );
    // Matches test/api/test_api.cpp: the connection owns an open transaction
    // containing DDL, so destroying it must roll that DDL back as well.
    {
        let mut abandoned = db.connect();
        abandoned.execute("BEGIN; CREATE TABLE test(i INTEGER)")?;
    }
    c.execute("CREATE TABLE test(i INTEGER)")?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn prepared_statements_rebind_catalog_and_parameters() -> Result<()> {
    let mut c = Database::memory()?.connect();
    c.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (4)")?;
    let prepared = c.prepare("SELECT i + $1 AS value FROM t WHERE i < $2")?;
    assert_eq!(
        c.execute_prepared(&prepared, &integers(&[3, 5]))?.rows,
        vec![integers(&[7])]
    );
    c.execute("DROP TABLE t; CREATE TABLE t(x INTEGER)")?;
    assert!(c.execute_prepared(&prepared, &integers(&[3, 5])).is_err());
    assert!(c.prepare("SELECT 1; SELECT 2").is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn anonymous_parameters_keep_lexical_identity_across_rebinding_and_typed_execution() -> Result<()> {
    let mut c = Database::memory()?.connect();
    let prepared = c.prepare("SELECT ? AS x, v FROM (VALUES (?)) t(v) ORDER BY x")?;
    for values in [integers(&[11, 22]), integers(&[33, 44])] {
        assert_eq!(
            c.execute_prepared(&prepared, &values)?.rows,
            vec![values.clone()]
        );
    }
    let prepared = c.prepare("SELECT $3, ?, $1, ?, '?;?' AS literal /* ? */")?;
    assert_eq!(
        c.execute_prepared(&prepared, &integers(&[1, 2, 3, 4, 5]))?
            .rows,
        vec![vec![
            Value::Integer(3),
            Value::Integer(4),
            Value::Integer(1),
            Value::Integer(5),
            Value::Varchar("?;?".into())
        ]]
    );
    c.execute("CREATE TABLE typed(id UUID PRIMARY KEY, d DECIMAL(12,2), ts TIMESTAMP, b BLOB, nested STRUCT(d DECIMAL(12,2), ts TIMESTAMP[]))")?;
    let insert =
        c.prepare("INSERT INTO typed VALUES (?, ?, ?, ?, {'d': ?, 'ts': [?::TIMESTAMP, NULL]})")?;
    let values = [
        Value::Uuid(1),
        Value::Varchar("12.34".into()),
        Value::Temporal(duckdb_rust::common::TemporalValue::Timestamp(0)),
        Value::Blob(vec![0, 255]),
        Value::Varchar("99.50".into()),
        Value::Varchar("2000-01-01".into()),
    ];
    c.execute_prepared(&insert, &values)?;
    c.execute("BEGIN")?;
    let update = c.prepare("UPDATE typed SET d=? WHERE id=?")?;
    c.execute_prepared(&update, &[Value::Null, Value::Uuid(1)])?;
    c.execute("ROLLBACK")?;
    let query = c.prepare("SELECT d, b, nested.d FROM typed WHERE id=?")?;
    let rows = c.execute_prepared(&query, &[Value::Uuid(1)])?.rows;
    assert_eq!(rows[0][0].to_string(), "12.34");
    assert_eq!(rows[0][1], Value::Blob(vec![0, 255]));
    assert_eq!(rows[0][2].to_string(), "99.50");
    assert!(c.execute_prepared(&insert, &values[..5]).is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ctas_resolves_untyped_null_leaves_before_assignment_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("inferred-types.json");
    let open = || {
        DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()
    };
    {
        let mut c = open()?.connect();
        c.execute("CREATE TABLE t AS SELECT NULL AS n, [] AS empty, [NULL] AS child, {'a': NULL, 'd': 1.25::DECIMAL(12,2)} AS s")?;
        c.execute("INSERT INTO t VALUES (4,[5],[6],{'a': 7,'d': 2.50})")?;
    }
    let mut c = open()?.connect();
    let types = c.query("SELECT typeof(n), typeof(empty), typeof(child) FROM t LIMIT 1")?;
    assert_eq!(
        types.rows[0],
        vec![
            Value::Varchar("INTEGER".into()),
            Value::Varchar("INTEGER[]".into()),
            Value::Varchar("INTEGER[]".into()),
        ]
    );
    assert_eq!(
        c.query("SELECT s FROM t LIMIT 1")?.columns[0].data_type,
        duckdb_rust::common::NestedType::Struct(vec![
            ("a".into(), DataType::Integer),
            (
                "d".into(),
                DataType::Decimal {
                    width: 12,
                    scale: 2
                }
            ),
        ])
        .data_type(),
    );
    let rows = c
        .query("SELECT n, empty[1], child[1], s.a, s.d FROM t ORDER BY n")?
        .rows;
    assert_eq!(&rows[0][..4], integers(&[4, 5, 6, 7]));
    assert!(rows[1][..4].iter().all(Value::is_null));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn recursive_map_grammar_reaches_typed_binding_without_changing_dialect() -> Result<()> {
    use duckdb_rust::parser::{DuckDbParser, Parser};
    let mut c = Database::memory()?.connect();
    c.execute("CREATE TABLE t(m MAP(INTEGER, STRUCT(d DECIMAL(12,2), ts TIMESTAMP[])))")?;
    c.execute(
        "INSERT INTO t VALUES (map([1],[{'d': 1.25, 'ts': [TIMESTAMP 'epoch', NULL]}])), (NULL)",
    )?;
    let result = c.query("SELECT m FROM t WHERE m IS NOT NULL")?;
    let Value::Nested(map) = &result.rows[0][0] else {
        panic!("typed MAP value")
    };
    let duckdb_rust::common::NestedPayload::Map(entries) = &map.payload else {
        panic!("MAP entries")
    };
    let Value::Nested(record) = &entries[0].1 else {
        panic!("typed STRUCT child")
    };
    let duckdb_rust::common::NestedPayload::Struct(fields) = &record.payload else {
        panic!("STRUCT fields")
    };
    assert_eq!(entries[0].0, Value::Integer(1));
    assert_eq!(fields[0].to_string(), "1.25");
    assert!(
        DuckDbParser
            .parse("SELECT NULL::MAP(INTEGER, MAP(VARCHAR, INTEGER[2]))")
            .is_ok()
    );
    assert!(
        DuckDbParser
            .parse("SELECT NULL::TUPLE(INTEGER, MAP(VARCHAR, TIMESTAMP[]))")
            .is_ok()
    );
    assert!(DuckDbParser.parse("SELECT NULL::MAP(INTEGER)").is_err());
    assert!(matches!(
        c.query("SELECT NULL::ENUM()"),
        Err(Error::Bind(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn contextual_comparisons_carry_scalar_types_through_joins_membership_and_mutations() -> Result<()>
{
    use duckdb_rust::execution::expression_executor::{BatchedEvaluator, ScalarEvaluator};
    for batched in [false, true] {
        for batch in [1, 7] {
            let mut c = DatabaseBuilder::new()
                .batch_size(batch)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .build()?
                .connect();
            c.execute("CREATE TABLE t(i INTEGER,b BOOLEAN,u UUID,ts TIMESTAMP,e ENUM('2','1'),s VARCHAR); INSERT INTO t VALUES (1,true,'00000000-0000-0000-0000-000000000001',TIMESTAMP 'epoch','1','1'), (2,false,'00000000-0000-0000-0000-000000000002',TIMESTAMP '2000-01-01','2','2'), (NULL,NULL,NULL,NULL,NULL,NULL)")?;
            for (sql, count) in [
                (
                    "SELECT count(*) FROM t WHERE u='00000000-0000-0000-0000-000000000001'",
                    1,
                ),
                ("SELECT count(*) FROM t WHERE ts >= '1970-01-01'", 2),
                ("SELECT count(*) FROM t a JOIN t b ON a.i=b.s", 2),
                ("SELECT count(*) FROM t WHERE i IN ('1','3')", 1),
                ("SELECT count(*) FROM t WHERE i IN (SELECT s FROM t)", 2),
                ("SELECT count(*) FROM t WHERE e=1", 1),
                ("SELECT count(*) FROM t WHERE e<'2'", 1),
                ("SELECT count(*) FROM t WHERE b=i", 1),
            ] {
                assert_eq!(c.query(sql)?.rows[0][0], Value::Integer(count), "{sql}");
            }
            for sql in ["SELECT i<s FROM t", "SELECT e<1 FROM t"] {
                assert!(matches!(c.query(sql), Err(Error::Bind(_))), "{sql}");
            }
            assert!(matches!(
                c.query("SELECT u='invalid uuid' FROM t"),
                Err(Error::Conversion(_))
            ));
            c.execute(
                "BEGIN; UPDATE t SET i=8 WHERE u='00000000-0000-0000-0000-000000000001'; ROLLBACK",
            )?;
            assert_eq!(
                c.query("SELECT sum(i) FROM t")?.rows[0][0],
                Value::Integer(3)
            );
            let injected = c.query(
                "SELECT union_extract(('1'::ENUM('2','1'))::UNION(e ENUM('2','1'),n INTEGER),'e')",
            )?;
            assert_eq!(injected.rows[0][0].to_string(), "1");
            assert!(
                c.query("SELECT TRY_CAST(union_value(a := 1) AS ENUM('1','2'))")?
                    .rows[0][0]
                    .is_null()
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn scalar_literal_coercion_does_not_grant_implicit_varchar_column_casts() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(
        c.query("SELECT make_date('2024',2,29)")?.rows[0][0].to_string(),
        "2024-02-29"
    );
    assert!(matches!(
        c.query("SELECT make_date(y,2,29) FROM (VALUES ('2024')) t(y)"),
        Err(Error::Bind(_))
    ));
    assert!(matches!(
        c.query("SELECT make_date('bad',2,29)"),
        Err(Error::Conversion(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn values_ctes_set_operations_and_hidden_sort_keys() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("WITH a AS (SELECT range AS i FROM range(4)) SELECT i+1 AS n FROM a WHERE i > 0 ORDER BY i DESC LIMIT 2")?.rows, vec![integers(&[4]),integers(&[3])]);
    assert_eq!(
        c.query("SELECT 1 AS n UNION SELECT 1 UNION SELECT 2 ORDER BY n DESC")?
            .rows,
        vec![integers(&[2]), integers(&[1])]
    );
    assert_eq!(c.query("SELECT x, sum(y) AS s FROM (VALUES (1,2),(1,3),(2,4)) t(x,y) GROUP BY x HAVING sum(y)>4 ORDER BY s")?.rows, vec![integers(&[1,5])]);
    assert!(
        c.query("SELECT x, sum(y) FROM (VALUES (1,2)) t(x,y)")
            .is_err()
    );
    assert!(c.query("SELECT count(*) FROM range(2) GROUP BY 2").is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn vectors_own_values_and_validate_shape() -> Result<()> {
    let base = Arc::new(Vector::flat(
        DataType::Integer,
        vec![Value::Integer(1), Value::Null, Value::Integer(3)],
    )?);
    let selected = base.select(vec![2, 1, 2, 0])?;
    drop(base);
    assert_eq!(
        selected.values().collect::<Vec<_>>(),
        vec![
            Value::Integer(3),
            Value::Null,
            Value::Integer(3),
            Value::Integer(1)
        ]
    );
    let constant = Vector::constant(DataType::Integer, Value::Integer(9), 4)?;
    assert_eq!(
        DataChunk::new(vec![selected, constant], 4)?.rows().count(),
        4
    );
    assert!(
        DataChunk::new(
            vec![Vector::constant(DataType::Boolean, Value::Null, 3)?],
            4
        )
        .is_err()
    );
    assert!(
        Arc::new(Vector::constant(DataType::Boolean, Value::Null, 3)?)
            .select(vec![3])
            .is_err()
    );
    assert_eq!(DataChunk::new(vec![], 3)?.rows().count(), 3);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn persistence_contract(durability: Arc<dyn Durability>) -> Result<()> {
    let manager = SnapshotTransactions::new(durability)?;
    let mut tx = manager.begin()?;
    tx.catalog_mut()?.create_table(
        TableDefinition {
            name: TableName::main("items"),
            columns: vec![ColumnDefinition::new("i", DataType::BigInt)],
            unique_keys: vec![],
        },
        false,
    )?;
    tx.storage_mut()?.insert(
        &TableName::main("items"),
        vec![integers(&[42])],
        &QueryContext::background(),
    )?;
    tx.commit()?;
    let tx = manager.begin()?;
    assert_eq!(
        tx.storage().fetch(
            &TableName::main("items"),
            &[0, 1, 0],
            &QueryContext::background()
        )?,
        vec![Some(integers(&[42])), None, Some(integers(&[42]))]
    );
    assert_eq!(tx.catalog().tables()?.len(), 1);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn persistence_adapters_share_transaction_contract() -> Result<()> {
    persistence_contract(Arc::new(MemoryDurability))?;
    let directory = tempfile::tempdir()?;
    let formats: Vec<Arc<dyn duckdb_rust::storage::format::SnapshotFormat>> = vec![
        Arc::new(JsonSnapshotFormat),
        Arc::new(duckdb_rust::storage::duckdb::DuckDbFormat::default()),
    ];
    for format in formats {
        let path = directory.path().join(format.name());
        persistence_contract(Arc::new(FileCheckpoint::open(
            &path,
            OpenMode::ReadWrite,
            format.clone(),
        )?))?;
        let db = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadOnly,
                format.clone(),
            )?))
            .build()?;
        assert_eq!(
            db.connect().query("SELECT * FROM items")?.rows,
            vec![integers(&[42])]
        );
        assert!(FileCheckpoint::open(&path, OpenMode::ReadWrite, format).is_err());
    }
    Ok(())
}

struct FailingDurability {
    publications: AtomicUsize,
    uncertain: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Durability for FailingDurability {
    fn name(&self) -> &'static str {
        "fault-injection"
    }
    fn load(
        &self,
        types: Arc<duckdb_rust::common::type_registry::TypeRegistry>,
    ) -> Result<Snapshot> {
        Ok(Snapshot::new(types))
    }
    fn publish(
        &self,
        _: duckdb_rust::storage::log::Commit<'_>,
    ) -> Result<duckdb_rust::storage::checkpoint::PublishOutcome> {
        if self.publications.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(duckdb_rust::storage::checkpoint::PublishOutcome::Published);
        }
        if self.uncertain {
            Err(Error::CommitUnknown("injected failure".into()))
        } else {
            Err(Error::Io(std::io::Error::other("injected fsync failure")))
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn failed_commit_never_publishes_and_uncertain_commit_blocks_use() -> Result<()> {
    for uncertain in [false, true] {
        let db = DatabaseBuilder::new()
            .durability(Arc::new(FailingDurability {
                publications: AtomicUsize::new(0),
                uncertain,
            }))
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t(i INTEGER)")?;
        assert!(c.execute("INSERT INTO t VALUES (1)").is_err());
        if uncertain {
            assert!(matches!(
                c.query("SELECT * FROM t"),
                Err(Error::CommitUnknown(_))
            ));
        } else {
            assert!(c.query("SELECT * FROM t")?.rows.is_empty());
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Twice;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Twice {
    fn name(&self) -> &str {
        "twice"
    }
    fn return_type(
        &self,
        args: &[DataType],
        _types: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if args.len() != 1 || !args[0].is_integer() {
            return Err(Error::Bind("twice requires one integer".into()));
        }
        Ok(DataType::HugeInt)
    }
    fn evaluate(&self, args: &[Value], _: &QueryContext) -> Result<Value> {
        if args[0].is_null() {
            return Ok(Value::Null);
        }
        args[0]
            .as_i128()?
            .checked_mul(2)
            .map(Value::Integer)
            .ok_or_else(|| Error::Execution("twice overflow".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ordinary_function_registration_and_capability_rejection() -> Result<()> {
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(Twice))?;
    assert!(functions.register_scalar(Arc::new(Twice)).is_err());
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT twice(range) FROM range(3) ORDER BY 1")?
            .rows,
        vec![integers(&[0]), integers(&[2]), integers(&[4])]
    );
    let mut c = DatabaseBuilder::new()
        .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![Arc::new(
            HashJoin,
        )])))
        .build()?
        .connect();
    assert!(matches!(
        c.query("SELECT * FROM range(2) a JOIN range(2) b ON a.range < b.range"),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}

#[derive(Debug)]
struct CastToSeven;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for CastToSeven {
    fn name(&self) -> &'static str {
        "cast-to-seven"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target == DataType::Integer
            && spec.mode == CastMode::Explicit
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        Ok(Value::Integer(7))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn builder_cast_registry_snapshots_isolate_services_and_stored_expressions() -> Result<()> {
    let spec = CastSpec {
        source: DataType::Varchar,
        target: DataType::Integer,
        mode: CastMode::Explicit,
    };
    let mut selected = CastRegistry::builtins();
    selected.replace(spec.clone(), Arc::new(CastToSeven))?;
    let retained = selected.clone();
    let database = DatabaseBuilder::new().casts(selected).build()?;
    let mut connection = database.connect();
    assert_eq!(
        connection
            .query("SELECT CAST('not-an-integer' AS INTEGER)")?
            .rows,
        vec![integers(&[7])]
    );
    connection
        .execute("CREATE TABLE copied(v INTEGER DEFAULT CAST('not-an-integer' AS INTEGER))")?;
    connection.execute("INSERT INTO copied DEFAULT VALUES")?;
    assert_eq!(
        connection.query("SELECT v FROM copied")?.rows,
        vec![integers(&[7])]
    );

    let mut after_build = retained;
    after_build.replace(spec, Arc::new(duckdb_rust::common::cast::PrimitiveCast))?;
    assert_eq!(
        connection
            .query("SELECT CAST('not-an-integer' AS INTEGER)")?
            .rows,
        vec![integers(&[7])]
    );
    assert!(
        after_build
            .register_type(
                &DataType::Date,
                &duckdb_rust::common::type_registry::builtin_types()
            )
            .is_err()
    );
    connection.execute("INSERT INTO copied DEFAULT VALUES")?;
    assert_eq!(
        connection.query("SELECT v FROM copied ORDER BY v")?.rows,
        vec![integers(&[7]), integers(&[7])]
    );
    let mut ordinary = DatabaseBuilder::new().build()?.connect();
    assert!(
        ordinary
            .query("SELECT CAST('not-an-integer' AS INTEGER)")
            .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn timeouts_and_resource_limits_release_transactions() -> Result<()> {
    let mut c = DatabaseBuilder::new()
        .max_intermediate_rows(100)
        .build()?
        .connect();
    assert!(matches!(
        c.query("SELECT * FROM range(101)"),
        Err(Error::Resource(_))
    ));
    c.set_timeout(Some(Duration::ZERO));
    assert!(matches!(c.query("SELECT 1"), Err(Error::Interrupted)));
    c.set_timeout(None);
    assert_eq!(c.query("SELECT 1")?.rows, vec![integers(&[1])]);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn snapshot_api_mutations_are_atomic() -> Result<()> {
    let mut state = Snapshot::default();
    let name = TableName::main("t");
    state.create_table(
        TableDefinition {
            name: name.clone(),
            columns: vec![ColumnDefinition::new("i", DataType::Integer)],
            unique_keys: vec![duckdb_rust::catalog::UniqueKey {
                columns: vec![0],
                primary: false,
            }],
        },
        false,
    )?;
    state.insert(
        &name,
        vec![integers(&[1]), integers(&[2])],
        &QueryContext::background(),
    )?;
    let update = UpdateMetadata::for_table(&state.table(&name)?, vec![0])?;
    assert!(
        state
            .update(
                &name,
                &update,
                vec![(0, integers(&[2]))],
                &QueryContext::background()
            )
            .is_err()
    );
    assert_eq!(
        state.scan(&name, &QueryContext::background())?,
        vec![(0, integers(&[1])), (1, integers(&[2]))]
    );
    state.delete(&name, &[0], &QueryContext::background())?;
    state.insert(&name, vec![integers(&[3])], &QueryContext::background())?;
    assert_eq!(
        state.fetch(&name, &[0, 2], &QueryContext::background())?,
        vec![None, Some(integers(&[3]))]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn shared_plan_boundary_rejects_invalid_types_and_identities() -> Result<()> {
    use duckdb_rust::planner::{BoundExpr, BoundStatement, Field, LogicalPlan, PlanNode};
    let mut c = Database::memory()?.connect();
    c.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (1)")?;
    let input = LogicalPlan {
        schema: vec![Field::new("i", DataType::Integer)],
        node: PlanNode::Scan(TableBinding::unversioned(TableName::main("t"))),
    };
    assert!(
        c.execute_plan(BoundStatement::Insert {
            table: TableBinding::unversioned(TableName::main("t")),
            columns: vec![99],
            source: input.clone()
        })
        .is_err()
    );
    let invalid = LogicalPlan {
        schema: input.schema.clone(),
        node: PlanNode::Projection {
            input: Box::new(input.clone()),
            expressions: vec![BoundExpr::column(4, DataType::Integer)],
        },
    };
    assert!(c.execute_plan(BoundStatement::Query(invalid)).is_err());
    let wrong_type = LogicalPlan {
        schema: vec![Field::new("i", DataType::Double)],
        ..input.clone()
    };
    assert!(c.execute_plan(BoundStatement::Query(wrong_type)).is_err());
    let disguised_literal = LogicalPlan {
        schema: vec![Field::new("i", DataType::Integer)],
        node: PlanNode::Values(vec![vec![BoundExpr {
            kind: duckdb_rust::planner::ExprKind::Literal(Value::Varchar("1".into())),
            data_type: DataType::Integer,
        }]]),
    };
    assert!(
        c.execute_plan(BoundStatement::Query(disguised_literal))
            .is_err()
    );
    assert_eq!(
        c.execute_plan(BoundStatement::Query(input))?.rows,
        vec![integers(&[1])]
    );
    c.execute("INSERT INTO t DEFAULT VALUES")?;
    assert_eq!(
        c.query("SELECT count(*) FROM t")?.rows,
        vec![integers(&[2])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn format_adapters_preserve_values_types_and_catalog_identity() -> Result<()> {
    use duckdb_rust::{
        catalog::Catalog,
        storage::{duckdb::DuckDbFormat, format::SnapshotFormat},
    };
    let formats: Vec<Box<dyn SnapshotFormat>> = vec![
        Box::new(JsonSnapshotFormat),
        Box::new(DuckDbFormat::default()),
    ];
    let mut snapshot = Snapshot::default();
    let types = [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
        DataType::Double,
        DataType::Varchar,
        DataType::Boolean,
    ];
    let defaults = [
        Value::Integer(-128),
        Value::Integer(-32768),
        Value::Integer(i32::MIN.into()),
        Value::Integer(i64::MIN.into()),
        Value::Integer(i128::MIN),
        Value::Double(0.5),
        Value::Varchar("🦆\0".repeat(3000)),
        Value::Boolean(true),
    ];
    snapshot.create_schema("empty", false)?;
    for name in [TableName::new("a.b", "c"), TableName::new("a", "b.c")] {
        snapshot.create_schema(&name.schema, false)?;
        snapshot.create_table(
            TableDefinition {
                name: name.clone(),
                columns: types
                    .iter()
                    .enumerate()
                    .map(|(i, t)| ColumnDefinition {
                        default: Some(duckdb_rust::catalog::expression::StoredExpression::literal(
                            t.clone(),
                            defaults[i].clone(),
                        )),
                        ..ColumnDefinition::new(format!("c{i}"), t.clone())
                    })
                    .collect(),
                unique_keys: vec![],
            },
            false,
        )?;
        let mut rows = Vec::new();
        for float in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0, 0.0, 0.5] {
            rows.push(vec![
                Value::Integer(-128),
                Value::Integer(-32768),
                Value::Integer(i32::MIN.into()),
                Value::Integer(i64::MIN.into()),
                Value::Integer(i128::MIN),
                Value::Double(float),
                Value::Varchar("🦆\0".repeat(160000)),
                Value::Boolean(true),
            ]);
        }
        rows.push(vec![Value::Null; types.len()]);
        snapshot.insert(&name, rows, &QueryContext::background())?;
    }
    for format in formats {
        let bytes = format.encode(&snapshot)?;
        let decoded = format.decode(bytes, duckdb_rust::common::type_registry::builtin_types())?;
        assert_eq!(snapshot.tables()?, decoded.tables()?);
        assert_eq!(snapshot.schemas()?, decoded.schemas()?);
        for table in snapshot.tables()? {
            let before =
                serde_json::to_string(&snapshot.scan(&table.name, &QueryContext::background())?)
                    .unwrap();
            let after =
                serde_json::to_string(&decoded.scan(&table.name, &QueryContext::background())?)
                    .unwrap();
            assert_eq!(before, after, "{} table {}", format.name(), table.name);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn composition_rejects_unused_transaction_bundle_selections() -> Result<()> {
    let manager = Arc::new(SnapshotTransactions::new(Arc::new(MemoryDurability))?);
    assert!(matches!(
        DatabaseBuilder::new()
            .transactions(manager.clone())
            .durability(Arc::new(MemoryDurability))
            .build(),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        DatabaseBuilder::new()
            .transactions(manager)
            .indexes(Arc::new(duckdb_rust::execution::index::BTreeIndexFactory))
            .build(),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn private_checkpoint_detects_valid_json_with_changed_values() -> Result<()> {
    use duckdb_rust::storage::format::SnapshotFormat;
    let context = QueryContext::background();
    let mut snapshot = Snapshot::default();
    let name = TableName::main("t");
    snapshot.create_table(
        TableDefinition {
            name: name.clone(),
            columns: vec![ColumnDefinition::new("a", DataType::Integer)],
            unique_keys: vec![],
        },
        false,
    )?;
    snapshot.insert(&name, vec![integers(&[123456789])], &context)?;
    let mut encoded = JsonSnapshotFormat.encode(&snapshot)?;
    let position = encoded.windows(9).position(|w| w == b"123456789").unwrap();
    encoded[position] = b'9';
    assert!(matches!(
        JsonSnapshotFormat.decode(encoded, duckdb_rust::common::type_registry::builtin_types()),
        Err(Error::Corrupt(_))
    ));
    Ok(())
}

#[derive(Debug)]
struct VolatileDefault(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for VolatileDefault {
    fn name(&self) -> &str {
        "app.default_tick"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: false,
        }
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if !arguments.is_empty() {
            return Err(Error::Bind("default_tick accepts no arguments".into()));
        }
        Ok(DataType::Integer)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if !arguments.is_empty() {
            return Err(Error::Internal("bound default_tick arguments".into()));
        }
        Ok(Value::Integer(
            self.0.fetch_add(1, Ordering::SeqCst) as i128 + 1,
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sql_defaults_are_retained_and_evaluated_only_at_omitted_row_demand() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut functions = FunctionRegistry::builtins();
    functions.register_scalar(Arc::new(VolatileDefault(calls.clone())))?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();

    assert!(
        c.execute("CREATE TABLE missing(v INTEGER DEFAULT missing())")
            .is_err()
    );
    assert!(
        c.execute("CREATE TABLE wrong_arity(v INTEGER DEFAULT app.default_tick(1))")
            .is_err()
    );
    assert!(
        c.execute("CREATE TABLE wrong_target(v INTEGER DEFAULT [1])")
            .is_err()
    );
    c.execute("CREATE TABLE failing_cast(v INTEGER DEFAULT CAST('bad' AS INTEGER))")?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    c.execute("INSERT INTO failing_cast VALUES (7)")?;
    assert!(
        c.execute("INSERT INTO failing_cast DEFAULT VALUES")
            .is_err()
    );

    c.execute("CREATE TABLE t(a INTEGER DEFAULT app.default_tick(), b INTEGER DEFAULT 1+2)")?;
    c.execute("ALTER TABLE t ALTER COLUMN a SET DEFAULT app.default_tick()")?;
    assert!(
        c.execute("ALTER TABLE t ALTER COLUMN b SET DEFAULT missing()")
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    c.execute("INSERT INTO t DEFAULT VALUES; INSERT INTO t(a) VALUES (10),(20)")?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        c.query("SELECT * FROM t ORDER BY a")?.rows,
        vec![integers(&[1, 3]), integers(&[10, 3]), integers(&[20, 3])]
    );
    c.execute("INSERT INTO t(b) VALUES (30),(31)")?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    c.execute("ALTER TABLE t ALTER COLUMN b SET DEFAULT 1/0")?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    c.execute("INSERT INTO t VALUES (30, 40),(31, 41)")?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(c.execute("INSERT INTO t(a) VALUES (50),(51)").is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        c.query("SELECT count(*) FROM t")?.rows,
        vec![integers(&[7])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn add_default_demand_distinguishes_materialized_and_simple_physical_paths() -> Result<()> {
    let mut c = Database::memory()?.connect();
    c.execute(
        "CREATE TABLE live(i INTEGER); INSERT INTO live VALUES (1),(2); \
         ALTER TABLE live ADD COLUMN added INTEGER DEFAULT 1+2",
    )?;
    assert_eq!(
        c.query("SELECT * FROM live ORDER BY i")?.rows,
        vec![integers(&[1, 3]), integers(&[2, 3])]
    );

    c.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (1),(2); DELETE FROM t")?;
    assert_eq!(
        c.query("SELECT count(*) FROM t")?.rows,
        vec![integers(&[0])]
    );

    c.execute("ALTER TABLE t ADD COLUMN deferred INTEGER DEFAULT 1/0")?;
    assert!(
        c.execute("ALTER TABLE t ADD COLUMN failed INTEGER DEFAULT CAST('bad' AS INTEGER)")
            .is_err()
    );
    assert!(
        c.execute(
            "ALTER TABLE live ADD COLUMN required INTEGER NOT NULL DEFAULT CAST(NULL AS INTEGER)"
        )
        .is_err()
    );
    assert_eq!(c.query("SELECT * FROM live ORDER BY i")?.columns.len(), 2);
    c.execute("CHECKPOINT")?;
    c.execute("ALTER TABLE t ADD COLUMN simple INTEGER DEFAULT CAST('bad' AS INTEGER)")?;
    c.execute("INSERT INTO t VALUES (3, 30, 40)")?;
    assert!(c.execute("INSERT INTO t(i) VALUES (4),(5)").is_err());
    assert_eq!(
        c.query("SELECT * FROM t")?.rows,
        vec![integers(&[3, 30, 40])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn update_mode_controls_later_volatile_add_order() -> Result<()> {
    let open = || -> Result<(duckdb_rust::Connection, Arc<AtomicUsize>)> {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(VolatileDefault(calls.clone())))?;
        Ok((
            DatabaseBuilder::new()
                .functions(functions)
                .build()?
                .connect(),
            calls,
        ))
    };

    // An ordinary scalar update retains the original physical row order.
    let (mut regular, calls) = open()?;
    regular.execute(
        "CREATE TABLE t(id INTEGER, v INTEGER); \
         INSERT INTO t VALUES (1,10),(2,20),(3,30); \
         UPDATE t SET v=v+1 WHERE id=1; \
         ALTER TABLE t ADD COLUMN observed INTEGER DEFAULT app.default_tick()",
    )?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        regular.query("SELECT id,observed FROM t ORDER BY id")?.rows,
        vec![integers(&[1, 1]), integers(&[2, 2]), integers(&[3, 3])]
    );

    // Naming an indexed column relocates the updated row. The old slot is
    // skipped and its replacement is evaluated after untouched live slots.
    let (mut indexed, calls) = open()?;
    indexed.execute(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER); \
         INSERT INTO t VALUES (1,10),(2,20),(3,30); \
         UPDATE t SET id=10 WHERE id=1; \
         UPDATE t SET id=id+100 WHERE id IN (2,10)",
    )?;
    assert_eq!(
        indexed.query("SELECT id FROM t")?.rows,
        vec![integers(&[3]), integers(&[102]), integers(&[110])]
    );
    indexed.execute("ALTER TABLE t ADD COLUMN observed INTEGER DEFAULT app.default_tick()")?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        indexed.query("SELECT id,observed FROM t ORDER BY id")?.rows,
        vec![integers(&[3, 1]), integers(&[102, 2]), integers(&[110, 3])]
    );

    // LIST cannot use DuckDB's regular update path, so assigning it has the
    // same relocation order without inspecting whether values changed.
    let (mut nested, calls) = open()?;
    nested.execute(
        "CREATE TABLE t(id INTEGER, v INTEGER[]); \
         INSERT INTO t VALUES (1,[1]),(2,[2]),(3,[3]); \
         UPDATE t SET v=[9] WHERE id=1; \
         ALTER TABLE t ADD COLUMN observed INTEGER DEFAULT app.default_tick()",
    )?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        nested.query("SELECT id,observed FROM t ORDER BY id")?.rows,
        vec![integers(&[1, 3]), integers(&[2, 1]), integers(&[3, 2])]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_failed_commit_rolls_back_and_catalog_errors_preserve_active_writes() -> Result<()> {
    let database = Database::memory()?;
    let mut connection = database.connect();
    connection.execute("CREATE TABLE failed_owner(i INTEGER PRIMARY KEY)")?;
    connection.execute("INSERT INTO failed_owner VALUES (1)")?;
    connection.execute("BEGIN")?;
    connection.execute("INSERT INTO failed_owner VALUES (2)")?;
    assert!(matches!(
        connection.execute("INSERT INTO failed_owner VALUES (1)"),
        Err(Error::Constraint(_))
    ));
    assert!(matches!(
        connection.query("SELECT * FROM failed_owner"),
        Err(Error::Transaction(_))
    ));
    connection.execute("COMMIT")?;
    assert_eq!(
        connection
            .query("SELECT i FROM failed_owner ORDER BY i")?
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    connection.execute("BEGIN")?;
    connection.execute("INSERT INTO failed_owner VALUES (3)")?;
    assert!(
        connection
            .execute("CREATE TABLE failed_owner(i INTEGER)")
            .is_err()
    );
    connection.execute("INSERT INTO failed_owner VALUES (4)")?;
    connection.execute("COMMIT")?;
    assert_eq!(
        connection
            .query("SELECT i FROM failed_owner ORDER BY i")?
            .rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(3)],
            vec![Value::Integer(4)]
        ]
    );
    Ok(())
}

// Oracles: root target/batch4/d1-oracle/run{1,2}/report.json, both pinned engines.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_active_same_column_conflict_retains_failed_owner_until_end() -> Result<()> {
    for end in ["ROLLBACK", "COMMIT"] {
        let database = Database::memory()?;
        let mut a = database.connect();
        let mut b = database.connect();
        a.execute("CREATE TABLE t(i INTEGER PRIMARY KEY, a INTEGER, b INTEGER)")?;
        a.execute("INSERT INTO t VALUES (1,0,0),(2,0,0)")?;
        a.execute("BEGIN")?;
        b.execute("BEGIN")?;
        a.query("SELECT * FROM t ORDER BY i")?;
        b.query("SELECT * FROM t ORDER BY i")?;
        a.execute("UPDATE t SET a=11 WHERE i=1")?;
        assert!(matches!(
            a.execute("INSERT INTO t VALUES(1,9,9)"),
            Err(Error::Constraint(_))
        ));
        assert!(matches!(
            b.execute("UPDATE t SET a=22 WHERE i=1"),
            Err(Error::Conflict)
        ));
        a.execute(end)?;
        b.execute("ROLLBACK")?;
        b.execute("UPDATE t SET a=33 WHERE i=1")?;
        assert_eq!(
            b.query("SELECT * FROM t ORDER BY i")?.rows,
            vec![integers(&[1, 33, 0]), integers(&[2, 0, 0])]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_disjoint_column_replay_preserves_snapshot_expression_values() -> Result<()> {
    for reverse_commit in [false, true] {
        let database = Database::memory()?;
        let mut a = database.connect();
        let mut b = database.connect();
        a.execute("CREATE TABLE t(i INTEGER PRIMARY KEY, a INTEGER, b INTEGER)")?;
        a.execute("INSERT INTO t VALUES (1,0,0),(2,0,0)")?;
        a.execute("BEGIN")?;
        b.execute("BEGIN")?;
        a.query("SELECT * FROM t ORDER BY i")?;
        b.query("SELECT * FROM t ORDER BY i")?;
        a.execute("UPDATE t SET a=11 WHERE i=1")?;
        b.execute("UPDATE t SET b=a+7 WHERE i=1")?;
        if reverse_commit {
            b.execute("COMMIT")?;
            a.execute("COMMIT")?;
        } else {
            a.execute("COMMIT")?;
            b.execute("COMMIT")?;
        }
        assert_eq!(
            a.query("SELECT * FROM t ORDER BY i")?.rows,
            vec![integers(&[1, 11, 7]), integers(&[2, 0, 0])]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_regular_update_and_delete_can_commit_without_resurrection() -> Result<()> {
    for reverse_acquire in [false, true] {
        for reverse_commit in [false, true] {
            let database = Database::memory()?;
            let mut a = database.connect();
            let mut b = database.connect();
            a.execute("CREATE TABLE t(i INTEGER PRIMARY KEY, a INTEGER)")?;
            a.execute("INSERT INTO t VALUES (1,0),(2,0)")?;
            a.execute("BEGIN")?;
            b.execute("BEGIN")?;
            a.query("SELECT * FROM t ORDER BY i")?;
            b.query("SELECT * FROM t ORDER BY i")?;
            if reverse_acquire {
                b.execute("DELETE FROM t WHERE i=1")?;
                a.execute("UPDATE t SET a=11 WHERE i=1")?;
            } else {
                a.execute("UPDATE t SET a=11 WHERE i=1")?;
                b.execute("DELETE FROM t WHERE i=1")?;
            }
            if reverse_commit {
                b.execute("COMMIT")?;
                a.execute("COMMIT")?;
            } else {
                a.execute("COMMIT")?;
                b.execute("COMMIT")?;
            }
            assert_eq!(
                a.query("SELECT * FROM t ORDER BY i")?.rows,
                vec![integers(&[2, 0])]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn d1_stale_delete_conflict_phase_depends_on_index_maintenance() -> Result<()> {
    for keyed in [false, true] {
        let database = Database::memory()?;
        let mut a = database.connect();
        let mut b = database.connect();
        a.execute(if keyed {
            "CREATE TABLE t(i INTEGER PRIMARY KEY)"
        } else {
            "CREATE TABLE t(i INTEGER)"
        })?;
        a.execute("INSERT INTO t VALUES(1),(2)")?;
        a.execute("BEGIN")?;
        a.query("SELECT * FROM t ORDER BY i")?;
        b.execute("DROP TABLE t")?;
        if keyed {
            assert!(matches!(
                a.execute("DELETE FROM t WHERE i=1"),
                Err(Error::Conflict)
            ));
            a.execute("COMMIT")?;
        } else {
            a.execute("DELETE FROM t WHERE i=1")?;
            assert!(matches!(a.execute("COMMIT"), Err(Error::Conflict)));
        }
    }
    Ok(())
}
