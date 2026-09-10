use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    catalog::{CatalogMut, ColumnDefinition, TableDefinition, TableName},
    common::vector::{DataChunk, Vector},
    execution::{
        operator::join::{HashJoin, NestedLoopJoin},
        physical_plan::NativePhysicalPlanner,
    },
    function::{FunctionRegistry, ScalarFunction},
    optimizer::IdentityOptimizer,
    parallel::QueryContext,
    storage::{
        TableStorage, TableStorageMut,
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
    assert!(std::ptr::eq(
        projected.columns()[1].get(0).unwrap(),
        source.columns()[0].get(2).unwrap()
    ));
    assert!(std::ptr::eq(
        projected.columns()[1].get(1).unwrap(),
        projected.columns()[2].get(1).unwrap()
    ));
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
        selected.values().cloned().collect::<Vec<_>>(),
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
    fn publish(&self, _: duckdb_rust::storage::log::Commit<'_>) -> Result<()> {
        if self.publications.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(());
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
    assert!(
        state
            .update(
                &name,
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
        node: PlanNode::Scan(TableName::main("t")),
    };
    assert!(
        c.execute_plan(BoundStatement::Insert {
            table: TableName::main("t"),
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
                        default: defaults[i].clone(),
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
